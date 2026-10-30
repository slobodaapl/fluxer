// SPDX-License-Identifier: AGPL-3.0-or-later

//! The voice debug event sink window of fluxer_desktop/src/main/VoiceDebugEventSinkPopout.ts:
//! a native window showing the renderer's voice debug log and stats, kept on top, with the
//! bridge installed so its pin button reaches `popoutSetAlwaysOnTop`.
//!
//! Electron loaded the page as a `data:` URL. Here the shell serves the same page at a reserved
//! local app path, to the sink window only, so it runs on the app origin the IPC accepts. The
//! page gets a CSP Electron did not give it: the stats snapshot is renderer HTML set through
//! `innerHTML` in a page that holds the bridge, so only the page's own script may run.

mod entries;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::http::{Method, Request, Response, StatusCode, header};
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{
    AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

use self::entries::{Flush, Loading, SinkState, normalize_entries, normalize_stats_html};
use crate::bridge::{self, CapabilityManifest};
use crate::config::{ActiveWindowBehavior, DesktopConfig};
use crate::documents::DocumentRegistry;
use crate::events::BridgeEvents;
use crate::external;
use crate::local_app::index_html::random_csp_nonce;
use crate::local_app::url::{VOICE_DEBUG_EVENT_SINK_PATH, origin, parse_local_app_url};
use crate::window::show::focus_window;

/// `VOICE_DEBUG_EVENT_SINK_POPOUT_KEY`, the page's key for `popoutSetAlwaysOnTop`.
pub const POPOUT_KEY: &str = "fluxer-voice-debug-event-sink";
const LABEL: &str = "voice-debug-event-sink";
const TITLE: &str = "Fluxer | Voice Debug Event Sink";
const SIZE: (f64, f64) = (1000.0, 700.0);
const MINIMUM_SIZE: (f64, f64) = (420.0, 260.0);
const FLUSH_INTERVAL: Duration = Duration::from_millis(50);
const PAGE: &str = include_str!("page.html");
const PAGE_NONCE_PLACEHOLDER: &str = "{{CSP_NONCE}}";
const SET_ENTRIES_FUNCTION: &str = "__fluxerVoiceDebugEventSinkSetEntries";
const APPEND_ENTRIES_FUNCTION: &str = "__fluxerVoiceDebugEventSinkAppendEntries";
const SET_STATS_HTML_FUNCTION: &str = "__fluxerVoiceDebugEventSinkSetStatsHtml";

#[derive(Default)]
pub struct EventSink {
    state: Mutex<SinkState>,
    /// The sink window's page is loading (`isLoadingMainFrame`).
    loading: Mutex<bool>,
}

impl EventSink {
    fn state(&self) -> MutexGuard<'_, SinkState> {
        self.state.lock().expect("event sink state lock")
    }

    fn set_loading(&self, loading: bool) {
        *self.loading.lock().expect("event sink loading lock") = loading;
    }

    fn is_loading(&self) -> bool {
        *self.loading.lock().expect("event sink loading lock")
    }
}

/// The sink window, while it exists.
pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

fn page_url() -> String {
    format!("{}{VOICE_DEBUG_EVENT_SINK_PATH}", origin())
}

/// The sink window's navigation gate: its page and nothing else.
fn allows_navigation(url: &str) -> bool {
    url == page_url()
}

/// Serves the page at its reserved path, to the sink window only. `None` for other paths.
pub fn page_response(webview_label: &str, request: &Request<Vec<u8>>) -> Option<Response<Vec<u8>>> {
    let url = parse_local_app_url(&request.uri().to_string())?;
    if url.path() != VOICE_DEBUG_EVENT_SINK_PATH {
        return None;
    }
    let method = request.method();
    if webview_label != LABEL || (method != Method::GET && method != Method::HEAD) {
        tracing::warn!(webview_label, "Refused the voice debug event sink page");
        return Some(
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Vec::new())
                .expect("static response parts are valid"),
        );
    }
    let nonce = random_csp_nonce();
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'unsafe-inline'; \
         connect-src ipc: http://ipc.localhost"
    );
    let body = if method == Method::HEAD {
        Vec::new()
    } else {
        PAGE.replace(PAGE_NONCE_PLACEHOLDER, &nonce).into_bytes()
    };
    Some(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-store")
            .header(header::CONTENT_SECURITY_POLICY, csp)
            .body(body)
            .expect("static response parts are valid"),
    )
}

