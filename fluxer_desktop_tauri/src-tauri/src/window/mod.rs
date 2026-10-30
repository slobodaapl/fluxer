// SPDX-License-Identifier: AGPL-3.0-or-later

//! The main window: `createWindow` of fluxer_desktop/src/main/Window.ts and the listeners it
//! installs. The window is built hidden at its saved or default bounds, revealed by the reveal
//! gate, keeps its bounds in `window-state.json`, and hides instead of closing or minimizing
//! where the tray (or macOS) keeps the app reachable.

mod chrome;
pub mod commands;
#[cfg(windows)]
mod live_resize;
pub mod policy;
pub mod popout;
#[cfg(target_os = "linux")]
mod renderer;
mod reveal;
pub mod session;
pub mod show;
mod state;

use std::sync::Arc;
use std::time::Instant;

use tauri::webview::PageLoadEvent;
use tauri::{
    CloseRequestApi, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

use self::policy::MainWindowPolicy;
use self::session::{MainWindowSession, MainWindowSlot};
pub use self::show::show_main_window;
use crate::bridge;
use crate::config::DesktopConfig;
use crate::documents::DocumentRegistry;
use crate::events::BridgeEvents;
use crate::navigation;
use crate::spellcheck::{Broadcast, Spellcheck};

pub const MAIN_WINDOW_LABEL: &str = "main";
/// `MIN_WINDOW_WIDTH`/`MIN_WINDOW_HEIGHT` of common/Constants.ts.
const MINIMUM_SIZE: (f64, f64) = (800.0, 600.0);
const MAXIMIZE_CHANGE_EVENT: &str = "window-maximize-change";

/// `clearSavedWindowBounds`, for `--fluxer-reset-window-state` and the window behavior setting.
pub fn clear_saved_state(app: &tauri::AppHandle) {
    state::clear(app);
}

/// `createWindow`. `start_hidden` is a start-minimized launch: the window stays hidden until
/// something shows it.
pub fn create_main_window(
    app: &tauri::AppHandle,
    capabilities: bridge::CapabilityManifest,
    start_hidden: bool,
) -> tauri::Result<WebviewWindow> {
    let started_at = Instant::now();
    let config = app.state::<Arc<DesktopConfig>>();
    let url = url::Url::parse(&config.landing_url()).expect("the landing URL is a valid URL");
    let active = config.active_window_behavior();
    let saved = state::load(
        app,
        &crate::config::launch::process_args(),
        &state::displays(app),
    );
    let bounds = saved
        .map(|saved| saved.bounds)
        .or_else(|| state::primary_work_area(app).map(|area| state::default_bounds(&area)));
    let opener = app.clone();
    let builder =
        WebviewWindowBuilder::new(app, MAIN_WINDOW_LABEL, WebviewUrl::CustomProtocol(url))
            .title(
                config
                    .launch_product_name()
                    .unwrap_or_else(|| app.package_info().name.clone()),
            )
            .min_inner_size(MINIMUM_SIZE.0, MINIMUM_SIZE.1)
            .visible(false)
            .initialization_script(bridge::initialization_script(capabilities, active))
            .on_navigation(|url| navigation::allows_navigation(url.as_str()))
            // `setWindowOpenHandler`: voice popouts and the Quick CSS editor, else the URL opens
            // outside.
            .on_new_window(move |url, features| popout::open(&opener, &url, features))
            // Electron's window title followed `page-title-updated`, empty titles included.
            .on_document_title_changed(|window, title| {
                if let Err(error) = window.set_title(&title) {
                    tracing::warn!(%error, "Failed to update the window title");
                }
            })
            .on_page_load(|webview, payload| {
                let documents = webview.state::<Arc<DocumentRegistry>>();
                match payload.event() {
                    PageLoadEvent::Started => {
                        documents.navigation_started(webview.label());
                        crate::streaming_priority::reset(webview.app_handle());
                        if !navigation::is_main_frame_document(payload.url().as_str()) {
                            return_to_landing(&webview, payload.url());
                        }
                    }
                    PageLoadEvent::Finished => {
                        documents.page_loaded(webview.label());
                        crate::tray::page_loaded(webview.app_handle());
                        crate::jump_list::page_loaded(webview.app_handle());
                        if let Some(session) = webview.state::<MainWindowSlot>().current() {
                            #[cfg(target_os = "linux")]
                            session.renderer.page_loaded();
                            reveal::ready_to_show(&webview, &session);
                        }
                    }
                }
            });
    let builder = match bounds {
        Some(bounds) => builder
            .inner_size(bounds.width, bounds.height)
            .position(bounds.x, bounds.y),
        None => builder
            .inner_size(state::DEFAULT_SIZE.0, state::DEFAULT_SIZE.1)
            .center(),
    };
    let builder = chrome::apply(builder, active, config.preboot_theme().as_deref());
    #[cfg(windows)]
    let builder = builder.use_https_scheme(true);
    let bounds = bounds.unwrap_or(state::Bounds {
        x: 0.0,
        y: 0.0,
        width: state::DEFAULT_SIZE.0,
        height: state::DEFAULT_SIZE.1,
    });
    let session = Arc::new(MainWindowSession::new(
        started_at,
        start_hidden,
        bounds,
        saved.is_some_and(|saved| saved.is_maximized),
    ));
    // The page-load handler finds the session in the slot, so it is there before the build.
    app.state::<MainWindowSlot>().set(session.clone());
    let window = builder.build()?;
    // Electron's width and height were the outer size; the builder's are the inner size.
    #[cfg(not(target_os = "linux"))]
    state::apply_size(&window, bounds.width, bounds.height)?;
    #[cfg(target_os = "linux")]
    {
        crate::linux_webview::configure(&window, active.smooth_scrolling)?;
        renderer::observe(&window, &session)?;
    }
    #[cfg(windows)]
    live_resize::install(&window, session.live_resize.clone())?;
    // `registerSpellcheck` applied the launch state to a new window without broadcasting.
    app.state::<Arc<Spellcheck>>().schedule_apply(
        app.clone(),
        MAIN_WINDOW_LABEL.to_owned(),
        Broadcast {
            state: false,
            resolved: false,
        },
    );
    observe_window_events(&window, &session);
    reveal::start(&window, &session);
    Ok(window)
}

/// The navigation gate lets frame targets through without knowing the frame, so a main frame
/// that lands on one is sent back to the app.
fn return_to_landing(webview: &WebviewWindow, url: &url::Url) {
    tracing::warn!(%url, "the main frame left the local app; returning to the landing page");
    let landing = webview.state::<Arc<DesktopConfig>>().landing_url();
    let landing = url::Url::parse(&landing).expect("the landing URL is a valid URL");
    if let Err(error) = webview.navigate(landing) {
        tracing::warn!(%error, "Failed to return the main frame to the landing page");
    }
}

fn observe_window_events(window: &WebviewWindow, session: &Arc<MainWindowSession>) {
    let observed = window.clone();
    let session = session.clone();
    session.maximized_changed(window.is_maximized().unwrap_or(false));
    window.on_window_event(move |event| match event {
        WindowEvent::Destroyed => {
            observed
                .state::<Arc<DocumentRegistry>>()
                .destroyed(observed.label());
            session.reveal_gate().dispose();
            observed.state::<MainWindowSlot>().clear(&session);
        }
        WindowEvent::CloseRequested { api, .. } => on_close_requested(&observed, &session, api),
        WindowEvent::Moved(_) => session.state.geometry_changed(&observed),
        WindowEvent::Resized(_) => {
            session.state.geometry_changed(&observed);
            on_resized(&observed, &session);
        }
        #[cfg(windows)]
        WindowEvent::Focused(false) => session.live_resize.end(&observed),
        _ => {}
    });
}

/// The `maximize`, `unmaximize` and `minimize` listeners. Tauri reports none of these, so they
/// are read off each resize.
fn on_resized(window: &WebviewWindow, session: &Arc<MainWindowSession>) {
    #[cfg(target_os = "macos")]
    show::finish_fullscreen_hide(window, session);
    if let Ok(maximized) = window.is_maximized()
        && let Some(maximized) = session.maximized_changed(maximized)
    {
        if maximized {
            session.state.maximized(window);
        } else {
            session.state.unmaximized(window);
        }
        window
            .state::<BridgeEvents>()
            .emit(window.label(), MAXIMIZE_CHANGE_EVENT, maximized);
    }
    let Ok(minimized) = window.is_minimized() else {
        return;
    };
    let Some(minimized) = session.minimized_changed(minimized) else {
        return;
    };
    // The `minimize` and `restore` listeners refresh the tray menu.
    let policy = window.state::<MainWindowPolicy>();
    if minimized
        && !policy.is_quitting()
        && policy.hides_on_minimize(&window.state::<Arc<DesktopConfig>>())
        && let Err(error) = window.hide()
    {
        tracing::warn!(%error, "Failed to hide the minimized main window");
    }
    crate::tray::refresh_menu(window.app_handle());
}

/// The main window's `close` listener: the bounds are saved, and unless the app is quitting a
/// close that the tray (or macOS) survives hides the window instead.
fn on_close_requested(
    window: &WebviewWindow,
    session: &Arc<MainWindowSession>,
    api: &CloseRequestApi,
) {
    session.state.cancel_pending_save();
    #[cfg(windows)]
    session.live_resize.end(window);
    session.state.save(window);
    let policy = window.state::<MainWindowPolicy>();
    if policy.is_quitting() || !policy.hides_on_close(&window.state::<Arc<DesktopConfig>>()) {
        return;
    }
    api.prevent_close();
    if cfg!(target_os = "macos") {
        tracing::info!("Window close hid the app. Use Quit to terminate the process");
    } else {
        tracing::info!("Window close hid the app to the tray. Use Quit to terminate the process");
    }
    show::hide(window, session);
}
