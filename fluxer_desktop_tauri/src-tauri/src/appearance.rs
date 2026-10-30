// SPDX-License-Identifier: AGPL-3.0-or-later

//! The portal appearance settings of LinuxAppearance.ts and the `linux-appearance-get` /
//! `linux-appearance-changed` handlers of IpcHandlers.ts, read through
//! fluxer_desktop/native/linux-portals. Electron read on its main thread; here reads run on a
//! blocking task and change re-reads on the subscription's own thread. Each read still opens its
//! own session bus connection, as the native crate does.

use std::sync::{Arc, Mutex};

use fluxer_linux_portals::settings::{
    self, AccentColor, ChangeEvent, ChangePayload, ColorScheme, Contrast, Subscription,
};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::error::BridgeError;
use crate::events::BridgeEvents;
use crate::window::MAIN_WINDOW_LABEL;

const CHANGED_EVENT: &str = "linux-appearance-changed";
const COLOR_SCHEME_KEY: &str = "color-scheme";
const CONTRAST_KEY: &str = "contrast";
const ACCENT_COLOR_KEY: &str = "accent-color";

#[derive(Clone, Copy, Debug, PartialEq)]
struct Appearance {
    color_scheme: ColorScheme,
    contrast: Contrast,
    accent: Option<AccentColor>,
}

impl Appearance {
    /// `readLinuxAppearance` with the addon disabled.
    const DEFAULT: Self = Self {
        color_scheme: ColorScheme::NoPreference,
        contrast: Contrast::NoPreference,
        accent: None,
    };

    fn read() -> Self {
        Self {
            color_scheme: settings::read_color_scheme(),
            contrast: settings::read_contrast(),
            accent: settings::read_accent_color(),
        }
    }

    /// `subscribeLinuxAppearance`: the freshly read settings with the changed key taken from the
    /// signal. A signal value the portal crate could not classify keeps the read value, except
    /// for the accent, which then reads as none.
    fn with_change(mut self, event: &ChangeEvent) -> Self {
        match (event.key.as_str(), &event.payload) {
            (COLOR_SCHEME_KEY, ChangePayload::Uint32(value)) => {
                self.color_scheme = ColorScheme::from_u32(*value);
            }
            (CONTRAST_KEY, ChangePayload::Uint32(value)) => {
                self.contrast = Contrast::from_u32(*value);
            }
            (ACCENT_COLOR_KEY, ChangePayload::Accent(accent)) => self.accent = Some(*accent),
            (ACCENT_COLOR_KEY, _) => self.accent = None,
            _ => {}
        }
        self
    }
}

/// `LinuxAppearanceSnapshot` of packages/desktop_ipc/src/DesktopBridgeTypes.ts.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    color_scheme: &'static str,
    contrast: &'static str,
    accent: Option<Accent>,
}

#[derive(Serialize)]
struct Accent {
    r: f64,
    g: f64,
    b: f64,
}

impl From<Appearance> for Snapshot {
    fn from(appearance: Appearance) -> Self {
        Self {
            color_scheme: appearance.color_scheme.as_str(),
            contrast: appearance.contrast.as_str(),
            accent: appearance
                .accent
                .map(|AccentColor { r, g, b }| Accent { r, g, b }),
        }
    }
}

#[derive(Default)]
struct SubscriptionSlot {
    subscription: Option<Subscription>,
    closed: bool,
}

/// The single settings subscription Electron opened at handler registration.
pub struct LinuxAppearance {
    disabled: bool,
    slot: Arc<Mutex<SubscriptionSlot>>,
}

impl LinuxAppearance {
    /// Subscribes from a background thread, since the portal crate waits up to five seconds for
    /// the bus. `disabled` is `getLinuxPortalsMode(argv) === 'off'`.
    pub fn start(app: &AppHandle, disabled: bool) -> Self {
        let slot = Arc::new(Mutex::new(SubscriptionSlot::default()));
        if disabled {
            tracing::info!("linux portals disabled by launch options; appearance reads disabled");
        } else {
            let app = app.clone();
            let slot = slot.clone();
            tauri::async_runtime::spawn_blocking(move || subscribe(app, &slot));
        }
        Self { disabled, slot }
    }

