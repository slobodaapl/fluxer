// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure message-to-player contracts derived from the web renderer. No network,
//! credentials, guessed CDN routes, or native player handles belong here.
//!
//! Authority: MediaViewerItemUtils.ts (`determineMediaType`, `attachmentToViewerItem`,
//! `buildViewerMediaURL`), MediaProxyUtils.ts (`stripMediaProxyParams`),
//! ChannelEmbed.tsx, channel_embed/EmbedMediaRenderer.tsx and media/Embed{Audio,Video,Gifv}.tsx.
use fluxer_core::Attachment;
use reqwest::Url;
use serde_json::Value;
use std::hash::{DefaultHasher, Hash, Hasher};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlayableKind {
    Audio,
    Video,
    Gifv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Concealment {
    Spoiler,
    Sensitive,
}

/// `source` is a media resource, never an embed's containing webpage. A consumer
/// must gate loading/playback on `concealment` and its scoped reveal state.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PlayableMedia {
    pub kind: PlayableKind,
    pub source: String,
    /// Safe media original for download/open. Never the article/provider page.
    pub original: Option<String>,
    pub poster: Option<String>,
    pub title: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration: Option<f64>,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    pub concealment: Option<Concealment>,
    /// Matches rich_message's reveal namespace, including forward snapshot scope.
    pub reveal_key: String,
    /// Resource revision as well as message scope; replacing a URL cannot reuse
    /// an old decoder. Signed resource URLs are not exposed in diagnostic IDs.
    pub resource_key: String,
}

pub(super) fn safe_media_url(raw: &str) -> Option<String> {
    if raw.chars().any(char::is_control) {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url.into())
}

pub(super) fn normalized_mime(mime: Option<&str>) -> Option<String> {
    mime.and_then(|mime| {
        let mime = mime.split(';').next()?.trim().to_ascii_lowercase();
        (!mime.is_empty()).then_some(mime)
    })
}

/// Source classification puts audio first, then VIDEO + IS_ANIMATED before
/// ordinary animated images. Flag 32 alone never turns an image into a video.
/// The source does not infer playable audio/video from a filename extension.
pub(super) fn attachment_kind(attachment: &Attachment) -> Option<PlayableKind> {
    let mime = normalized_mime(attachment.content_type.as_deref())?;
    if mime.starts_with("audio/") {
        Some(PlayableKind::Audio)
    } else if mime.starts_with("video/") {
        Some(if attachment.flags & 32 != 0 {
            PlayableKind::Gifv
        } else {
            PlayableKind::Video
        })
    } else {
        None
    }
}

fn expired(value: &Value, now_ms: i64) -> bool {
    value.get("expired").and_then(Value::as_bool) == Some(true)
        || value
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            .is_some_and(|time| time.timestamp_millis() <= now_ms)
}

fn concealment(value: &Value) -> Option<Concealment> {
    let flags = value.get("flags").and_then(Value::as_u64).unwrap_or(0);
    if flags & 8 != 0
        || value.get("spoiler").and_then(Value::as_bool) == Some(true)
        || value
            .get("filename")
            .and_then(Value::as_str)
            .is_some_and(|name| name.starts_with("SPOILER_"))
    {
        Some(Concealment::Spoiler)
    } else if flags & 16 != 0 || value.get("nsfw").and_then(Value::as_bool) == Some(true) {
        Some(Concealment::Sensitive)
    } else {
        None
    }
}

fn combine_concealment(a: Option<Concealment>, b: Option<Concealment>) -> Option<Concealment> {
    match (a, b) {
        (Some(Concealment::Spoiler), _) | (_, Some(Concealment::Spoiler)) => {
            Some(Concealment::Spoiler)
        }
        (Some(Concealment::Sensitive), _) | (_, Some(Concealment::Sensitive)) => {
            Some(Concealment::Sensitive)
        }
        _ => None,
    }
}

