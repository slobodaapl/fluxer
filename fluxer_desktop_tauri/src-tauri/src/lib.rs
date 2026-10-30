// SPDX-License-Identifier: AGPL-3.0-or-later

mod app_metrics;
#[cfg(target_os = "linux")]
mod appearance;
mod atomic_file;
mod autostart;
mod bridge;
mod build_channel;
mod clipboard;
mod config;
mod deep_link;
mod desktop_info;
mod dialog;
#[cfg(any(target_os = "macos", test))]
mod dock_menu;
mod documents;
mod downloads;
mod error;
mod event_sink;
mod events;
mod external;
mod gateway;
mod gpu_info;
mod i18n;
mod idle;
mod instance;
#[cfg(test)]
mod ipc_acl_tests;
mod js;
mod jump_list;
#[cfg(target_os = "linux")]
mod launcher_entry;
#[cfg(target_os = "linux")]
mod linux_webview;
mod local_app;
mod menu;
mod navigation;
mod net;
mod notifications;
mod png_image;
mod renderer_console;
mod runtime;
mod single_instance;
mod spellcheck;
mod storage;
mod streaming_priority;
mod taskbar_progress;
mod theme_files;
mod tray;
mod unread_badge;
mod voice_background;
mod window;

use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

use crate::config::DesktopConfig;
use crate::documents::DocumentRegistry;
use crate::events::BridgeEvents;
use crate::local_app::LocalApp;

