// SPDX-License-Identifier: AGPL-3.0-or-later

//! WebKitGTK settings Chromium had on by default, and the launch's smooth-scrolling setting
//! (Electron's `--enable-smooth-scrolling`/`--disable-smooth-scrolling` switch). Whether WebRTC
//! works also depends on the distribution building WebKitGTK with it; the setting is a no-op
//! where it was compiled out.

use tauri::WebviewWindow;
use webkit2gtk::{SettingsExt, WebContextExt, WebViewExt};

pub fn configure(window: &WebviewWindow, smooth_scrolling: bool) -> tauri::Result<()> {
    window.with_webview(move |webview| {
        // Electron showed no native context menu: editable targets get the renderer's own
        // menu through `textarea-context-menu` (bridge/spellcheck.ts), everything else none.
        webview.inner().connect_context_menu(|_, _, _, _| true);
        let Some(settings) = webview.inner().settings() else {
            tracing::error!("the WebKitGTK webview has no settings object");
            return;
        };
        // Electron let the renderer's `window.open` through without a user gesture; the
        // popout windows are opened from effects that run after the click.
        settings.set_javascript_can_open_windows_automatically(true);
        settings.set_enable_media_stream(true);
        settings.set_enable_webrtc(true);
        settings.set_enable_smooth_scrolling(smooth_scrolling);
        settings.set_enable_write_console_messages_to_stdout(cfg!(debug_assertions));
    })
}

/// Turns WebKit's spell checking on for the system dictionary `tags` (`en-us`), or off when
/// there are none. The languages belong to the webview's web context.
pub fn apply_spell_checking(window: &WebviewWindow, tags: &[String]) -> tauri::Result<()> {
    let languages: Vec<String> = tags.iter().map(|tag| webkit_language(tag)).collect();
    window.with_webview(move |webview| {
        let Some(context) = webview.inner().context() else {
            tracing::error!("the WebKitGTK webview has no web context");
            return;
        };
        if languages.is_empty() {
            context.set_spell_checking_enabled(false);
            return;
        }
        let languages: Vec<&str> = languages.iter().map(String::as_str).collect();
        context.set_spell_checking_languages(&languages);
        context.set_spell_checking_enabled(true);
    })
}

/// The `lang_COUNTRY` form WebKit passes to Enchant: `en-us` → `en_US`, `de-ch-frami` →
/// `de_CH_frami`, matching the dictionary file names the tags were read from.
fn webkit_language(tag: &str) -> String {
    tag.split('-')
        .enumerate()
        .map(|(index, part)| {
            if index > 0 && part.len() == 2 && part.chars().all(|ch| ch.is_ascii_alphabetic()) {
                part.to_uppercase()
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::webkit_language;

    #[test]
    fn tags_become_enchant_language_names() {
        assert_eq!(webkit_language("en-us"), "en_US");
        assert_eq!(webkit_language("de"), "de");
        assert_eq!(webkit_language("de-ch-frami"), "de_CH_frami");
    }
}
