// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source MediaProxyUtils animated-image transforms and GIF autoplay default.
use crate::image_gallery::safe_image_url;
use gpui::http_client::Url;
use serde_json::Value;

/// Frame state belongs to this resource revision, never to a previous URL at
/// the same attachment position. Keep signed URLs out of diagnostic IDs.
pub(super) fn image_id(namespace: &str, source: &str) -> gpui::SharedString {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hash = DefaultHasher::new();
    source.hash(&mut hash);
    format!("{namespace}-{:016x}", hash.finish()).into()
}

pub(super) fn is_animated_image(mime: Option<&str>, name_or_url: &str, flags: u64) -> bool {
    let gif_mime = mime.is_some_and(|mime| {
        mime.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("image/gif")
    });
    let path = Url::parse(name_or_url)
        .ok()
        .map(|url| url.path().to_owned())
        .unwrap_or_else(|| name_or_url.to_owned());
    flags & (1 << 5) != 0 || gif_mime || path.to_ascii_lowercase().ends_with(".gif")
}

/// buildAnimatedImageProxyURL preserves the proxy's existing fit dimensions,
/// removes static encoding/quality choices, and explicitly requests animation.
/// Full-view requests omit thumbnail dimensions like buildViewerMediaURL.
pub(super) fn animated_proxy_url(raw: &str, full_size: bool) -> Option<String> {
    let mut url = Url::parse(&safe_image_url(raw)?).ok()?;
    let svg = url.path().to_ascii_lowercase().ends_with(".svg");
    let dimension = |key: &str| {
        url.query_pairs()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.)
            .map(|value| value.ceil().to_string())
    };
    let width = (!full_size && !svg).then(|| dimension("width")).flatten();
    let height = (!full_size && !svg).then(|| dimension("height")).flatten();
    let remaining = url
        .query_pairs()
        .filter(|(key, _)| {
            !matches!(
                key.as_ref(),
                "width" | "height" | "format" | "quality" | "animated"
            )
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !remaining.is_empty() || !svg {
        let mut query = url.query_pairs_mut();
        query.extend_pairs(remaining);
        if let Some(width) = width {
            query.append_pair("width", &width);
        }
        if let Some(height) = height {
            query.append_pair("height", &height);
        }
        if !svg {
            query.append_pair("animated", "true");
        }
    }
    Some(url.into())
}

pub(super) fn effective_gif_autoplay(settings: &Value, reduced_motion: bool) -> bool {
    gif_autoplay(settings) && !reduced_motion
}

pub(super) fn gif_autoplay(settings: &Value) -> bool {
    settings
        .get("gif_auto_play")
        .or_else(|| settings.get("gifAutoPlay"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_animation_flag_and_gif_mime_or_path_identify_animated_images() {
        assert!(is_animated_image(
            Some("image/GIF; charset=binary"),
            "opaque",
            0
        ));
        assert!(is_animated_image(
            None,
            "https://cdn.example/a.GIF?signature=x",
            0
        ));
        assert!(is_animated_image(Some("image/webp"), "opaque", 32));
        assert!(!is_animated_image(Some("image/png"), "still.png", 0));
        assert!(!is_animated_image(Some("video/mp4"), "clip.mp4", 0));
    }

    #[test]
    fn animated_proxy_preserves_signed_fields_and_fit_but_replaces_static_transforms() {
        let raw = "https://proxy.example/a.gif?signature=a%2Fb&width=400.1&height=300&format=png&quality=low&animated=false";
        assert_eq!(
            animated_proxy_url(raw, false).as_deref(),
            Some("https://proxy.example/a.gif?signature=a%2Fb&width=401&height=300&animated=true")
        );
        assert_eq!(
            animated_proxy_url(raw, true).as_deref(),
            Some("https://proxy.example/a.gif?signature=a%2Fb&animated=true")
        );
        assert_eq!(
            animated_proxy_url("https://proxy.example/a?width=NaN&height=-4", false).as_deref(),
            Some("https://proxy.example/a?animated=true")
        );
        for bad in [
            "file:///tmp/a.gif",
            "https://user:pass@host/a.gif",
            "https://host/a\ngif",
        ] {
            assert!(animated_proxy_url(bad, false).is_none());
        }
    }

    #[test]
    fn reduced_motion_prevents_automatic_playback_without_changing_account_preference() {
        let settings = json!({"gif_auto_play":true});
        assert!(!effective_gif_autoplay(&settings,true));
        assert!(effective_gif_autoplay(&settings,false));
        assert!(!effective_gif_autoplay(&json!({"gif_auto_play":false}),false));
    }
    #[test]
    fn authoritative_autoplay_false_is_preserved_and_missing_setting_uses_source_default() {
        assert!(!gif_autoplay(&json!({"gif_auto_play":false})));
        assert!(!gif_autoplay(&json!({"gifAutoPlay":false})));
        assert!(gif_autoplay(&json!({"gif_auto_play":true})));
        assert!(gif_autoplay(&Value::Null));
    }
}