/// Source viewer strips image transforms for a playable media resource. Keep
/// every other raw query token byte-for-byte, including signed URL fields.
pub(super) fn playback_proxy_url(raw: &str) -> Option<String> {
    let mut url = Url::parse(&safe_media_url(raw)?).ok()?;
    if let Some(query) = url.query() {
        let query = query
            .split('&')
            .filter(|part| {
                let name = part.split('=').next().unwrap_or_default();
                !is_proxy_transform(name)
            })
            .collect::<Vec<_>>()
            .join("&");
        url.set_query((!query.is_empty()).then_some(query.as_str()));
    }
    Some(url.into())
}

// URLSearchParams matches decoded names, while signed values must retain their
// original encoding. Decode only the parameter name for this comparison.
fn is_proxy_transform(raw_name: &str) -> bool {
    let mut decoded = Vec::with_capacity(raw_name.len());
    let bytes = raw_name.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = |byte: u8| (byte as char).to_digit(16).map(|n| n as u8);
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                decoded.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        decoded.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    matches!(
        decoded.as_slice(),
        b"width" | b"height" | b"format" | b"quality" | b"animated"
    )
}

fn poster_url(raw: &str) -> Option<String> {
    let mut url = Url::parse(&playback_proxy_url(raw)?).ok()?;
    let query = url.query().unwrap_or_default();
    let query = if query.is_empty() {
        "format=webp".into()
    } else {
        format!("{query}&format=webp")
    };
    url.set_query(Some(&query));
    Some(url.into())
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}
fn dimension(value: &Value, key: &str) -> Option<u32> {
    value.get(key)?.as_u64()?.try_into().ok().filter(|n| *n > 0)
}
fn duration(value: &Value) -> Option<f64> {
    value
        .get("duration")?
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.)
}
fn resource_key(reveal_key: &str, kind: PlayableKind, source: &str) -> String {
    let mut hash = DefaultHasher::new();
    source.hash(&mut hash);
    format!("{reveal_key}:{kind:?}:{:016x}", hash.finish())
}

pub(super) fn attachment(
    attachment: &Attachment,
    namespace: &str,
    now_ms: i64,
) -> Option<PlayableMedia> {
    let kind = attachment_kind(attachment)?;
    let value = serde_json::to_value(attachment).ok()?;
    if expired(&value, now_ms) {
        return None;
    }
    let original = attachment.url.as_deref().and_then(safe_media_url);
    let proxy = text(&value, "proxy_url").and_then(|raw| playback_proxy_url(&raw));
    let source = proxy.clone().or_else(|| original.clone())?;
    let reveal_key = format!(
        "attachment:{}:{namespace}:{}",
        namespace.len(),
        attachment.id
    );
    Some(PlayableMedia {
        kind,
        resource_key: resource_key(&reveal_key, kind, &source),
        source,
        original,
        poster: (kind != PlayableKind::Audio)
            .then(|| proxy.as_deref().and_then(poster_url))
            .flatten(),
        title: text(&value, "title").unwrap_or_else(|| attachment.filename.clone()),
        width: attachment.width.filter(|n| *n > 0),
        height: attachment.height.filter(|n| *n > 0),
        duration: duration(&value),
        size: Some(attachment.size),
        content_type: normalized_mime(attachment.content_type.as_deref()),
        concealment: concealment(&value),
        reveal_key,
    })
}

pub(super) fn embeds_suppressed(message_flags: u32) -> bool {
    message_flags & 4 != 0
}

