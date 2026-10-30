// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the main window's `setWindowOpenHandler` and `did-create-window` in
//! fluxer_desktop/src/main/Window.ts decided from a `window.open` call. Electron saw the frame
//! name with the request; Tauri passes no name, so the decision is split: the URL and the
//! capacity are checked when the window is requested, the name once the child's `window.name`
//! has been read back from it.

use crate::js::utf16_length;

const BLANK_DOCUMENT: &str = "about:blank";
/// `VOICE_POPOUT_WINDOW_NAME_PREFIX` and `VOICE_POPOUT_WINDOW_NAME_LENGTH_MAX`.
const VOICE_POPOUT_NAME_PREFIX: &str = "fluxer-voice-popout:";
const VOICE_POPOUT_NAME_LENGTH_MAX: usize = 256;
/// `QUICK_CSS_EDITOR_WINDOW_KEY`, also the key `popoutSetAlwaysOnTop` and `popoutFocus` take.
pub const QUICK_CSS_EDITOR_KEY: &str = "fluxer_quick_css_editor";
const QUICK_CSS_EDITOR_TITLE: &str = "Fluxer | Quick CSS";
/// `VOICE_POPOUT_WINDOWS_MAX`.
const VOICE_POPOUTS_MAX: usize = 8;

#[derive(Debug, PartialEq, Eq)]
pub enum OpenRequest {
    /// Anything but a blank page leaves the app (`openExternalDeduped` + deny).
    OpenExternally,
    /// `Denied voice popout window: capacity reached`.
    AtCapacity,
    /// A child window is created and named afterwards.
    Create,
}

/// `open_children` counts the voice popouts and the children not named yet: a child that is
/// still unnamed may turn out to be a voice popout, so it holds a place.
pub fn request(url: &str, open_children: usize) -> OpenRequest {
    if url != BLANK_DOCUMENT {
        return OpenRequest::OpenExternally;
    }
    if open_children >= VOICE_POPOUTS_MAX {
        return OpenRequest::AtCapacity;
    }
    OpenRequest::Create
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PopoutKind {
    /// A voice tile or call popout, by its window name.
    Voice(String),
    QuickCssEditor,
}

/// The child's window name, as `isVoicePopoutWindowName` and the Quick CSS key read the frame
/// name. `None`: Electron denied the window.
pub fn classify(name: &str) -> Option<PopoutKind> {
    if name == QUICK_CSS_EDITOR_KEY {
        return Some(PopoutKind::QuickCssEditor);
    }
    (name.starts_with(VOICE_POPOUT_NAME_PREFIX)
        && utf16_length(name) <= VOICE_POPOUT_NAME_LENGTH_MAX)
        .then(|| PopoutKind::Voice(name.to_owned()))
}

impl PopoutKind {
    /// `getPopoutWindowOptions(minWidth, minHeight)`.
    pub fn minimum_size(&self) -> (f64, f64) {
        match self {
            Self::Voice(_) => (360.0, 240.0),
            Self::QuickCssEditor => (900.0, 620.0),
        }
    }

    /// The title Electron gave the window before the document set its own.
    pub fn initial_title(&self) -> Option<&'static str> {
        match self {
            Self::Voice(_) => None,
            Self::QuickCssEditor => Some(QUICK_CSS_EDITOR_TITLE),
        }
    }

    /// `trackQuickCssEditorWindow` pinned the editor on top.
    pub fn starts_always_on_top(&self) -> bool {
        matches!(self, Self::QuickCssEditor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_blank_pages_below_capacity_create_a_window() {
        assert_eq!(request("about:blank", 0), OpenRequest::Create);
        assert_eq!(request("about:blank", 7), OpenRequest::Create);
        assert_eq!(request("about:blank", 8), OpenRequest::AtCapacity);
        assert_eq!(request("about:blank", 9), OpenRequest::AtCapacity);
        for url in [
            "https://fluxer.app/x",
            "about:blank#x",
            "about:srcdoc",
            "blob:fluxer-app://app/1",
            "fluxer-app://app/channels/@me",
        ] {
            // The URL wins over capacity, as Electron's handler fell through to the external
            // opener for any name.
            assert_eq!(request(url, 0), OpenRequest::OpenExternally, "{url}");
            assert_eq!(request(url, 8), OpenRequest::OpenExternally, "{url}");
        }
    }

    #[test]
    fn names_select_the_popout_kind() {
        let tile = "fluxer-voice-popout:tile:camera:user-1";
        assert_eq!(classify(tile), Some(PopoutKind::Voice(tile.to_owned())));
        let call = "fluxer-voice-popout:call:123";
        assert_eq!(classify(call), Some(PopoutKind::Voice(call.to_owned())));
        assert_eq!(
            classify("fluxer_quick_css_editor"),
            Some(PopoutKind::QuickCssEditor)
        );
        for name in [
            "",
            "_blank",
            "fluxer-voice-debug-event-sink",
            "fluxer_quick_css_editor ",
            "Fluxer-voice-popout:tile",
        ] {
            assert_eq!(classify(name), None, "{name:?}");
        }
    }

    #[test]
    fn voice_names_are_limited_to_256_utf16_units() {
        let prefix = "fluxer-voice-popout:";
        let longest = format!("{prefix}{}", "a".repeat(256 - prefix.len()));
        assert!(classify(&longest).is_some());
        assert!(classify(&format!("{longest}a")).is_none());
        // An astral character is two units, as in `frameName.length`.
        let astral = format!("{prefix}{}\u{1f600}", "a".repeat(256 - prefix.len() - 2));
        assert!(classify(&astral).is_some());
        assert!(classify(&format!("{astral}a")).is_none());
    }
}
