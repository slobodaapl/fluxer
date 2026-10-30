// SPDX-License-Identifier: AGPL-3.0-or-later

//! `--fluxer-log-renderer-console`: the main window's console messages in this process's
//! output, the `console-message` listener of fluxer_desktop/src/main/Window.ts.
//!
//! - Linux: WebKitGTK writes them to stdout itself (`CONSOLE LOG ...` lines, not through the
//!   log), the setting a debug build always has on.
//! - Windows: the DevTools protocol's `Runtime.consoleAPICalled` and `Runtime.exceptionThrown`
//!   events, logged at the console call's level. Not verified on Windows.
//! - macOS: WKWebView has no public console hook, so the flag only logs that it is unsupported.

use tauri::WebviewWindow;

/// Starts forwarding, once, after the main window is built.
#[cfg_attr(target_os = "macos", allow(unused_variables))]
pub fn forward(window: &WebviewWindow) {
    #[cfg(target_os = "linux")]
    let started = window.with_webview(|webview| {
        use webkit2gtk::{SettingsExt, WebViewExt};
        if let Some(settings) = webview.inner().settings() {
            settings.set_enable_write_console_messages_to_stdout(true);
        }
    });
    #[cfg(windows)]
    let started = window.with_webview(|webview| {
        // SAFETY: the controller is the live webview's; see `cdp::listen`.
        if let Err(error) = unsafe { cdp::listen(&webview.controller()) } {
            tracing::warn!(%error, "Failed to forward the renderer console");
        }
    });
    #[cfg(target_os = "macos")]
    let started: tauri::Result<()> = {
        tracing::warn!("--fluxer-log-renderer-console is not supported on macOS");
        Ok(())
    };
    if let Err(error) = started {
        tracing::warn!(%error, "Failed to forward the renderer console");
    }
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
enum Level {
    Error,
    Warning,
    Info,
    Debug,
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
struct ConsoleMessage {
    level: Level,
    text: String,
    source: Option<String>,
    /// 1-based, as Electron reported it.
    line: Option<u64>,
}

/// A `RemoteObject` as text: strings as they are, other values as JSON, the rest by their
/// description (objects, functions) or type (`undefined`).
#[cfg(any(windows, test))]
fn remote_object_text(object: &serde_json::Value) -> String {
    use serde_json::Value;
    match (object.get("value"), object.get("unserializableValue")) {
        (Some(Value::String(text)), _) => text.clone(),
        (Some(value), _) => value.to_string(),
        (None, Some(Value::String(text))) => text.clone(),
        _ => object
            .get("description")
            .or_else(|| object.get("type"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

/// The location of a protocol event: its `url` and 0-based `lineNumber`.
#[cfg(any(windows, test))]
fn location(frame: Option<&serde_json::Value>) -> (Option<String>, Option<u64>) {
    let Some(frame) = frame else {
        return (None, None);
    };
    let source = frame
        .get("url")
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.is_empty())
        .map(str::to_owned);
    let line = frame
        .get("lineNumber")
        .and_then(serde_json::Value::as_u64)
        .map(|line| line + 1);
    (source, line)
}

/// `Runtime.consoleAPICalled` parameters as Electron's `console-message` would have had them.
#[cfg(any(windows, test))]
fn console_api_message(params: &serde_json::Value) -> Option<ConsoleMessage> {
    let level = match params.get("type")?.as_str()? {
        "error" | "assert" => Level::Error,
        "warning" => Level::Warning,
        "debug" => Level::Debug,
        _ => Level::Info,
    };
    let text = params
        .get("args")?
        .as_array()?
        .iter()
        .map(remote_object_text)
        .collect::<Vec<_>>()
        .join(" ");
    let (source, line) = location(params.pointer("/stackTrace/callFrames/0"));
    Some(ConsoleMessage {
        level,
        text,
        source,
        line,
    })
}

/// `Runtime.exceptionThrown` parameters: an uncaught error, with its stack when there is one.
#[cfg(any(windows, test))]
fn exception_message(params: &serde_json::Value) -> Option<ConsoleMessage> {
    let details = params.get("exceptionDetails")?;
    let text = details
        .pointer("/exception/description")
        .or_else(|| details.get("text"))?
        .as_str()?
        .to_owned();
    let (source, line) = location(Some(details));
    Some(ConsoleMessage {
        level: Level::Error,
        text,
        source,
        line,
    })
}

#[cfg(windows)]
impl ConsoleMessage {
    fn log(&self) {
        let (text, source, line) = (&self.text, self.source.as_deref(), self.line);
        match self.level {
            Level::Error => tracing::error!(source, line, "Renderer console: {text}"),
            Level::Warning => tracing::warn!(source, line, "Renderer console: {text}"),
            Level::Info => tracing::info!(source, line, "Renderer console: {text}"),
            Level::Debug => tracing::debug!(source, line, "Renderer console: {text}"),
        }
    }
}

#[cfg(windows)]
mod cdp {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2, ICoreWebView2CallDevToolsProtocolMethodCompletedHandler,
        ICoreWebView2Controller,
    };
    use webview2_com::{DevToolsProtocolEventReceivedEventHandler, take_pwstr};
    use windows::core::{HSTRING, PCWSTR, PWSTR, Result, w};

    use super::{ConsoleMessage, console_api_message, exception_message};

    /// Logs every `event` the webview's DevTools protocol reports, read by `parse`.
    unsafe fn subscribe(
        core: &ICoreWebView2,
        event: &str,
        parse: fn(&serde_json::Value) -> Option<ConsoleMessage>,
    ) -> Result<()> {
        let event = HSTRING::from(event);
        let mut token = 0i64;
        // SAFETY: COM calls on the live webview; the handler copies the JSON out before it
        // returns.
        unsafe {
            core.GetDevToolsProtocolEventReceiver(PCWSTR(event.as_ptr()))?
                .add_DevToolsProtocolEventReceived(
                    &DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else {
                            return Ok(());
                        };
                        let mut json = PWSTR::null();
                        args.ParameterObjectAsJson(&mut json)?;
                        if let Some(message) = serde_json::from_str(&take_pwstr(json))
                            .ok()
                            .and_then(|params| parse(&params))
                        {
                            message.log();
                        }
                        Ok(())
                    })),
                    &mut token,
                )
        }
    }

    /// Subscribes to console calls and uncaught exceptions, then turns the Runtime domain on.
    pub unsafe fn listen(controller: &ICoreWebView2Controller) -> Result<()> {
        // SAFETY: see `subscribe`.
        unsafe {
            let core = controller.CoreWebView2()?;
            subscribe(&core, "Runtime.consoleAPICalled", console_api_message)?;
            subscribe(&core, "Runtime.exceptionThrown", exception_message)?;
            core.CallDevToolsProtocolMethod(
                w!("Runtime.enable"),
                w!("{}"),
                None::<&ICoreWebView2CallDevToolsProtocolMethodCompletedHandler>,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn console_calls_keep_their_level_text_and_first_frame() {
        let message = console_api_message(&json!({
            "type": "warning",
            "args": [
                {"type": "string", "value": "count"},
                {"type": "number", "value": 3},
                {"type": "number", "unserializableValue": "NaN", "description": "NaN"},
                {"type": "object", "className": "Object", "description": "Object"},
                {"type": "undefined"},
            ],
            "stackTrace": {"callFrames": [
                {"url": "https://app.local/main.js", "lineNumber": 41, "columnNumber": 2},
                {"url": "https://app.local/other.js", "lineNumber": 1},
            ]},
        }))
        .unwrap();
        assert_eq!(
            message,
            ConsoleMessage {
                level: Level::Warning,
                text: "count 3 NaN Object undefined".to_owned(),
                source: Some("https://app.local/main.js".to_owned()),
                line: Some(42),
            }
        );
    }

    #[test]
    fn console_types_map_to_electrons_levels() {
        for (kind, level) in [
            ("error", Level::Error),
            ("assert", Level::Error),
            ("warning", Level::Warning),
            ("debug", Level::Debug),
            ("log", Level::Info),
            ("info", Level::Info),
            ("table", Level::Info),
        ] {
            let message = console_api_message(&json!({"type": kind, "args": []})).unwrap();
            assert_eq!(message.level, level, "{kind}");
            assert_eq!(message.source, None);
        }
        assert_eq!(console_api_message(&json!({"args": []})), None);
    }

    #[test]
    fn uncaught_exceptions_prefer_the_error_description() {
        let message = exception_message(&json!({"exceptionDetails": {
            "text": "Uncaught",
            "url": "https://app.local/a.js",
            "lineNumber": 0,
            "exception": {"type": "object", "description": "TypeError: x is not a function\n    at a.js:1"},
        }}))
        .unwrap();
        assert_eq!(message.level, Level::Error);
        assert_eq!(
            message.text,
            "TypeError: x is not a function\n    at a.js:1"
        );
        assert_eq!(message.line, Some(1));
        let thrown_string =
            exception_message(&json!({"exceptionDetails": {"text": "Uncaught boom"}}));
        assert_eq!(
            thrown_string.map(|message| message.text).as_deref(),
            Some("Uncaught boom")
        );
    }
}