/// `spoilered` comes from the renderer's canonical source URL/spoiler matching.
/// Caller excludes message flags SUPPRESS_EMBEDS before calling. Rich embeds
/// may contain an actual video too (EmbedMediaRenderer), but URLs alone never
/// establish playability: only the declared audio/video media object does.
pub(super) fn embed(
    embed: &Value,
    namespace: &str,
    index: usize,
    spoilered: bool,
    now_ms: i64,
) -> Option<PlayableMedia> {
    if expired(embed, now_ms) || embed.get("suppressed").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    // YouTube is rendered by EmbedYouTube, not a native file player.
    let youtube = |raw: &str| {
        Url::parse(raw)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .is_some_and(|host| {
                host == "youtu.be"
                    || host == "youtube.com"
                    || host.ends_with(".youtube.com")
                    || host == "youtube-nocookie.com"
                    || host.ends_with(".youtube-nocookie.com")
            })
    };
    if embed
        .pointer("/provider/url")
        .and_then(Value::as_str)
        .is_some_and(youtube)
    {
        return None;
    }
    let embed_type = embed
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (kind, media) = if embed_type == "audio" {
        (PlayableKind::Audio, embed.get("audio")?)
    } else {
        let video = embed.get("video").filter(|video| video.is_object())?;
        (
            if embed_type == "gifv" {
                PlayableKind::Gifv
            } else {
                PlayableKind::Video
            },
            video,
        )
    };
    if expired(media, now_ms) {
        return None;
    }
    let raw_proxy = media.get("proxy_url")?.as_str()?;
    // Missing proxies never fall through to embed.url, video.url, or a guessed
    // API/CDN route. The web renderer's embed players require proxy_url.
    let source = playback_proxy_url(raw_proxy)?;
    if youtube(&source) {
        return None;
    }
    let content_type = normalized_mime(media.get("content_type").and_then(Value::as_str));
    if content_type.as_deref().is_some_and(|mime| {
        if kind == PlayableKind::Audio {
            !mime.starts_with("audio/")
        } else {
            !mime.starts_with("video/")
        }
    }) {
        return None;
    }
    let thumbnail = embed.get("thumbnail");
    if kind == PlayableKind::Gifv && thumbnail.is_some_and(|thumbnail| expired(thumbnail, now_ms)) {
        return None;
    }
    let poster_source = if kind == PlayableKind::Gifv {
        embed
            .get("thumbnail")
            .and_then(|value| value.get("proxy_url"))
            .and_then(Value::as_str)
            .unwrap_or(raw_proxy)
    } else {
        raw_proxy
    };
    let mut hidden = combine_concealment(concealment(embed), concealment(media));
    if kind == PlayableKind::Gifv {
        hidden = combine_concealment(hidden, thumbnail.and_then(concealment));
    }
    if spoilered {
        hidden = Some(Concealment::Spoiler);
    }
    let reveal_key = format!("embed:{}:{namespace}:{index}", namespace.len());
    Some(PlayableMedia {
        kind,
        resource_key: resource_key(&reveal_key, kind, &source),
        source,
        original: media
            .get("url")
            .and_then(Value::as_str)
            .and_then(safe_media_url),
        poster: (kind != PlayableKind::Audio)
            .then(|| poster_url(poster_source))
            .flatten(),
        title: text(embed, "title")
            .or_else(|| text(media, "description"))
            .unwrap_or_else(|| {
                match kind {
                    PlayableKind::Audio => "Audio",
                    PlayableKind::Video => "Video",
                    PlayableKind::Gifv => "GIF",
                }
                .into()
            }),
        width: dimension(media, "width"),
        height: dimension(media, "height"),
        duration: duration(media),
        size: media.get("size").and_then(Value::as_u64),
        content_type,
        concealment: hidden,
        reveal_key,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PlaybackDefaults {
    pub autoplay: bool,
    pub looping: bool,
    pub muted: bool,
}
impl PlayableKind {
    /// EmbedAudio/useMediaPlayer and desktop VideoPlayer start on user action;
    /// EmbedGifv defaults to autoplay under its gates, looping and muted.
    pub fn defaults(self) -> PlaybackDefaults {
        let gifv = self == Self::Gifv;
        PlaybackDefaults {
            autoplay: gifv,
            looping: gifv,
            muted: gifv,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct GifvPlaybackSignals {
    pub near_viewport: bool,
    pub in_viewport: bool,
    pub revealed: bool,
    pub document_visible: bool,
    pub window_focused_or_interactive: bool,
    pub viewer_open: bool,
    pub poster_revealed: bool,
    pub autoplay: bool,
    pub interacting: bool,
    pub reduced_motion: bool,
    pub keep_under_reduced_motion: bool,
    pub save_data: bool,
}

/// GifViewportGate + useShouldAnimate(kind=gif) +
/// useAnimatedMediaPlaybackAllowed + EmbedGifv's shouldPlay expression.
pub(super) fn gifv_should_play(s: GifvPlaybackSignals) -> bool {
    s.near_viewport
        && s.in_viewport
        && s.revealed
        && s.document_visible
        && s.window_focused_or_interactive
        && !s.viewer_open
        && s.poster_revealed
        && !s.save_data
        && (s.interacting || s.autoplay && (!s.reduced_motion || s.keep_under_reduced_motion))
}

/// useHiddenPlaybackGuard holds an attempted video play while already hidden,
/// except picture-in-picture. Audio has no such guard; normal audio/video are
/// not paused merely because their message scrolls out of view or loses focus.
/// The source's visibilitychange listener only releases an existing hold when
/// visible, rather than pausing a video that was playing before it was hidden.
pub(super) fn hold_play_started_hidden(
    kind: PlayableKind,
    document_visible: bool,
    picture_in_picture: bool,
) -> bool {
    kind == PlayableKind::Video && !document_visible && !picture_in_picture
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const NOW: i64 = 1_700_000_000_000;

    fn attachment_value(value: Value) -> Attachment {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn viewer_classification_orders_audio_and_animated_video_before_images() {
        for (mime, flags, expected) in [
            (Some("audio/ogg"), 32, Some(PlayableKind::Audio)),
            (Some("VIDEO/MP4; codecs=avc1"), 32, Some(PlayableKind::Gifv)),
            (Some("video/webm"), 0, Some(PlayableKind::Video)),
            (Some("image/gif"), 32, None),
            (Some("image/webp"), 32, None),
            (Some("application/octet-stream"), 0, None),
            (None, 0, None),
        ] {
            let attachment = Attachment {
                filename: "clip.mp4".into(),
                content_type: mime.map(str::to_owned),
                flags,
                ..Attachment::default()
            };
            assert_eq!(attachment_kind(&attachment), expected, "{mime:?}/{flags}");
        }
    }

    #[test]
    fn proxy_playback_removes_image_transforms_and_preserves_signed_original_metadata() {
        let item = attachment(&attachment_value(json!({
            "id":"7", "filename":"track.ogg", "title":"A recording", "content_type":"Audio/Ogg; codecs=opus",
            "url":"https://cdn.example/track.ogg?signature=a%2Fb%20c&format=source",
            "proxy_url":"https://proxy.example/track.ogg?width=320&signature=a%2Fb%20c&format=webp&quality=low&height=200&animated=false",
            "size":4096, "duration":12.5
        })), "100", NOW).unwrap();
        assert_eq!(item.kind, PlayableKind::Audio);
        assert_eq!(
            item.source,
            "https://proxy.example/track.ogg?signature=a%2Fb%20c"
        );
        assert_eq!(
            item.original.as_deref(),
            Some("https://cdn.example/track.ogg?signature=a%2Fb%20c&format=source")
        );
        assert_eq!(item.title, "A recording");
        assert_eq!(item.duration, Some(12.5));
        assert_eq!(item.size, Some(4096));
        assert_eq!(item.content_type.as_deref(), Some("audio/ogg"));
        assert_eq!(item.poster, None);
        assert!(!item.resource_key.contains("signature"));
        assert_eq!(
            playback_proxy_url(
                "https://proxy.example/v?%77idth=400&sig=one%20two&format=webp&format=png&WiDtH=5"
            )
            .as_deref(),
            Some("https://proxy.example/v?sig=one%20two&WiDtH=5")
        );
    }

    #[test]
    fn concealment_expiry_and_resource_revision_are_independent_of_reveal_state() {
        let value = json!({"id":"7", "filename":"SPOILER_secret.mp4", "flags":48,
            "content_type":"video/mp4", "url":"https://cdn.example/secret.mp4",
            "proxy_url":"https://proxy.example/secret.mp4", "width":640, "height":360, "duration":-1});
        let hidden = attachment(&attachment_value(value.clone()), "100", NOW).unwrap();
        assert_eq!(hidden.kind, PlayableKind::Gifv);
        assert_eq!(hidden.concealment, Some(Concealment::Spoiler));
        assert_eq!(
            hidden.poster.as_deref(),
            Some("https://proxy.example/secret.mp4?format=webp")
        );
        assert_eq!((hidden.width, hidden.height), (Some(640), Some(360)));
        assert_eq!(hidden.duration, None);
        let forwarded =
            attachment(&attachment_value(value.clone()), "100-snapshot-0", NOW).unwrap();
        assert_ne!(forwarded.reveal_key, hidden.reveal_key);
        assert_ne!(forwarded.resource_key, hidden.resource_key);
        let mut changed = value.clone();
        changed["proxy_url"] = json!("https://proxy.example/replacement.mp4");
        let changed = attachment(&attachment_value(changed), "100", NOW).unwrap();
        assert_eq!(changed.reveal_key, hidden.reveal_key);
        assert_ne!(changed.resource_key, hidden.resource_key);
        for patch in [
            json!({"expired":true}),
            json!({"expires_at":"2000-01-01T00:00:00Z"}),
        ] {
            let mut expired = value.clone();
            expired
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(attachment(&attachment_value(expired), "100", NOW).is_none());
        }
    }

    #[test]
    fn provider_pages_and_missing_proxies_never_become_playback_sources() {
        for value in [
            json!({"type":"video", "url":"https://article.example/post"}),
            json!({"type":"video", "url":"https://article.example/post", "video":{"url":"https://cdn.example/clip.mp4"}}),
            json!({"type":"video", "provider":{"url":"https://www.youtube.com"}, "video":{"proxy_url":"https://proxy.example/iframe"}}),
            json!({"type":"video", "video":{"proxy_url":"https://www.youtube.com/embed/abc"}}),
            json!({"type":"video", "video":{"proxy_url":"https://proxy.example/iframe", "content_type":"text/html"}}),
            json!({"type":"audio", "audio":{"proxy_url":"file:///tmp/track.ogg"}}),
        ] {
            assert!(embed(&value, "100", 0, false, NOW).is_none(), "{value}");
        }
        for raw in [
            "file:///tmp/clip",
            "blob:https://host/id",
            "data:video/mp4,abc",
            "https://user:pass@host/video",
            "https://host/clip\n.mp4",
        ] {
            assert!(safe_media_url(raw).is_none(), "{raw}");
        }
    }

    #[test]
    fn media_embed_fixtures_use_the_media_original_and_forward_scoped_identity() {
        for (value, kind) in [
            (
                json!({"type":"audio", "title":"Track", "url":"https://article.example/audio", "audio":{"url":"https://cdn.example/a.ogg?sig=x", "proxy_url":"https://proxy.example/a.ogg", "duration":4}}),
                PlayableKind::Audio,
            ),
            (
                json!({"type":"video", "title":"Clip", "url":"https://article.example/video", "video":{"url":"https://cdn.example/v.webm?sig=x", "proxy_url":"https://proxy.example/v.webm", "width":1280,"height":720}}),
                PlayableKind::Video,
            ),
            (
                json!({"type":"gifv", "url":"https://gifs.example/page", "video":{"url":"https://cdn.example/g.mp4?sig=x", "proxy_url":"https://proxy.example/g.mp4"}, "thumbnail":{"proxy_url":"https://proxy.example/thumb.webp", "flags":16}}),
                PlayableKind::Gifv,
            ),
            (
                json!({"type":"rich", "title":"Article", "video":{"url":"https://cdn.example/r.mp4?sig=x", "proxy_url":"https://proxy.example/r.mp4"}}),
                PlayableKind::Video,
            ),
        ] {
            let item = embed(&value, "100", 2, false, NOW).unwrap();
            assert_eq!(item.kind, kind);
            assert!(
                item.original
                    .as_ref()
                    .unwrap()
                    .starts_with("https://cdn.example/")
            );
            let forwarded = embed(&value, "100-snapshot-0", 2, false, NOW).unwrap();
            assert_ne!(item.reveal_key, forwarded.reveal_key);
            let spoilered = embed(&value, "100", 2, true, NOW).unwrap();
            assert_eq!(spoilered.concealment, Some(Concealment::Spoiler));
            if kind == PlayableKind::Gifv {
                assert_eq!(item.concealment, Some(Concealment::Sensitive));
                assert_eq!(
                    item.poster.as_deref(),
                    Some("https://proxy.example/thumb.webp?format=webp")
                );
            }
        }
        assert!(embeds_suppressed(4));
        assert!(!embeds_suppressed(0));
        assert!(embed(&json!({"type":"video","expired":true,"video":{"proxy_url":"https://proxy.example/v"}}), "100", 0, false, NOW).is_none());
        assert!(embed(&json!({"type":"video","video":{"expired":true,"proxy_url":"https://proxy.example/v"}}), "100", 0, false, NOW).is_none());
    }

    #[test]
    fn animated_video_policy_has_each_source_gate_and_hover_override() {
        let allowed = GifvPlaybackSignals {
            near_viewport: true,
            in_viewport: true,
            revealed: true,
            document_visible: true,
            window_focused_or_interactive: true,
            viewer_open: false,
            poster_revealed: true,
            autoplay: true,
            interacting: false,
            reduced_motion: false,
            keep_under_reduced_motion: false,
            save_data: false,
        };
        assert!(gifv_should_play(allowed));
        for blocked in [
            GifvPlaybackSignals {
                near_viewport: false,
                ..allowed
            },
            GifvPlaybackSignals {
                in_viewport: false,
                ..allowed
            },
            GifvPlaybackSignals {
                revealed: false,
                ..allowed
            },
            GifvPlaybackSignals {
                document_visible: false,
                ..allowed
            },
            GifvPlaybackSignals {
                window_focused_or_interactive: false,
                ..allowed
            },
            GifvPlaybackSignals {
                viewer_open: true,
                ..allowed
            },
            GifvPlaybackSignals {
                poster_revealed: false,
                ..allowed
            },
            GifvPlaybackSignals {
                autoplay: false,
                ..allowed
            },
            GifvPlaybackSignals {
                reduced_motion: true,
                ..allowed
            },
            GifvPlaybackSignals {
                save_data: true,
                interacting: true,
                ..allowed
            },
        ] {
            assert!(!gifv_should_play(blocked), "{blocked:?}");
        }
        assert!(gifv_should_play(GifvPlaybackSignals {
            autoplay: false,
            interacting: true,
            reduced_motion: true,
            ..allowed
        }));
        assert!(gifv_should_play(GifvPlaybackSignals {
            reduced_motion: true,
            keep_under_reduced_motion: true,
            ..allowed
        }));
        assert_eq!(
            PlayableKind::Gifv.defaults(),
            PlaybackDefaults {
                autoplay: true,
                looping: true,
                muted: true
            }
        );
        for kind in [PlayableKind::Audio, PlayableKind::Video] {
            assert_eq!(
                kind.defaults(),
                PlaybackDefaults {
                    autoplay: false,
                    looping: false,
                    muted: false
                }
            );
        }
        assert!(!hold_play_started_hidden(PlayableKind::Audio, false, false));
        assert!(hold_play_started_hidden(PlayableKind::Video, false, false));
        assert!(!hold_play_started_hidden(PlayableKind::Video, false, true));
    }
}
