// SPDX-License-Identifier: AGPL-3.0-or-later

//! `get-gpu-info` of IpcHandlers.ts: `getGpuInfo` of PlatformInfo.ts over the platform probe of
//! fluxer_desktop/native/platform-info. Electron merged in Chromium's `app.getGPUInfo`, which a
//! webview shell lacks, so `glRenderer`, `glVendor` and `machineModel*` stay absent and the
//! probe's own `active` flags are kept instead of Chromium's.

use fluxer_platform_info::{PlatformGpuDeviceInfo, PlatformGpuInfo};
use serde::Serialize;

/// `GpuInfo` of packages/desktop_ipc/src/DesktopBridgeTypes.ts.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    devices: Vec<GpuDeviceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_source: Option<String>,
}

/// `GpuDeviceInfo` of packages/desktop_ipc/src/DesktopBridgeTypes.ts.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GpuDeviceInfo {
    active: bool,
    vendor_id: u32,
    device_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    vendor_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_string: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    driver_vendor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    driver_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dedicated_video_memory: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shared_system_memory: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subsystem_vendor_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subsystem_device_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    registry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    adapter_luid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pci_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    integrated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    removable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headless: Option<bool>,
    source: String,
}

impl From<PlatformGpuDeviceInfo> for GpuDeviceInfo {
    fn from(device: PlatformGpuDeviceInfo) -> Self {
        Self {
            active: device.active,
            vendor_id: device.vendor_id,
            device_id: device.device_id,
            vendor_name: device.vendor_name,
            device_string: device.device_string,
            driver_vendor: device.driver_vendor,
            driver_version: device.driver_version,
            dedicated_video_memory: device.dedicated_video_memory,
            shared_system_memory: device.shared_system_memory,
            subsystem_vendor_id: device.subsystem_vendor_id,
            subsystem_device_id: device.subsystem_device_id,
            registry_id: device.registry_id,
            adapter_luid: device.adapter_luid,
            pci_path: device.pci_path,
            integrated: device.integrated,
            removable: device.removable,
            headless: device.headless,
            source: device.source,
        }
    }
}

impl From<PlatformGpuInfo> for GpuInfo {
    fn from(info: PlatformGpuInfo) -> Self {
        Self {
            devices: info.devices.into_iter().map(Into::into).collect(),
            native_source: Some(info.source),
        }
    }
}

/// Never rejects: a probe that panics reads as no devices, as Electron's caught native throw did.
#[tauri::command]
pub async fn get_gpu_info() -> GpuInfo {
    match tauri::async_runtime::spawn_blocking(fluxer_platform_info::get_gpu_info).await {
        Ok(info) => info.into(),
        Err(error) => {
            tracing::warn!(%error, "the GPU probe failed");
            GpuInfo {
                devices: Vec::new(),
                native_source: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_device(vendor_id: u32) -> PlatformGpuDeviceInfo {
        PlatformGpuDeviceInfo {
            active: false,
            vendor_id,
            device_id: 0x3e92,
            vendor_name: None,
            device_string: None,
            driver_vendor: None,
            driver_version: None,
            dedicated_video_memory: None,
            shared_system_memory: None,
            subsystem_vendor_id: None,
            subsystem_device_id: None,
            registry_id: None,
            adapter_luid: None,
            pci_path: None,
            integrated: None,
            removable: None,
            headless: None,
            source: fluxer_platform_info::SOURCE_LINUX_SYSFS.to_owned(),
        }
    }

    #[test]
    fn the_payload_has_the_renderer_shape_with_unknown_fields_absent() {
        let primary = PlatformGpuDeviceInfo {
            active: true,
            device_id: 0x2684,
            vendor_name: Some("NVIDIA".to_owned()),
            driver_vendor: Some("nvidia".to_owned()),
            dedicated_video_memory: Some(8_589_934_592.0),
            subsystem_vendor_id: Some(0x1458),
            pci_path: Some("0000:01:00.0".to_owned()),
            headless: Some(false),
            ..bare_device(0x10de)
        };
        let info = GpuInfo::from(PlatformGpuInfo {
            devices: vec![primary, bare_device(0x8086)],
            source: fluxer_platform_info::SOURCE_LINUX_SYSFS.to_owned(),
            error: Some("probe detail the renderer never saw".to_owned()),
        });
        assert_eq!(
            serde_json::to_value(info).unwrap(),
            serde_json::json!({
                "devices": [
                    {
                        "active": true,
                        "vendorId": 0x10de,
                        "deviceId": 0x2684,
                        "vendorName": "NVIDIA",
                        "driverVendor": "nvidia",
                        "dedicatedVideoMemory": 8_589_934_592.0,
                        "subsystemVendorId": 0x1458,
                        "pciPath": "0000:01:00.0",
                        "headless": false,
                        "source": "linux-sysfs",
                    },
                    {"active": false, "vendorId": 0x8086, "deviceId": 0x3e92, "source": "linux-sysfs"},
                ],
                "nativeSource": "linux-sysfs",
            })
        );
    }
}
