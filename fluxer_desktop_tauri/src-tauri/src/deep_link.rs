// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deep links, the port of fluxer_desktop/src/main/DeepLinks.ts. A link of the build channel's
//! scheme ([`BuildChannel::app_protocol`]) reaches the renderer as a path with its query and
//! fragment (`fluxer://invite/abc` → `/invite/abc`). Links come from this launch's argv, a
//! second instance's argv, and macOS `open-url` (`RunEvent::Opened`).
//!
//! Timing is Electron's. The launch's own link waits in a single slot until `getInitialDeepLink`
//! takes it. A later link goes to the main window as `deep-link`, which also shows it; like
//! `webContents.send`, the event is lost when the renderer has not subscribed yet (the renderer
//! awaits `getInitialDeepLink` before it calls `onDeepLink`). Before the main window exists a
//! link replaces the slot instead. The same link again within 1.5 s is dropped.
//!
//! A link that names an instance (`api`, `host`, `instance` and the like, as its host or as a
//! query key) is refused: links resolve against the foreground account only.
//!
//! Not ported: making this app the scheme's handler (the Electron install keeps it until the
//! cutover, so links arrive only when something passes them on the command line or macOS opens
//! one), `app.addRecentDocument`, holding links during an
//! update takeover (there is no updater), and the browser handoff return sink (there is no
//! browser handoff yet, so a handoff return link only shows the window).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, State};
use url::Url;

use crate::build_channel::BuildChannel;
use crate::events::BridgeEvents;
use crate::window::{self, MAIN_WINDOW_LABEL};

const DEEP_LINK_EVENT: &str = "deep-link";
/// `DUPLICATE_URL_SUPPRESS_MS`.
const DUPLICATE_SUPPRESS: Duration = Duration::from_millis(1500);
const URL_ARG: &str = "--url";
const HANDOFF_RETURN_HOST: &str = "handoff";
/// `DEEP_LINK_RENDERER_PAYLOAD_BLOCKLIST`.
const PAYLOAD_BLOCKLIST: [char; 9] = ['"', '\'', '<', '>', '\\', '|', '\t', '\r', '\n'];
/// `INSTANCE_DESIGNATING_KEYS`, compared after [`designation_key`].
const INSTANCE_DESIGNATING_KEYS: [&str; 10] = [
    "api",
    "apibase",
    "apiendpoint",
    "domain",
    "endpoint",
    "gateway",
    "host",
    "instance",
    "instancekey",
    "origin",
];

/// `designationKey`: lower case with everything but `a-z0-9` removed.
fn designation_key(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

fn is_designating(name: &str) -> bool {
    INSTANCE_DESIGNATING_KEYS.contains(&designation_key(name).as_str())
}

/// `findInstanceDesignation`: the host, then the query keys.
fn find_instance_designation(url: &Url) -> Option<String> {
    let host = url.host_str().unwrap_or_default();
    if is_designating(host) {
        return Some(host.to_owned());
    }
    url.query_pairs()
        .map(|(name, _)| name)
        .find(|name| is_designating(name))
        .map(|name| name.into_owned())
}

/// `findRawInstanceDesignation`: each name matched by `/[?&]([^=&#]+)=/g` in turn.
fn find_raw_instance_designation(raw: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(offset) = raw[from..].find(['?', '&']) {
        let start = from + offset + 1;
        let end = raw[start..]
            .find(['=', '&', '#'])
            .map_or(raw.len(), |length| start + length);
        if end > start && raw[end..].starts_with('=') {
            if is_designating(&raw[start..end]) {
                return Some(&raw[start..end]);
            }
            from = end + 1;
        } else {
            from = start;
        }
    }
    None
}

fn reject_instance_designation(designation: &str) -> Option<String> {
    tracing::warn!(
        "Rejected a deep link that tries to designate an instance through \"{designation}\" \
         because deep links resolve against the foreground account only"
    );
    None
}

/// `isAppProtocolUrl`.
fn is_app_protocol_url(value: &str, scheme: &str) -> bool {
    // JavaScript string lengths count UTF-16 units; the scheme is ASCII.
    if value.encode_utf16().count() <= scheme.len() + 1 {
        return false;
    }
    match Url::parse(value) {
        Ok(url) => url.scheme() == scheme,
        Err(_) => value
            .to_lowercase()
            .strip_prefix(scheme)
            .is_some_and(|rest| rest.starts_with(':')),
    }
}

/// `extractDeepLinkFromArgv`: after `--url` (and its `--` separator, when there is one) first,
/// then anywhere.
pub fn from_args<'a>(args: &'a [String], scheme: &str) -> Option<&'a str> {
    let is_link = |arg: &&'a String| is_app_protocol_url(arg, scheme);
    if let Some(flag) = args.iter().position(|arg| arg == URL_ARG) {
        let rest = &args[flag + 1..];
        let links = match rest.iter().position(|arg| arg == "--") {
            Some(separator) => &rest[separator + 1..],
            None => rest,
        };
        if let Some(link) = links.iter().find(is_link) {
            return Some(link);
        }
    }
    args.iter().find(is_link).map(String::as_str)
}