    /// Stops the subscription; one still starting is stopped when it arrives.
    pub fn close(&self) {
        let subscription = {
            let mut slot = self.slot.lock().expect("appearance subscription lock");
            slot.closed = true;
            slot.subscription.take()
        };
        if let Some(subscription) = subscription {
            subscription.close();
        }
    }
}

fn subscribe(app: AppHandle, slot: &Mutex<SubscriptionSlot>) {
    let subscription = Subscription::new(Arc::new(move |event: ChangeEvent| {
        let snapshot = Snapshot::from(Appearance::read().with_change(&event));
        app.state::<BridgeEvents>()
            .emit(MAIN_WINDOW_LABEL, CHANGED_EVENT, snapshot);
    }));
    let subscription = match subscription {
        Ok(subscription) => subscription,
        Err(error) => {
            tracing::warn!(%error, "linux-portals Settings subscribe failed");
            return;
        }
    };
    let mut slot = slot.lock().expect("appearance subscription lock");
    if slot.closed {
        drop(slot);
        subscription.close();
    } else {
        slot.subscription = Some(subscription);
    }
}

#[tauri::command]
pub async fn linux_appearance_get(
    appearance: tauri::State<'_, LinuxAppearance>,
) -> Result<Snapshot, BridgeError> {
    if appearance.disabled {
        return Ok(Appearance::DEFAULT.into());
    }
    tauri::async_runtime::spawn_blocking(Appearance::read)
        .await
        .map(Snapshot::from)
        .map_err(|error| BridgeError::new("Error", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(key: &str, payload: ChangePayload) -> ChangeEvent {
        ChangeEvent {
            namespace: settings::APPEARANCE_NAMESPACE.to_owned(),
            key: key.to_owned(),
            payload,
        }
    }

    fn payload(appearance: Appearance) -> serde_json::Value {
        serde_json::to_value(Snapshot::from(appearance)).unwrap()
    }

    const READ: Appearance = Appearance {
        color_scheme: ColorScheme::PreferLight,
        contrast: Contrast::NoPreference,
        accent: Some(AccentColor {
            r: 0.25,
            g: 0.5,
            b: 1.0,
        }),
    };

    #[test]
    fn the_renderer_payload_has_the_electron_shape() {
        assert_eq!(
            payload(READ),
            serde_json::json!({
                "colorScheme": "prefer-light",
                "contrast": "no-preference",
                "accent": {"r": 0.25, "g": 0.5, "b": 1.0},
            })
        );
        assert_eq!(
            payload(Appearance::DEFAULT),
            serde_json::json!({"colorScheme": "no-preference", "contrast": "no-preference", "accent": null})
        );
    }

    #[test]
    fn a_change_overrides_the_reread_key_from_the_signal() {
        let dark = READ.with_change(&event(COLOR_SCHEME_KEY, ChangePayload::Uint32(1)));
        assert_eq!(payload(dark)["colorScheme"], "prefer-dark");
        assert_eq!(dark.accent, READ.accent);
        let unknown_scheme = READ.with_change(&event(COLOR_SCHEME_KEY, ChangePayload::Uint32(7)));
        assert_eq!(payload(unknown_scheme)["colorScheme"], "no-preference");
        let high = READ.with_change(&event(CONTRAST_KEY, ChangePayload::Uint32(1)));
        assert_eq!(payload(high)["contrast"], "high");
    }

    #[test]
    fn an_unclassified_signal_value_keeps_the_read_value_except_for_the_accent() {
        // The portal crate reports 0 (and negative accents) as Unknown.
        let scheme = READ.with_change(&event(COLOR_SCHEME_KEY, ChangePayload::Unknown));
        assert_eq!(scheme, READ);
        let accent = READ.with_change(&event(ACCENT_COLOR_KEY, ChangePayload::Unknown));
        assert_eq!(payload(accent)["accent"], serde_json::Value::Null);
        let new_accent = AccentColor {
            r: 1.0,
            g: 0.0,
            b: 0.0,
        };
        let accent = READ.with_change(&event(ACCENT_COLOR_KEY, ChangePayload::Accent(new_accent)));
        assert_eq!(accent.accent, Some(new_accent));
        let other = READ.with_change(&event("reduced-motion", ChangePayload::Uint32(1)));
        assert_eq!(other, READ);
    }
}