/// `runPopoutFunction`: calls a page function with `payload`, `<` escaped as Electron did.
fn page_call(function: &str, payload: &impl Serialize) -> String {
    let payload = serde_json::to_string(payload)
        .expect("event sink payloads serialise")
        .replace('<', "\\u003c");
    format!("typeof window.{function} === \"function\" && window.{function}({payload})")
}

fn run_page_call(window: &WebviewWindow, function: &str, payload: &impl Serialize) {
    if let Err(error) = window.eval(page_call(function, payload)) {
        tracing::warn!(%error, function, "Failed to update the voice debug event sink");
    }
}

/// `sendSnapshotToPopout`.
fn send_snapshot(window: &WebviewWindow, sink: &EventSink) {
    let (entries, stats_html) = sink.state().snapshot();
    run_page_call(window, SET_ENTRIES_FUNCTION, &entries);
    run_page_call(window, SET_STATS_HTML_FUNCTION, &stats_html);
}

fn schedule_flush(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FLUSH_INTERVAL).await;
        flush(&app);
    });
}

/// `flushPendingEntriesToPopout`.
fn flush(app: &AppHandle) {
    let sink = app.state::<EventSink>();
    let window = window(app);
    let loading = window.as_ref().map(|_| Loading(sink.is_loading()));
    let delivered = sink.state().flush(loading);
    match delivered {
        Flush::Idle => {}
        Flush::Reschedule => schedule_flush(app),
        Flush::Deliver(entries) => {
            if let Some(window) = window {
                run_page_call(&window, APPEND_ENTRIES_FUNCTION, &entries);
            }
        }
    }
}

/// `createEventSinkWindow`.
fn create_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let config = app.state::<Arc<DesktopConfig>>();
    // The page draws no titlebar; the window keeps its native frame.
    let active = ActiveWindowBehavior {
        use_native_title_bar: true,
        allow_transparency: false,
        ..config.active_window_behavior()
    };
    let capabilities = *app.state::<CapabilityManifest>();
    let url = url::Url::parse(&page_url()).expect("the event sink page URL is a valid URL");
    let opener = app.clone();
    let builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::CustomProtocol(url))
        .title(TITLE)
        .inner_size(SIZE.0, SIZE.1)
        .min_inner_size(MINIMUM_SIZE.0, MINIMUM_SIZE.1)
        .always_on_top(true)
        .initialization_script(bridge::initialization_script(capabilities, active))
        .on_navigation(|url| allows_navigation(url.as_str()))
        .on_new_window(move |url, _features| {
            if let Err(error) = external::open_external_url(&opener, url.as_str()) {
                tracing::warn!(%url, %error, "Failed to open external URL from the event sink");
            }
            NewWindowResponse::Deny
        })
        // Electron's title followed the page's `<title>` once it loaded.
        .on_document_title_changed(|window, title| {
            if let Err(error) = window.set_title(&title) {
                tracing::warn!(%error, "Failed to update the event sink window title");
            }
        })
        .on_page_load(|webview, payload| {
            let sink = webview.state::<EventSink>();
            let documents = webview.state::<Arc<DocumentRegistry>>();
            match payload.event() {
                PageLoadEvent::Started => {
                    documents.navigation_started(webview.label());
                    sink.set_loading(true);
                }
                // `did-finish-load`.
                PageLoadEvent::Finished => {
                    documents.page_loaded(webview.label());
                    sink.set_loading(false);
                    send_snapshot(&webview, &sink);
                }
            }
        });
    #[cfg(windows)]
    let builder = builder.use_https_scheme(true);
    app.state::<EventSink>().set_loading(true);
    let window = builder.build()?;
    #[cfg(target_os = "linux")]
    crate::linux_webview::configure(&window, active.smooth_scrolling)?;
    let observed = window.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Destroyed = event {
            observed
                .state::<Arc<DocumentRegistry>>()
                .destroyed(observed.label());
            observed.state::<BridgeEvents>().detach(observed.label());
        }
    });
    Ok(window)
}

