// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reuse Fluxer's tested PipeWire routing/capture engine without its Node API.
//! Source stays shared with the existing desktop client to preserve its loop
//! prevention and application-isolation behavior during migration.
#![allow(dead_code)]

#[cfg(target_os = "linux")]
#[rustfmt::skip]
#[path = "../../../../fluxer_desktop/native/linux-audio-capture/src/audio_contract.rs"]
mod audio_contract;
#[cfg(target_os = "linux")]
#[rustfmt::skip]
#[path = "../../../../fluxer_desktop/native/linux-audio-capture/src/capture_graph.rs"]
mod capture_graph;
#[cfg(target_os = "linux")]
#[rustfmt::skip]
#[path = "../../../../fluxer_desktop/native/linux-audio-capture/src/direct_buffer.rs"]
mod direct_buffer;
#[cfg(target_os = "linux")]
#[rustfmt::skip]
#[path = "../../../../fluxer_desktop/native/linux-audio-capture/src/pipewire/mod.rs"]
mod pipewire;
#[cfg(target_os = "linux")]
#[rustfmt::skip]
#[path = "../../../../fluxer_desktop/native/linux-audio-capture/src/pipewire_bridge.rs"]
mod pipewire_bridge;
#[cfg(target_os = "linux")]
#[rustfmt::skip]
#[path = "../../../../fluxer_desktop/native/linux-audio-capture/src/routing.rs"]
mod routing;
#[cfg(target_os = "linux")]
#[rustfmt::skip]
#[path = "../../../../fluxer_desktop/native/linux-audio-capture/src/self_identity.rs"]
mod self_identity;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Application {
    pub process_id: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    System,
    Application { process_id: String },
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("System audio capture is unavailable on this platform")]
    Unsupported,
    #[error("PipeWire audio capture is unavailable; check the user audio service")]
    Unavailable,
    #[error("Invalid application audio source")]
    InvalidSource,
}
pub struct PcmFrame {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u32,
}

pub struct SystemAudio {
    #[cfg(target_os = "linux")]
    backend: pipewire_bridge::PipeWireDirectCapture,
}

impl SystemAudio {
    pub fn available() -> bool {
        #[cfg(target_os = "linux")]
        {
            pipewire_bridge::daemon_reachable()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Enumerates playback applications without starting capture or moving any
    /// audio routes. The current Fluxer process is excluded from the picker.
    pub fn applications() -> Result<Vec<Application>, Error> {
        #[cfg(target_os = "linux")]
        {
            let bridge = pipewire_bridge::PipeWireBridge::open().ok_or(Error::Unavailable)?;
            let own_pid = std::process::id().to_string();
            let mut entries = std::collections::BTreeMap::new();
            for properties in bridge.inventory() {
                if properties.get("media.class").map(String::as_str) != Some("Stream/Output/Audio")
                {
                    continue;
                }
                let Some(pid) = properties.get("application.process.id") else {
                    continue;
                };
                if pid == &own_pid {
                    continue;
                }
                let label = properties
                    .get("application.name")
                    .or_else(|| properties.get("node.description"))
                    .cloned()
                    .unwrap_or_else(|| format!("Process {pid}"));
                entries.entry(pid.clone()).or_insert(label);
            }
            Ok(entries
                .into_iter()
                .map(|(process_id, label)| Application { process_id, label })
                .collect())
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(Error::Unsupported)
        }
    }

    pub fn start(selection: Selection) -> Result<Self, Error> {
        #[cfg(target_os = "linux")]
        {
            let backend =
                pipewire_bridge::PipeWireDirectCapture::open().ok_or(Error::Unavailable)?;
            let mut identity = routing::SelfIdentity::default();
            self_identity::populate_self_identity(&mut identity);
            identity.add_pid(std::process::id().to_string());
            identity.add_display_name("Fluxer".to_string());
            identity.add_display_name("fluxer-desktop".to_string());
            backend.populate_self_identity(identity);
            let include_when = match selection {
                Selection::System => vec![routing::PropPattern::new()],
                Selection::Application { process_id } => {
                    if process_id.is_empty()
                        || !process_id.bytes().all(|b| b.is_ascii_digit())
                        || process_id == std::process::id().to_string()
                    {
                        return Err(Error::InvalidSource);
                    }
                    vec![routing::PropPattern::from([(
                        "application.process.id".into(),
                        process_id,
                    )])]
                }
            };
            if !backend.start(routing::RoutingRule {
                include_when,
                skip_hardware_devices: true,
                ..Default::default()
            }) {
                return Err(Error::Unavailable);
            }
            Ok(Self { backend })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = selection;
            Err(Error::Unsupported)
        }
    }

    pub fn read(&self) -> Option<PcmFrame> {
        #[cfg(target_os = "linux")]
        {
            self.backend.read().map(|frame| PcmFrame {
                samples: frame.samples,
                sample_rate: frame.sample_rate,
                channels: frame.channels,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
    pub fn stop(&self) {
        #[cfg(target_os = "linux")]
        self.backend.stop();
    }
}
impl Drop for SystemAudio {
    fn drop(&mut self) {
        self.stop();
    }
}
