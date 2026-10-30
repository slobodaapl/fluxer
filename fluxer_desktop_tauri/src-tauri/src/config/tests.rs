// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::json;

use super::*;

fn settings_file(directory: &Path) -> PathBuf {
    directory.join(CONFIG_FILE_NAME)
}

fn load(directory: &Path) -> DesktopConfig {
    DesktopConfig::load(directory, &["app".to_owned()])
}

fn stored(directory: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(settings_file(directory)).unwrap()).unwrap()
}

#[test]
fn a_missing_or_corrupt_file_loads_the_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let config = load(directory.path());
    let defaults = config.window_behavior();
    assert!(defaults.show_tray_icon && defaults.close_to_tray && defaults.remember_window_state);
    assert!(!defaults.use_native_title_bar);
    assert_eq!(
        config.launch_troubleshooting(),
        DesktopTroubleshootingSettings::default()
    );
    assert_eq!(config.preboot_theme(), None);
    assert_eq!(config.launch_product_name(), None);
    assert!(!config.has_requested_mac_screen_recording());
    assert!(!settings_file(directory.path()).exists());

    std::fs::write(settings_file(directory.path()), "{ not json").unwrap();
    assert_eq!(load(directory.path()).window_behavior(), defaults);
}

#[test]
fn loading_coerces_the_stored_fields() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        settings_file(directory.path()),
        json!({
            "window_behavior": {"showTrayIcon": false, "closeToTrayV2": true, "useNativeTitleBar": "x"},
            "troubleshooting": {"disableModuleUpdates": true},
            "preboot_theme": "Dark",
            "instance_product_names": [{"instance_key": "k", "product_name": "  Acme "}],
        })
        .to_string(),
    )
    .unwrap();
    let config = load(directory.path());
    let behavior = config.window_behavior();
    assert!(!behavior.show_tray_icon && !behavior.close_to_tray);
    assert!(!behavior.use_native_title_bar);
    assert!(config.launch_troubleshooting().disable_module_updates);
    assert_eq!(config.preboot_theme(), None);
    assert_eq!(config.launch_product_name().as_deref(), Some("Acme"));
}