/// `voice-debug-event-sink:open`.
/// Async: a window built from a synchronous command deadlocks WebView2.
#[tauri::command]
pub async fn voice_debug_event_sink_open(
    app: AppHandle,
    sink: State<'_, EventSink>,
    entries: Value,
) -> tauri::Result<()> {
    sink.state().replace(normalize_entries(&entries));
    let window = match window(&app) {
        Some(window) => window,
        None => create_window(&app)?,
    };
    focus_window(&window);
    if !sink.is_loading() {
        send_snapshot(&window, &sink);
    }
    Ok(())
}

/// `voice-debug-event-sink:append`.
#[tauri::command]
pub fn voice_debug_event_sink_append(app: AppHandle, sink: State<'_, EventSink>, entries: Value) {
    let entries = normalize_entries(&entries);
    let mut state = sink.state();
    state.append(&entries);
    if window(&app).is_some() && state.queue(entries) {
        schedule_flush(&app);
    }
}

/// `voice-debug-event-sink:set-stats-html`.
#[tauri::command]
pub fn voice_debug_event_sink_set_stats_html(
    app: AppHandle,
    sink: State<'_, EventSink>,
    html: Value,
) {
    let html = sink
        .state()
        .set_stats_html(normalize_stats_html(&html))
        .to_owned();
    if let Some(window) = window(&app)
        && !sink.is_loading()
    {
        run_page_call(&window, SET_STATS_HTML_FUNCTION, &html);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(url: &str) -> Request<Vec<u8>> {
        Request::builder().uri(url).body(Vec::new()).unwrap()
    }

    #[test]
    fn the_page_is_served_to_the_sink_window_only() {
        let page = page_url();
        let response = page_response(LABEL, &get(&page)).unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .to_owned();
        let body = String::from_utf8(response.into_body()).unwrap();
        let nonce = csp
            .split("'nonce-")
            .nth(1)
            .and_then(|rest| rest.split('\'').next())
            .unwrap();
        assert!(body.contains(&format!("<script nonce=\"{nonce}\">")));
        assert!(!body.contains(PAGE_NONCE_PLACEHOLDER));

        for label in ["main", "popout-1", ""] {
            let refused = page_response(label, &get(&page)).unwrap();
            assert_eq!(refused.status(), StatusCode::NOT_FOUND, "{label}");
            assert!(refused.body().is_empty());
        }
        let post = Request::builder()
            .method(Method::POST)
            .uri(&page)
            .body(Vec::new())
            .unwrap();
        assert_eq!(
            page_response(LABEL, &post).unwrap().status(),
            StatusCode::NOT_FOUND
        );
        // Other paths fall through to the renderer files.
        assert!(page_response(LABEL, &get(&format!("{}/", origin()))).is_none());
        assert!(page_response(LABEL, &get(&format!("{page}/x"))).is_none());
    }

    #[test]
    fn the_sink_window_navigates_to_its_page_only() {
        assert!(allows_navigation(&page_url()));
        assert!(!allows_navigation(&format!("{}/channels/@me", origin())));
        assert!(!allows_navigation("about:blank"));
        assert!(!allows_navigation("https://fluxer.app/"));
    }

    #[test]
    fn page_calls_cannot_close_the_script_element() {
        let call = page_call(
            SET_STATS_HTML_FUNCTION,
            &"</script><script>alert(1)</script>",
        );
        assert!(!call.contains('<'));
        assert_eq!(
            call,
            "typeof window.__fluxerVoiceDebugEventSinkSetStatsHtml === \"function\" && \
             window.__fluxerVoiceDebugEventSinkSetStatsHtml(\"\\u003c/script>\\u003cscript>alert(1)\\u003c/script>\")"
        );
    }
}