const RENDERER_DIR_ENV: &str = "FLUXER_RENDERER_DIR";
const RENDERER_RESOURCE_DIR: &str = "renderer";
const LOG_FILTER_ENV: &str = "FLUXER_LOG";

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env(LOG_FILTER_ENV).unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        // First, so a second launch hands over its argv before anything else starts.
        .plugin(single_instance::plugin())
        .plugin(tauri_plugin_opener::init())
        .register_asynchronous_uri_scheme_protocol(local_app::SCHEME, |ctx, request, responder| {
            let local_app = ctx.app_handle().state::<Arc<LocalApp>>().inner().clone();
            let webview_label = ctx.webview_label().to_owned();
            tauri::async_runtime::spawn(async move {
                let mut response = local_app.handle(&webview_label, request).await;
                // A proxied body counts against the response buffer budget until the webview
                // has it.
                let held = local_app::proxy::take_held_buffer(&mut response);
                responder.respond(response);
                drop(held);
            });
        })
        .manage(BridgeEvents::default())
        .manage(window::policy::MainWindowPolicy::default())
        .manage(window::session::MainWindowSlot::default())
        .manage(Arc::new(DocumentRegistry::default()))
        .manage(tray::DesktopTray::default())
        .manage(window::popout::Popouts::default())
        .manage(event_sink::EventSink::default())
        .manage(Arc::new(streaming_priority::StreamingPriority::default()))
        .manage(Arc::new(spellcheck::Spellcheck::new(
            &config::launch::process_args(),
        )))
        .manage(Arc::new(idle::SystemIdle::default()))
        .manage(Arc::new(app_metrics::AppMetrics::default()))
        .manage(deep_link::DeepLinks::from_launch(
            &config::launch::process_args(),
        ))
        .manage(jump_list::JumpList::default())
        .manage(Arc::new(theme_files::watch::ThemeLinkedFileWatches::default()))
        .invoke_handler(tauri::generate_handler![
            events::bridge_attach,
            documents::register_document,
            desktop_info::get_desktop_info,
            external::open_external,
            window::commands::window_minimize,
            window::commands::window_toggle_maximize,
            window::commands::window_close,
            window::commands::window_is_maximized,
            window::commands::window_start_dragging,
            window::commands::window_first_content_painted,
            window::commands::window_flash,
            window::commands::window_stop_flash,
            window::commands::set_zoom_factor,
            config::commands::desktop_window_behavior_get,
            config::commands::desktop_window_behavior_set,
            config::commands::desktop_window_behavior_pending_restart,
            config::commands::desktop_app_relaunch,
            config::commands::desktop_troubleshooting_get,
            config::commands::desktop_troubleshooting_set_disable_hardware_acceleration,
            config::commands::desktop_troubleshooting_set_disable_module_updates,
            config::commands::desktop_preboot_theme_report,
            config::commands::desktop_instance_product_name_report,
            config::commands::desktop_last_route_report,
            storage::commands::desktop_accounts_get_all,
            storage::commands::desktop_accounts_get,
            storage::commands::desktop_accounts_upsert,
            storage::commands::desktop_accounts_compare_and_swap,
            storage::commands::desktop_accounts_delete,
            storage::commands::desktop_accounts_import,
            storage::commands::desktop_accounts_prune,
            storage::commands::desktop_storage_get_status,
            storage::commands::desktop_storage_load,
            storage::commands::desktop_storage_get,
            storage::commands::desktop_storage_set,
            storage::commands::desktop_storage_delete,
            storage::commands::desktop_storage_clear_all_for_scope,
            storage::commands::desktop_storage_clear_all_except,
            storage::commands::desktop_storage_set_many,
            storage::commands::desktop_storage_import,
            storage::commands::desktop_storage_get_marker,
            storage::commands::desktop_storage_set_marker,
            storage::commands::desktop_known_instances_get_all,
            storage::commands::desktop_known_instances_upsert,
            storage::commands::desktop_known_instances_delete,
            runtime::commands::desktop_runtime_config_initial_input,
            runtime::commands::desktop_runtime_config_resolve,
            runtime::commands::desktop_runtime_config_cancel_resolution,
            runtime::commands::desktop_runtime_config_prepare,
            runtime::commands::desktop_runtime_config_commit,
            runtime::commands::desktop_runtime_config_abort,
            runtime::commands::desktop_runtime_config_finalize,
            runtime::commands::desktop_runtime_config_rollback,
            runtime::commands::desktop_runtime_config_deactivate,
            spellcheck::commands::spellcheck_set_state,
            spellcheck::commands::spellcheck_get_available_languages,
            spellcheck::commands::spellcheck_get_bundled_dictionaries,
            spellcheck::commands::spellcheck_add_word_to_dictionary,
            spellcheck::commands::spellcheck_update_autodetect_language,
            spellcheck::commands::spellcheck_check_word,
            gateway::commands::native_gateway_transport_is_available,
            gateway::commands::native_gateway_transport_create,
            gateway::commands::native_gateway_transport_send_text,
            gateway::commands::native_gateway_transport_send_binary,
            gateway::commands::native_gateway_transport_close,
            gateway::commands::native_gateway_transport_dispose,
            local_app::proxy::upload_progress::local_app_upload_subscribe,
            tray::commands::tray_runtime_state_update,
            unread_badge::set_badge_count,
            unread_badge::set_unread_badge,
            taskbar_progress::taskbar_progress_set,
            i18n::commands::native_locale_set,
            notifications::commands::show_notification,
            notifications::commands::notification_sound_allowed,
            notifications::commands::close_notification,
            notifications::commands::close_notifications,
            streaming_priority::streaming_priority_acquire,
            streaming_priority::streaming_priority_release,
            streaming_priority::streaming_priority_get_diagnostics,
            idle::system_idle_time_ms,
            gpu_info::get_gpu_info,
            app_metrics::get_app_metrics,
            #[cfg(target_os = "linux")]
            appearance::linux_appearance_get,
            window::popout::commands::popout_set_always_on_top,
            window::popout::commands::popout_focus,
            event_sink::voice_debug_event_sink_open,
            event_sink::voice_debug_event_sink_append,
            event_sink::voice_debug_event_sink_set_stats_html,
            autostart::autostart_enable,
            autostart::autostart_disable,
            autostart::autostart_is_enabled,
            autostart::autostart_is_initialized,
            autostart::autostart_mark_initialized,
            deep_link::get_initial_deep_link,
            #[cfg(any(windows, target_os = "macos"))]
            jump_list::direct_messages_availability_set,
            menu::app_menu_action,
            theme_files::commands::theme_local_files_pick,
            theme_files::commands::theme_local_files_read,
            theme_files::commands::theme_local_files_clear,
            theme_files::commands::theme_directory_import,
            theme_files::commands::theme_linked_files_pick,
            theme_files::commands::theme_linked_files_watch,
            voice_background::voice_background_media_cache_write,
            voice_background::voice_background_media_cache_read,
            voice_background::voice_background_media_cache_delete,
            downloads::download_file,
        ])
        .setup(|app| {
            let renderer_root = resolve_renderer_root(app.handle())?;
            tracing::info!(root = %renderer_root.display(), "serving the local renderer");
            let app_data_dir = app.path().app_data_dir()?;
            let args = config::launch::process_args();
            let config = Arc::new(DesktopConfig::load(&app_data_dir, &args));
            app.manage(config.clone());
            app.manage(i18n::MainI18n::load(&app_data_dir));
            app.manage(notifications::Notifications::new(&app_data_dir));
            #[cfg(target_os = "linux")]
            app.manage(launcher_entry::LauncherEntry::default());
            #[cfg(target_os = "linux")]
            app.manage(appearance::LinuxAppearance::start(
                app.handle(),
                !config::launch::linux_portals_enabled(&args, |name| std::env::var(name).ok())
                    .unwrap_or_else(|error| {
                        tracing::warn!(%error, "Linux portals are unavailable");
                        false
                    }),
            ));
            #[cfg(windows)]
            app.manage(unread_badge::WindowsOverlay::default());
            if config::launch::resets_window_state(&args) {
                window::clear_saved_state(app.handle());
            }
            // `isStartMinimizedLaunch` of AutostartLaunch.ts.
            let start_hidden = config::launch::is_autostart_launch(&args)
                && config.window_behavior().start_minimized;
            let storage =
                storage::open_desktop_app_storage(&app_data_dir, &app.state::<i18n::MainI18n>());
            let app_store = storage.status().available;
            let outbound = Arc::new(net::outbound::OutboundHttp::new(
                net::trust::client_config()?
            ));
            let markers = runtime::Markers::over(
                storage.clone(),
                |storage, key| async move { storage.get_marker(&key).await },
                |storage, key, value| async move { storage.set_marker(&key, &value).await },
            );
            let documents = app.state::<Arc<DocumentRegistry>>().inner().clone();
            let runtime = Arc::new(runtime::RuntimeConfig::new(
                outbound,
                markers,
                documents.clone(),
            ));
            app.manage(runtime.clone());
            let known_instances = storage.clone();
            let gateway = gateway::start(documents, runtime.outbound(), move || {
                let storage = known_instances.clone();
                Box::pin(async move { storage.get_all_known_instances().await })
            });
            let anchors = gateway.registry().clone();
            runtime.plans().set_observer(Box::new(move |plan| {
                anchors.record_anchor_gateway(&plan.endpoints.api, &plan.endpoints.gateway);
            }));
            let gateway_socket = gateway.is_available();
            app.manage(gateway);
            let events = app.handle().clone();
            app.manage(Arc::new(LocalApp::new(
                renderer_root,
                config,
                runtime,
                Box::new(move |webview_label, progress| {
                    events.state::<BridgeEvents>().emit(
                        webview_label,
                        local_app::proxy::upload_progress::UPLOAD_PROGRESS_EVENT,
                        progress,
                    );
                }),
            )));
            app.manage(storage);
            let capabilities = bridge::CapabilityManifest {
                app_store,
                gateway_socket,
            };
            app.manage(capabilities);
            // Electron's start-up ran its jump list, dock menu, autostart and application menu
            // phases before it created the window.
            #[cfg(windows)]
            jump_list::initialize(app.handle());
            #[cfg(target_os = "macos")]
            dock_menu::install(app.handle());
            #[cfg(target_os = "linux")]
            autostart::repair_on_launch(app.handle());
            #[cfg(target_os = "macos")]
            menu::macos::install(app.handle());
            let main_window = window::create_main_window(app.handle(), capabilities, start_hidden)?;
            if config::launch::opens_devtools(&args) {
                tracing::info!("Opening DevTools for this launch");
                menu::open_devtools(&main_window, true);
            }
            if config::launch::forwards_renderer_console(&args) {
                renderer_console::forward(&main_window);
            }
            jump_list::launch_task(app.handle(), &args);
            tray::initialize(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("the Tauri application failed to build")
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { code, api, .. } => {
                window::policy::on_exit_requested(app, code, &api);
            }
            tauri::RunEvent::Exit => shutdown(app),
            // `activate`: the dock icon was clicked.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => window::show_main_window(app),
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Opened { urls } => deep_link::opened(app, &urls),
            _ => {}
        });
}

/// `will-quit` cleanup order of the Electron shell: runtime-config handlers first (they roll
/// back a provisional commit), then the local app protocol, then the native gateway transport,
/// then outbound HTTP, then the tray.
fn shutdown(app: &tauri::AppHandle) {
    let runtime = app.state::<Arc<runtime::RuntimeConfig>>();
    runtime.cleanup();
    app.state::<Arc<LocalApp>>().upload_progress().clear();
    runtime.plans().shutdown();
    app.state::<gateway::GatewayTransport>().cleanup();
    runtime.outbound().cleanup();
    tray::destroy(app);
    #[cfg(target_os = "linux")]
    app.state::<appearance::LinuxAppearance>().close();
}

fn resolve_renderer_root(app: &tauri::AppHandle) -> tauri::Result<PathBuf> {
    if let Some(root) = std::env::var_os(RENDERER_DIR_ENV) {
        return Ok(PathBuf::from(root));
    }
    Ok(app.path().resource_dir()?.join(RENDERER_RESOURCE_DIR))
}
