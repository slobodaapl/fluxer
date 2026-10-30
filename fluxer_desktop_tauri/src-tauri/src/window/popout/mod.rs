// SPDX-License-Identifier: AGPL-3.0-or-later

//! Voice popouts and the Quick CSS editor: the child windows the main renderer opens with
//! `window.open('about:blank', key, 'width=…,height=…')` and then writes from its own realm
//! (fluxer_app PopoutWindow.tsx). Ports the main window's `setWindowOpenHandler` and
//! `did-create-window`, `getPopoutWindowOptions`, `trackVoicePopoutWindow`,
//! `trackQuickCssEditorWindow` and `attachChildWindowGuards` of fluxer_desktop/src/main/Window.ts.
//!
//! The child is a Tauri window handed back as `NewWindowResponse::Create`, so the engine binds
//! it to the opener's `window.open` call and the opener gets a scriptable same-origin proxy.
//! Tauri reports no frame name, so the child is built without the per-kind options and named
//! from its own `window.name` once the engine has set it: on Linux at WebKit's `ready-to-show`,
//! which also carries the size the features asked for (Tauri reports none there), and the
//! window stays hidden until then, so a child Electron would have denied never shows. Windows
//! and macOS report the size, show the window at once and are named when its blank page
//! finishes loading; neither path is verified.
//!
//! Electron forwarded maximize changes to each popout's webContents; nothing in a popout
//! document listens (the renderer subscribes from the main document only), so none are sent.

mod classify;
pub mod commands;
mod registry;

use std::sync::{Arc, Mutex, MutexGuard};

use tauri::webview::{NewWindowFeatures, NewWindowResponse, PageLoadEvent};
use tauri::{
    AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent, Wry,
};

use self::classify::{OpenRequest, PopoutKind};
use self::registry::{Naming, Registry};
use crate::bridge::{self, CapabilityManifest};
use crate::config::{ActiveWindowBehavior, DesktopConfig};
use crate::documents::DocumentRegistry;
use crate::events::BridgeEvents;
use crate::external;
use crate::local_app::LocalApp;
use crate::navigation;

const BLANK_DOCUMENT: &str = "about:blank";
/// Read back from the child once it is bound to its `window.open` call.
const NAME_PROBE: &str = "[window.name, document.title]";

#[derive(Default)]
pub struct Popouts(Mutex<Registry>);

impl Popouts {
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.0.lock().expect("popout registry lock")
    }

    /// The live window `popoutSetAlwaysOnTop` and `popoutFocus` act on for `key`.
    fn window(&self, app: &AppHandle, key: &str) -> Option<WebviewWindow> {
        let label = self.lock().label_for(key)?.to_owned();
        app.get_webview_window(&label)
    }
}

/// The main window's `setWindowOpenHandler`.
pub fn open(
    app: &AppHandle,
    url: &url::Url,
    features: NewWindowFeatures,
) -> NewWindowResponse<Wry> {
    let popouts = app.state::<Popouts>();
    let label = {
        let mut registry = popouts.lock();
        match classify::request(url.as_str(), registry.open_children()) {
            OpenRequest::OpenExternally => {
                if let Err(error) = external::open_external_url(app, url.as_str()) {
                    tracing::warn!(%url, %error, "Failed to open external URL from window-open");
                }
                return NewWindowResponse::Deny;
            }
            OpenRequest::AtCapacity => {
                tracing::warn!("Denied voice popout window: capacity reached");
                return NewWindowResponse::Deny;
            }
            OpenRequest::Create => registry.add_unnamed(),
        }
    };
    match build(app, &label, features) {
        Ok(window) => NewWindowResponse::Create { window },
        Err(error) => {
            tracing::warn!(%error, "Failed to create a popout window");
            popouts.lock().forget(&label);
            NewWindowResponse::Deny
        }
    }
}

fn build(
    app: &AppHandle,
    label: &str,
    features: NewWindowFeatures,
) -> tauri::Result<WebviewWindow> {
    let config = app.state::<Arc<DesktopConfig>>();
    // `getPopoutWindowOptions`: the active title bar, never transparent.
    let active = ActiveWindowBehavior {
        allow_transparency: false,
        ..config.active_window_behavior()
    };
    let capabilities = *app.state::<CapabilityManifest>();
    let opener = app.clone();
    let blank = url::Url::parse(BLANK_DOCUMENT).expect("about:blank is a valid URL");
    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::External(blank))
        .window_features(features)
        .title(
            config
                .launch_product_name()
                .unwrap_or_else(|| app.package_info().name.clone()),
        )
        .visible(cfg!(not(target_os = "linux")))
        .initialization_script(bridge::initialization_script(capabilities, active))
        .on_navigation(|url| navigation::allows_child_navigation(url.as_str()))
        // The child's own `setWindowOpenHandler`: everything opens outside.
        .on_new_window(move |url, _features| {
            if let Err(error) = external::open_external_url(&opener, url.as_str()) {
                tracing::warn!(%url, %error, "Failed to open external URL from a popout window-open");
            }
            NewWindowResponse::Deny
        })
        .on_document_title_changed(|window, title| {
            if let Err(error) = window.set_title(&title) {
                tracing::warn!(%error, "Failed to update the popout window title");
            }
        })
        .on_page_load(|webview, payload| {
            let documents = webview.state::<Arc<DocumentRegistry>>();
            match payload.event() {
                PageLoadEvent::Started => documents.navigation_started(webview.label()),
                PageLoadEvent::Finished => {
                    documents.page_loaded(webview.label());
                    #[cfg(not(target_os = "linux"))]
                    probe_name(&webview);
                }
            }
        });
    let builder = super::chrome::apply(builder, active, config.preboot_theme().as_deref());
    #[cfg(windows)]
    let builder = builder.use_https_scheme(true);
    let window = builder.build()?;
    app.state::<Arc<LocalApp>>()
        .authorized_webviews()
        .authorize(label);
    observe_window_events(&window);
    #[cfg(target_os = "linux")]
    {
        crate::linux_webview::configure(&window, active.smooth_scrolling)?;
        linux::attach(&window)?;
    }
    Ok(window)
}