/// `parseHandoffReturnLink`, as a test.
fn is_handoff_return_link(raw: &str, scheme: &str) -> bool {
    Url::parse(raw).is_ok_and(|url| {
        url.scheme() == scheme
            && url
                .host_str()
                .is_some_and(|host| host.eq_ignore_ascii_case(HANDOFF_RETURN_HOST))
    })
}

/// `normalizeDeepLinkForRenderer`: the path the renderer routes on, or `None` for a link it must
/// not see.
fn normalize_for_renderer(raw: &str, scheme: &str) -> Option<String> {
    let blocked = |payload: &str| payload.contains(PAYLOAD_BLOCKLIST);
    let Ok(url) = Url::parse(raw) else {
        if let Some(designation) = find_raw_instance_designation(raw) {
            return reject_instance_designation(designation);
        }
        return (is_app_protocol_url(raw, scheme) && !blocked(raw)).then(|| raw.to_owned());
    };
    if url.scheme() != scheme {
        return None;
    }
    if let Some(designation) = find_instance_designation(&url) {
        return reject_instance_designation(&designation);
    }
    let host = url.host_str().unwrap_or_default();
    let path = match url.path() {
        pathname if !host.is_empty() && host != "-" => format!("/{host}{pathname}"),
        "" => "/".to_owned(),
        pathname => pathname.to_owned(),
    };
    // `URL.search` and `URL.hash` are empty for an empty query or fragment.
    let search = url.query().filter(|query| !query.is_empty());
    let hash = url.fragment().filter(|fragment| !fragment.is_empty());
    let payload = format!(
        "{}{path}{}{}",
        if path.starts_with('/') { "" } else { "/" },
        search.map(|query| format!("?{query}")).unwrap_or_default(),
        hash.map(|fragment| format!("#{fragment}"))
            .unwrap_or_default(),
    );
    (!blocked(&payload)).then_some(payload)
}

#[derive(Default)]
struct Slots {
    /// `initialDeepLink`.
    initial: Option<String>,
    /// `lastDispatchedUrl` and `lastDispatchedAt`.
    last: Option<(String, Instant)>,
}

pub struct DeepLinks {
    slots: Mutex<Slots>,
}

impl DeepLinks {
    /// `registerInitialDeepLinkHandler`: this launch's own link, unless it is a handoff return
    /// link. It also primes the duplicate check.
    pub fn from_launch(args: &[String]) -> Self {
        Self::from_launch_at(args, BuildChannel::current().app_protocol(), Instant::now())
    }

