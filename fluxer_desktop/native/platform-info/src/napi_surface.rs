// SPDX-License-Identifier: AGPL-3.0-or-later

use napi_derive::napi;

#[napi(object)]
pub struct PlatformGpuDeviceInfo {
    pub active: bool,
    #[napi(js_name = "vendorId")]
    pub vendor_id: u32,
    #[napi(js_name = "deviceId")]
    pub device_id: u32,
    #[napi(js_name = "vendorName")]
    pub vendor_name: Option<String>,
    #[napi(js_name = "deviceString")]
    pub device_string: Option<String>,
    #[napi(js_name = "driverVendor")]
    pub driver_vendor: Option<String>,
    #[napi(js_name = "driverVersion")]
    pub driver_version: Option<String>,
    #[napi(js_name = "dedicatedVideoMemory")]
    pub dedicated_video_memory: Option<f64>,
    #[napi(js_name = "sharedSystemMemory")]
    pub shared_system_memory: Option<f64>,
    #[napi(js_name = "subsystemVendorId")]
    pub subsystem_vendor_id: Option<u32>,
    #[napi(js_name = "subsystemDeviceId")]
    pub subsystem_device_id: Option<u32>,
    #[napi(js_name = "registryId")]
    pub registry_id: Option<String>,
    #[napi(js_name = "adapterLuid")]
    pub adapter_luid: Option<String>,
    #[napi(js_name = "pciPath")]
    pub pci_path: Option<String>,
    pub integrated: Option<bool>,
    pub removable: Option<bool>,
    pub headless: Option<bool>,
    pub source: String,
}

impl From<crate::PlatformGpuDeviceInfo> for PlatformGpuDeviceInfo {
    fn from(device: crate::PlatformGpuDeviceInfo) -> Self {
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

#[napi(object)]
pub struct PlatformGpuInfo {
    pub devices: Vec<PlatformGpuDeviceInfo>,
    pub source: String,
    pub error: Option<String>,
}

#[napi(js_name = "getGpuInfo")]
pub fn get_gpu_info() -> PlatformGpuInfo {
    let info = crate::get_gpu_info();
    PlatformGpuInfo {
        devices: info.devices.into_iter().map(Into::into).collect(),
        source: info.source,
        error: info.error,
    }
}