fn observe_window_events(window: &WebviewWindow) {
    let observed = window.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Destroyed = event {
            let label = observed.label();
            observed.state::<Popouts>().lock().forget(label);
            observed
                .state::<Arc<LocalApp>>()
                .authorized_webviews()
                .revoke(label);
            observed.state::<Arc<DocumentRegistry>>().destroyed(label);
            observed.state::<BridgeEvents>().detach(label);
        }
    });
}

/// Reads an unnamed child's name and title back and names it.
#[cfg(not(target_os = "linux"))]
fn probe_name(window: &WebviewWindow) {
    if !window.state::<Popouts>().lock().is_unnamed(window.label()) {
        return;
    }
    let child = window.clone();
    if let Err(error) = window.eval_with_callback(NAME_PROBE, move |probe| {
        named(&child, &probe, None);
    }) {
        tracing::warn!(%error, "Failed to read the popout window name");
    }
}

/// `did-create-window` for a child whose `window.name` and `document.title` read `probe` (JSON).
/// `size` is the size the window features asked for, where the platform did not apply it.
fn named(window: &WebviewWindow, probe: &str, size: Option<(f64, f64)>) {
    let kind = serde_json::from_str::<(String, String)>(probe)
        .ok()
        .and_then(|(name, title)| Some((classify::classify(&name)?, title)));
    let Some((kind, document_title)) = kind else {
        // `openExternalDeduped('about:blank')` and deny: the blank page opens nowhere.
        tracing::warn!("Closing a window-open child that is not a popout");
        close(window);
        return;
    };
    let replaced = match window.state::<Popouts>().lock().name(window.label(), &kind) {
        Naming::Gone => return,
        Naming::Named { replaced } => replaced,
    };
    if let Some(replaced) = replaced.and_then(|label| window.get_webview_window(&label)) {
        close(&replaced);
    }
    if let Err(error) = apply_options(window, &kind, &document_title, size) {
        tracing::warn!(%error, "Failed to apply the popout window options");
    }
}

fn apply_options(
    window: &WebviewWindow,
    kind: &PopoutKind,
    document_title: &str,
    size: Option<(f64, f64)>,
) -> tauri::Result<()> {
    let (min_width, min_height) = kind.minimum_size();
    window.set_min_size(Some(tauri::LogicalSize::new(min_width, min_height)))?;
    if let Some((width, height)) = size {
        window.set_size(tauri::LogicalSize::new(width, height))?;
    }
    // The document's own title replaced Electron's initial one as soon as it set it.
    if let Some(title) = kind.initial_title()
        && document_title.is_empty()
    {
        window.set_title(title)?;
    }
    if kind.starts_always_on_top() {
        window.set_always_on_top(true)?;
    }
    if !window.is_visible()? {
        super::show::focus_window(window);
    }
    Ok(())
}

fn close(window: &WebviewWindow) {
    if let Err(error) = window.close() {
        tracing::warn!(%error, label = window.label(), "Failed to close a popout window");
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use gtk::glib;
    use gtk::prelude::WidgetExt;
    use javascriptcore::ValueExt;
    use tauri::{Manager, WebviewWindow};
    use webkit2gtk::{WebViewExt, WindowPropertiesExt};

    use super::{NAME_PROBE, close, named};

    /// WebKit binds the child to its `window.open` call, names it and fills in its window
    /// properties before `ready-to-show`.
    pub(super) fn attach(window: &WebviewWindow) -> tauri::Result<()> {
        let child = window.clone();
        window.with_webview(move |webview| {
            let view = webview.inner();
            let ready = child.clone();
            view.connect_ready_to_show(move |view| {
                let size = view
                    .window_properties()
                    .map(|properties| properties.geometry())
                    .filter(|geometry| geometry.width() > 0 && geometry.height() > 0)
                    .map(|geometry| (f64::from(geometry.width()), f64::from(geometry.height())));
                let probed = ready.clone();
                // Tauri's `eval_with_callback` holds scripts until a load commits, which never
                // happens in a child that keeps its initial blank document. The shell supports
                // WebKitGTK 2.38, which lacks the `evaluate_javascript` that deprecates this.
                #[allow(deprecated)]
                view.run_javascript(NAME_PROBE, None::<&gtk::gio::Cancellable>, move |result| {
                    let probe = result
                        .ok()
                        .and_then(|result| result.js_value())
                        .and_then(|value| value.to_json(0))
                        .map(|json| json.to_string())
                        .unwrap_or_default();
                    named(&probed, &probe, size);
                });
            });
            // wry answers a script `window.close()` by destroying the webview widget alone, which
            // also disconnects any later `close` handler; the window goes once the widget has.
            // A native close destroys the widget too, after the window is gone.
            let app = child.app_handle().clone();
            let label = child.label().to_owned();
            view.connect_destroy(move |_| {
                let app = app.clone();
                let label = label.clone();
                glib::idle_add_local_once(move || {
                    if let Some(window) = app.get_webview_window(&label) {
                        close(&window);
                    }
                });
            });
        })
    }
}
