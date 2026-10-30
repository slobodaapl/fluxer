// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri_plugin_opener::OpenerExt;

use crate::build_channel::BuildChannel;

const DEDUPE_WINDOW: Duration = Duration::from_millis(500);
const ALLOWED_SCHEMES: [&str; 5] = ["http", "https", "mailto", "tel", "appstream"];

static RECENT_OPENS: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn safe_external_url(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    let scheme = parsed.scheme();
    let allowed =
        ALLOWED_SCHEMES.contains(&scheme) || scheme == BuildChannel::current().app_protocol();
    allowed.then(|| parsed.to_string())
}

/// Same window as the Electron build: a repeat within 500 ms is dropped and extends the window.
fn admit(url: &str, now: Instant) -> bool {
    let mut guard = RECENT_OPENS.lock().expect("recent external opens lock");
    let recent = guard.get_or_insert_with(HashMap::new);
    recent.retain(|_, opened| now.duration_since(*opened) <= DEDUPE_WINDOW);
    let duplicate = recent
        .get(url)
        .is_some_and(|opened| now.duration_since(*opened) < DEDUPE_WINDOW);
    recent.insert(url.to_owned(), now);
    !duplicate
}

#[tauri::command]
pub fn open_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    open_external_url(&app, &url)
}

/// `openExternalDeduped`: opens an allowed URL outside the app, dropping a quick repeat.
pub fn open_external_url(app: &tauri::AppHandle, url: &str) -> Result<(), String> {
    let safe_url = safe_external_url(url).ok_or("External URL open request blocked")?;
    if !admit(&safe_url, Instant::now()) {
        return Ok(());
    }
    app.opener()
        .open_url(safe_url, None::<&str>)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_schemes_open() {
        assert_eq!(
            safe_external_url("https://fluxer.app/x").as_deref(),
            Some("https://fluxer.app/x")
        );
        assert!(safe_external_url("mailto:a@b.c").is_some());
        assert!(safe_external_url("file:///etc/passwd").is_none());
        assert!(safe_external_url("javascript:alert(1)").is_none());
        assert!(safe_external_url("not a url").is_none());
        let own = format!("{}://invite/x", BuildChannel::current().app_protocol());
        assert!(safe_external_url(&own).is_some());
    }

    #[test]
    fn repeats_inside_the_window_are_dropped() {
        let start = Instant::now();
        let url = "https://dedupe.test/unique";
        assert!(admit(url, start));
        assert!(!admit(url, start + Duration::from_millis(200)));
        assert!(!admit(url, start + Duration::from_millis(650)));
        assert!(admit(url, start + Duration::from_millis(1200)));
    }
}