    fn from_launch_at(args: &[String], scheme: &str, now: Instant) -> Self {
        let links = Self {
            slots: Mutex::new(Slots::default()),
        };
        if let Some(raw) = from_args(args, scheme)
            && !is_handoff_return_link(raw, scheme)
            && let Some(normalized) = normalize_for_renderer(raw, scheme)
        {
            links.suppresses_duplicate(&normalized, now);
            links.lock().initial = Some(normalized);
        }
        links
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Slots> {
        self.slots.lock().expect("deep link slots lock")
    }

    /// `shouldSuppressAsDuplicate`: true for the last link again within 1.5 s; otherwise `url`
    /// becomes the last link.
    fn suppresses_duplicate(&self, url: &str, now: Instant) -> bool {
        let mut slots = self.lock();
        if let Some((last, at)) = &slots.last
            && last == url
            && now.duration_since(*at) < DUPLICATE_SUPPRESS
        {
            return true;
        }
        slots.last = Some((url.to_owned(), now));
        false
    }

    fn take_initial(&self) -> Option<String> {
        self.lock().initial.take()
    }

    fn set_initial(&self, url: String) {
        self.lock().initial = Some(url);
    }
}

/// `dispatchDeepLink`, for a second instance's link and macOS `open-url`.
pub fn dispatch(app: &AppHandle, raw: &str) {
    let scheme = BuildChannel::current().app_protocol();
    if is_handoff_return_link(raw, scheme) {
        tracing::info!("Received a browser handoff return link while no handoff is waiting");
        window::show_main_window(app);
        return;
    }
    let Some(normalized) = normalize_for_renderer(raw, scheme) else {
        return;
    };
    let links = app.state::<DeepLinks>();
    if links.suppresses_duplicate(&normalized, Instant::now()) {
        return;
    }
    if app.get_webview_window(MAIN_WINDOW_LABEL).is_some() {
        app.state::<BridgeEvents>()
            .emit(MAIN_WINDOW_LABEL, DEEP_LINK_EVENT, &normalized);
        window::show_main_window(app);
    } else {
        links.set_initial(normalized);
    }
}

/// The `open-url` listener of ModuleBootHandoff.ts: links opened once the app is quitting are
/// ignored. Not verified on macOS.
#[cfg(target_os = "macos")]
pub fn opened(app: &AppHandle, urls: &[Url]) {
    if app
        .state::<crate::window::policy::MainWindowPolicy>()
        .is_quitting()
    {
        tracing::info!("Ignoring an opened link because the app is quitting");
        return;
    }
    for url in urls {
        dispatch(app, url.as_str());
    }
}

/// `get-initial-deep-link`: the launch's link once, then `null`.
#[tauri::command]
pub fn get_initial_deep_link(links: State<'_, DeepLinks>) -> Option<String> {
    links.take_initial()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEME: &str = "fluxer";

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn normalize(raw: &str) -> Option<String> {
        normalize_for_renderer(raw, SCHEME)
    }

    #[test]
    fn links_become_renderer_paths() {
        for (raw, expected) in [
            ("fluxer://invite/abc", "/invite/abc"),
            ("fluxer://invite", "/invite"),
            ("FLUXER://invite/abc", "/invite/abc"),
            ("fluxer:///channels/1/2", "/channels/1/2"),
            ("fluxer://-/channels/1", "/channels/1"),
            ("fluxer://-", "/"),
            ("fluxer:invite/abc", "/invite/abc"),
            ("fluxer://Settings/user", "/Settings/user"),
            ("fluxer://gift/a%20b?x=1#top", "/gift/a%20b?x=1#top"),
            ("fluxer://users/1:80/x", "/users/1:80/x"),
            ("fluxer://user@invite/abc", "/invite/abc"),
            ("fluxer://invite/abc?#", "/invite/abc"),
            // The query encode set leaves no blocked character in a parsed link's query.
            (
                "fluxer://invite/abc?q=\"<x>\"",
                "/invite/abc?q=%22%3Cx%3E%22",
            ),
        ] {
            assert_eq!(normalize(raw).as_deref(), Some(expected), "{raw}");
        }
    }

    #[test]
    fn other_schemes_and_blocked_payloads_are_refused() {
        for raw in [
            "https://fluxer.app/invite/abc",
            "fluxer-development://invite/abc",
            // A non-special path keeps its backslash and apostrophe.
            "fluxer://invite/a\\b",
            "fluxer://invite/it's",
            "fluxer://invite/a|b",
        ] {
            assert_eq!(normalize(raw), None, "{raw}");
        }
    }

    #[test]
    fn links_that_designate_an_instance_are_refused() {
        for raw in [
            "fluxer://api/x",
            "fluxer://API-Base/x",
            "fluxer://instance_key",
            "fluxer://invite/abc?host=evil.example",
            "fluxer://invite/abc?a=1&Api%20Endpoint=x",
            "fluxer://invite/abc?gate+way=x",
        ] {
            assert_eq!(normalize(raw), None, "{raw}");
        }
        assert_eq!(
            normalize("fluxer://invite/abc?hostname=x").as_deref(),
            Some("/invite/abc?hostname=x")
        );
    }

    #[test]
    fn unparseable_links_pass_raw_unless_blocked_or_designating() {
        let raw = "fluxer://[bad/x";
        assert!(Url::parse(raw).is_err());
        assert_eq!(normalize(raw).as_deref(), Some(raw));
        assert_eq!(normalize("fluxer://[bad/x?a=1&origin=x"), None);
        assert_eq!(normalize("fluxer://[bad/x??api=1"), None);
        assert_eq!(normalize("fluxer://[bad/<x>"), None);
        assert_eq!(normalize("other://[bad/x"), None);
    }

    #[test]
    fn raw_designations_follow_the_global_match_order() {
        assert_eq!(find_raw_instance_designation("x?a=1&b&api=2"), Some("api"));
        assert_eq!(
            find_raw_instance_designation("x?a#b=c&origin=1"),
            Some("origin")
        );
        assert_eq!(find_raw_instance_designation("x?origin"), None);
        assert_eq!(find_raw_instance_designation("x?=api&ok=1"), None);
    }

    #[test]
    fn protocol_urls_need_more_than_the_scheme() {
        assert!(!is_app_protocol_url("fluxer:", SCHEME));
        assert!(is_app_protocol_url("fluxer:x", SCHEME));
        assert!(is_app_protocol_url("Fluxer://[bad", SCHEME));
        assert!(!is_app_protocol_url("fluxerx://a", SCHEME));
        assert!(!is_app_protocol_url("/home/user/fluxer://a", SCHEME));
    }

    #[test]
    fn argv_links_prefer_those_after_the_url_flag() {
        let link = |argv: &[&str]| from_args(&args(argv), SCHEME).map(str::to_owned);
        assert_eq!(
            link(&["app", "fluxer://a", "--url", "--", "fluxer://b"]).as_deref(),
            Some("fluxer://b")
        );
        assert_eq!(
            link(&["app", "--url", "fluxer://a", "--", "x"]).as_deref(),
            Some("fluxer://a"),
            "a separator with no link after it falls back to the whole argv"
        );
        assert_eq!(
            link(&["app", "--url", "fluxer://c"]).as_deref(),
            Some("fluxer://c")
        );
        assert_eq!(
            link(&["app", "--fluxer-task=new-dm", "fluxer://d", "fluxer://e"]).as_deref(),
            Some("fluxer://d")
        );
        assert_eq!(link(&["app", "--url", "--", "https://x"]), None);
    }

    #[test]
    fn handoff_return_links_are_recognised_by_host() {
        assert!(is_handoff_return_link("fluxer://handoff?code=1", SCHEME));
        assert!(is_handoff_return_link("FLUXER://HandOff/x", SCHEME));
        assert!(!is_handoff_return_link("fluxer://invite/handoff", SCHEME));
        assert!(!is_handoff_return_link("https://handoff/x", SCHEME));
    }

    #[test]
    fn the_launch_link_is_taken_once_and_primes_the_duplicate_check() {
        let now = Instant::now();
        let links = DeepLinks::from_launch_at(&args(&["app", "fluxer://invite/abc"]), SCHEME, now);
        assert_eq!(links.take_initial().as_deref(), Some("/invite/abc"));
        assert_eq!(links.take_initial(), None);
        assert!(links.suppresses_duplicate("/invite/abc", now + Duration::from_millis(1499)));
    }

    #[test]
    fn handoff_and_refused_launch_links_leave_the_slot_empty() {
        let now = Instant::now();
        for argv in [
            &["app", "fluxer://handoff?x=1"][..],
            &["app", "fluxer://api/x"],
            &["app"],
        ] {
            let links = DeepLinks::from_launch_at(&args(argv), SCHEME, now);
            assert_eq!(links.take_initial(), None, "{argv:?}");
        }
    }

    #[test]
    fn duplicates_are_dropped_within_the_window_only() {
        let links = DeepLinks::from_launch_at(&[], SCHEME, Instant::now());
        let start = Instant::now();
        assert!(!links.suppresses_duplicate("/a", start));
        assert!(links.suppresses_duplicate("/a", start + Duration::from_millis(1000)));
        assert!(!links.suppresses_duplicate("/b", start + Duration::from_millis(1100)));
        assert!(!links.suppresses_duplicate("/a", start + Duration::from_millis(1200)));
        // A suppressed repeat does not extend the window.
        assert!(links.suppresses_duplicate("/a", start + Duration::from_millis(2600)));
        assert!(!links.suppresses_duplicate("/a", start + Duration::from_millis(2700)));
    }
}
