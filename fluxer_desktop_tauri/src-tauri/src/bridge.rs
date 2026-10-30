// SPDX-License-Identifier: AGPL-3.0-or-later

//! Builds the initialization script that installs `window.electron` before any page script
//! runs. Values Electron's preload read synchronously (platform, channel, capability manifest)
//! are serialised into the script, since Tauri has no `sendSync`.

use serde::Serialize;

use crate::build_channel::BuildChannel;
use crate::config::ActiveWindowBehavior;
use crate::desktop_info::node_platform;
use crate::local_app;

const BRIDGE_BUNDLE: &str = include_str!("../../dist/bridge.js");
const CONFIG_GLOBAL: &str = "__FLUXER_DESKTOP_BRIDGE_CONFIG__";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeConfig {
    platform: &'static str,
    build_channel: &'static str,
    local_development_instance_url: Option<&'static str>,
    offline_build: bool,
    local_app_origin: &'static str,
    capabilities: CapabilityManifest,
    /// The `--fluxer-active-*` renderer arguments Electron's preload read from `process.argv`.
    active_use_native_title_bar: bool,
    active_allow_transparency: bool,
}

/// `DesktopCapabilityManifest` in packages/desktop_ipc/src/CapabilityManifest.ts.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityManifest {
    /// The desktop app store opened and is serving requests.
    pub app_store: bool,
    /// The native gateway transport initialised.
    pub gateway_socket: bool,
}

/// `active`: what the window being created puts in effect.
pub fn initialization_script(
    capabilities: CapabilityManifest,
    active: ActiveWindowBehavior,
) -> String {
    let config = BridgeConfig {
        platform: node_platform(),
        build_channel: BuildChannel::current().as_str(),
        local_development_instance_url: BuildChannel::current().local_development_instance_url(),
        offline_build: crate::build_channel::OFFLINE_BUILD,
        local_app_origin: local_app::url::origin(),
        capabilities,
        active_use_native_title_bar: active.use_native_title_bar,
        active_allow_transparency: active.allow_transparency,
    };
    let config = serde_json::to_string(&config).expect("bridge config serialises");
    format!(
        "Object.defineProperty(globalThis,{CONFIG_GLOBAL:?},{{value:{config},configurable:true}});\n{BRIDGE_BUNDLE}"
    )
}
