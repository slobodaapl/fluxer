// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuildChannel {
    Stable,
    Canary,
    Development,
}

impl BuildChannel {
    pub const fn current() -> Self {
        match option_env!("FLUXER_BUILD_CHANNEL") {
            Some(value) => Self::parse(value),
            None => Self::Development,
        }
    }

    const fn parse(value: &str) -> Self {
        match value.as_bytes() {
            b"stable" => Self::Stable,
            b"canary" => Self::Canary,
            b"development" => Self::Development,
            _ => panic!("FLUXER_BUILD_CHANNEL must be stable, canary or development"),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Canary => "canary",
            Self::Development => "development",
        }
    }

    /// `DESKTOP_APP_NAME` in fluxer_desktop/src/common/DesktopIdentity.ts.
    pub const fn app_name(self) -> &'static str {
        match self {
            Self::Stable => "Fluxer",
            Self::Canary => "Fluxer Canary",
            Self::Development => "Fluxer Development",
        }
    }

    /// The desktop entry id (Linux `.desktop` file name without the suffix), also the Tauri
    /// bundle identifier. It differs from Electron's `LINUX_DESKTOP_ENTRY_ID`
    /// (`app.fluxer.FluxerDesktop*`) on purpose: until this shell replaces the Electron one, the
    /// two installs must not claim each other's launcher entries, badges or notifications.
    pub const fn desktop_id(self) -> &'static str {
        match self {
            Self::Stable => "app.fluxer.tauri.stable",
            Self::Canary => "app.fluxer.tauri.canary",
            Self::Development => "app.fluxer.tauri.development",
        }
    }

    /// The Windows AppUserModelID, kept apart from Electron's `Fluxer.Fluxer*` for the same
    /// reason as [`Self::desktop_id`], whose value it shares. Toasts are shown under it.
    #[cfg(windows)]
    pub const fn app_user_model_id(self) -> &'static str {
        self.desktop_id()
    }

    /// `LOCAL_DEVELOPMENT_INSTANCE_URL` in fluxer_desktop/src/common/Constants.ts.
    pub const fn local_development_instance_url(self) -> Option<&'static str> {
        match self {
            Self::Development => Some("http://localhost:8088"),
            Self::Stable | Self::Canary => None,
        }
    }

    /// Deep-link scheme. Stable and canary share one, as in the Electron build.
    pub const fn app_protocol(self) -> &'static str {
        match self {
            Self::Stable | Self::Canary => "fluxer",
            Self::Development => "fluxer-development",
        }
    }
}

const _: BuildChannel = BuildChannel::current();

/// `IS_OFFLINE_BUILD`: the build set `FLUXER_OFFLINE` to `1` or `true`
/// (fluxer_desktop/scripts/build.mjs).
pub const OFFLINE_BUILD: bool = match option_env!("FLUXER_OFFLINE") {
    Some(value) => matches!(value.as_bytes(), b"1" | b"true"),
    None => false,
};