#[test]
fn changes_are_written_atomically_and_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(settings_file(directory.path()), r#"{"future": [1, 2]}"#).unwrap();
    let config = load(directory.path());
    let updated =
        config.set_window_behavior(&json!({"minimizeToTray": true, "showTrayIcon": true}));
    assert!(updated.minimize_to_tray);
    config.set_troubleshooting(|settings| settings.disable_hardware_acceleration = true);
    config.set_preboot_theme("midnight");
    config.remember_instance_product_name(&json!({"instanceKey": "a", "productName": "Alpha"}));
    config.mark_mac_screen_recording_requested();
    config.add_allowed_theme_local_files(&[
        "/b.css".to_owned(),
        " ".to_owned(),
        "/a.css".to_owned(),
    ]);
    config.update_global_shortcuts(|settings| settings.migrated = true);

    let raw = std::fs::read_to_string(settings_file(directory.path())).unwrap();
    assert!(raw.starts_with("{\n  \"future\": [\n"), "{raw}");
    assert!(!directory.path().join("settings.json.tmp").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(settings_file(directory.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(
        stored(directory.path()),
        json!({
            "future": [1, 2],
            "window_behavior": {
                "showTrayIcon": true,
                "coloredTrayIcon": false,
                "darkTaskbarBadge": false,
                "useNativeTitleBar": false,
                "rememberWindowState": true,
                "allowTransparency": false,
                "smoothScrolling": true,
                "middleClickAutoscroll": false,
                "startMinimized": false,
                "minimizeToTrayV2": true,
                "closeToTrayV2": true,
            },
            "troubleshooting": {"disableHardwareAcceleration": true, "disableModuleUpdates": false},
            "preboot_theme": "midnight",
            "instance_product_names": [{"instance_key": "a", "product_name": "Alpha"}],
            "mac_screen_recording_requested": true,
            "theme_allowed_local_files": ["/a.css", "/b.css"],
            "global_shortcuts": {"portal_consent": "unset", "direct_input_enabled": false, "migrated": true},
        })
    );

    let reloaded = load(directory.path());
    assert_eq!(reloaded.window_behavior(), config.window_behavior());
    assert_eq!(reloaded.preboot_theme().as_deref(), Some("midnight"));
    assert_eq!(reloaded.launch_product_name().as_deref(), Some("Alpha"));
    assert_eq!(reloaded.allowed_theme_local_files(), ["/a.css", "/b.css"]);
    assert!(reloaded.global_shortcuts().migrated);
    assert!(reloaded.has_requested_mac_screen_recording());
    // The launch view reflects the file as loaded; the running config's view does not move.
    assert!(
        !config
            .launch_troubleshooting()
            .disable_hardware_acceleration
    );
    assert_eq!(
        reloaded
            .launch_troubleshooting()
            .disable_hardware_acceleration,
        !cfg!(target_os = "macos")
    );

    reloaded.clear_allowed_theme_local_files();
    assert!(
        stored(directory.path())
            .get("theme_allowed_local_files")
            .is_none()
    );
}

#[test]
fn unchanged_values_are_not_rewritten() {
    let directory = tempfile::tempdir().unwrap();
    let config = load(directory.path());
    config.set_preboot_theme("Not-A-Token");
    config.remember_instance_product_name(&json!({"instanceKey": "", "productName": "x"}));
    assert!(!settings_file(directory.path()).exists());
    config.set_preboot_theme("dark");
    std::fs::remove_file(settings_file(directory.path())).unwrap();
    config.set_preboot_theme("dark");
    config.remember_instance_product_name(&json!({"instanceKey": "a", "productName": "A"}));
    std::fs::remove_file(settings_file(directory.path())).unwrap();
    config.remember_instance_product_name(&json!({"instanceKey": "a", "productName": " A "}));
    config.mark_mac_screen_recording_requested();
    std::fs::remove_file(settings_file(directory.path())).unwrap();
    config.mark_mac_screen_recording_requested();
    assert!(!settings_file(directory.path()).exists());
}

#[test]
fn the_newest_product_name_report_is_the_launch_name() {
    let directory = tempfile::tempdir().unwrap();
    let config = load(directory.path());
    config.remember_instance_product_name(&json!({"instanceKey": "a", "productName": "Alpha"}));
    config.remember_instance_product_name(&json!({"instanceKey": "b", "productName": "Beta"}));
    assert_eq!(config.launch_product_name().as_deref(), Some("Beta"));
    config.remember_instance_product_name(&json!({"instanceKey": "a", "productName": "Alpha 2"}));
    assert_eq!(
        stored(directory.path())["instance_product_names"],
        json!([
            {"instance_key": "a", "product_name": "Alpha 2"},
            {"instance_key": "b", "product_name": "Beta"},
        ])
    );
}

#[test]
fn the_title_bar_and_transparency_settings_pend_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let config = load(directory.path());
    assert!(!config.active_window_behavior().use_native_title_bar);
    assert!(!config.active_window_behavior().allow_transparency);

    let updated = config.set_window_behavior(&json!({"useNativeTitleBar": true}));
    assert!(updated.use_native_title_bar && !updated.active_use_native_title_bar);
    assert_eq!(
        config.window_behavior_pending_restart(),
        !cfg!(target_os = "macos")
    );

    // Transparency pends a restart everywhere and masks the native title bar again.
    let updated = config.set_window_behavior(&json!({"allowTransparency": true}));
    assert!(updated.allow_transparency && !updated.active_allow_transparency);
    assert!(config.window_behavior_pending_restart());

    config.set_window_behavior(&json!({"useNativeTitleBar": false, "allowTransparency": false}));
    assert!(!config.window_behavior_pending_restart());
    config.set_window_behavior(&json!({"smoothScrolling": false, "middleClickAutoscroll": true}));
    assert!(!config.window_behavior_pending_restart());
}

#[test]
fn hiding_the_tray_pends_a_restart_on_linux_only() {
    let directory = tempfile::tempdir().unwrap();
    let config = load(directory.path());
    config.set_window_behavior(&json!({"showTrayIcon": false}));
    assert_eq!(
        config.window_behavior_pending_restart(),
        cfg!(target_os = "linux")
    );
    config.set_window_behavior(&json!({"showTrayIcon": true}));
    assert!(!config.window_behavior_pending_restart());
}

#[test]
fn settings_save_into_a_directory_that_does_not_exist_yet() {
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("app-data");
    let config = load(&directory);
    config.set_preboot_theme("dark");
    assert_eq!(stored(&directory), json!({"preboot_theme": "dark"}));
}
