// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure clicked-media policy. Authority: MediaContextMenu.tsx, MediaMenuData.tsx,
//! MediaDownloadPolicy.ts and ForumChannelUtils.ts. Message actions stay with
//! the existing message menu; this module never sends a request or changes data.
use crate::playable_media::{self, Concealment, PlayableKind};
use fluxer_core::{Attachment, Channel, ChannelType, Message, Snowflake};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    hash::{DefaultHasher, Hash, Hasher},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MediaAsset {
    Attachment(Snowflake),
    Embed(usize),
    /// The legacy browser-video card is a separate asset from its thumbnail.
    EmbedVideo(usize),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MediaTarget {
    pub snapshot_index: Option<usize>,
    pub asset: MediaAsset,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MediaKind {
    Image,
    Gif,
    Gifv,
    Video,
    Audio,
    File,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MediaAccess {
    Available,
    Concealed,
    Expired,
    HiddenByChannel,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MediaContext {
    pub target: MediaTarget,
    pub kind: MediaKind,
    pub filename: String,
    pub attachment_id: Option<Snowflake>,
    /// Copy/Open use this media original, never the containing article webpage.
    pub original_url: Option<String>,
    /// Save prefers a supplied proxy stripped of image transformation parameters.
    pub download_url: Option<String>,
    /// A bitmap copy may fetch this full-size resource. GIF/file/audio/video
    /// clipboard-file fallbacks copy original_url instead (classic desktop).
    pub image_source: Option<String>,
    pub concealment: Option<Concealment>,
    pub reveal_key: String,
    pub resource_key: String,
    pub access: MediaAccess,
}
impl MediaContext {
    pub fn available(&self) -> bool {
        self.access == MediaAccess::Available
    }
    pub fn can_copy_image(&self) -> bool {
        self.available() && self.kind == MediaKind::Image && self.image_source.is_some()
    }
    /// File names from wire data are labels, not paths. Save dialogs should use
    /// this basename, then their user's explicitly selected destination.
    pub fn suggested_filename(&self) -> String {
        let name = self.filename.rsplit(['/', '\\']).next().unwrap_or_default();
        let name: String = name.chars().filter(|c| !c.is_control()).collect();
        let name = if name.trim().is_empty() || matches!(name.as_str(), "." | "..") {
            "download".into()
        } else {
            name
        };
        // FileDownloadUtils.deriveSuggestedName/applyDefaultExtension.
        let extension = match self.kind {
            MediaKind::Image => ".png",
            MediaKind::Gif => ".gif",
            MediaKind::Gifv | MediaKind::Video => ".mp4",
            MediaKind::Audio => ".mp3",
            MediaKind::File => "",
        };
        match name.rfind('.') {
            None => format!("{name}{extension}"),
            Some(dot)
                if self.kind == MediaKind::Gif && !name[dot..].eq_ignore_ascii_case(".gif") =>
            {
                format!("{}{extension}", &name[..dot])
            }
            _ => name,
        }
    }
}

pub(super) fn hide_media_downloads(
    channel: Snowflake,
    kind: MediaKind,
    channels: &BTreeMap<Snowflake, Channel>,
) -> bool {
    if !matches!(
        kind,
        MediaKind::Image | MediaKind::Gif | MediaKind::Gifv | MediaKind::Video
    ) {
        return false;
    }
    let Some(thread) = channels
        .get(&channel)
        .filter(|channel| channel.kind.is_thread())
    else {
        return false;
    };
    let Some(parent) = thread.parent_id.and_then(|id| channels.get(&id)) else {
        return false;
    };
    parent.kind == ChannelType::Media
        && parent
            .extra
            .get("flags")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            & (1 << 15)
            != 0
}

/// Caller supplies existing canonical spoiler URL matching for the clicked
/// embed. Re-resolve at activation to recheck current flags, expiry and reveal.
pub(super) fn resolve(
    message: &Message,
    target: &MediaTarget,
    channels: &BTreeMap<Snowflake, Channel>,
    revealed: &BTreeSet<String>,
    embed_spoilered: bool,
    now_ms: i64,
) -> Option<MediaContext> {
    let (selected, namespace) = selected_message(message, target.snapshot_index)?;
    let mut context = match target.asset {
        MediaAsset::Attachment(id) => {
            let attachment = selected
                .attachments
                .iter()
                .find(|attachment| attachment.id == id)?;
            attachment_context(attachment, &namespace, target.clone(), now_ms)
        }
        MediaAsset::EmbedVideo(index) => {
            if playable_media::embeds_suppressed(selected.flags) {
                return None;
            }
            let embed = selected.extra.get("embeds")?.as_array()?.get(index)?;
            embed_video_context(
                embed,
                &namespace,
                index,
                target.clone(),
                embed_spoilered,
                now_ms,
            )?
        }
        MediaAsset::Embed(index) => {
            if playable_media::embeds_suppressed(selected.flags) {
                return None;
            }
            let embed = selected.extra.get("embeds")?.as_array()?.get(index)?;
            embed_context(
                embed,
                &namespace,
                index,
                target.clone(),
                embed_spoilered,
                now_ms,
            )?
        }
    };
    context.access = if context.access == MediaAccess::Expired {
        MediaAccess::Expired
    } else if hide_media_downloads(message.channel_id, context.kind, channels) {
        MediaAccess::HiddenByChannel
    } else if context.concealment.is_some() && !revealed.contains(&context.reveal_key) {
        MediaAccess::Concealed
    } else if context.original_url.is_none() || context.download_url.is_none() {
        MediaAccess::Unavailable
    } else {
        MediaAccess::Available
    };
    if !context.available() {
        context.original_url = None;
        context.download_url = None;
        context.image_source = None;
    }
    Some(context)
}

pub(super) fn resolve_attachment(
    message: &Message,
    attachment_id: Snowflake,
    snapshot_index: Option<usize>,
    channels: &BTreeMap<Snowflake, Channel>,
    revealed: &BTreeSet<String>,
    now_ms: i64,
) -> Option<MediaContext> {
    resolve(
        message,
        &MediaTarget {
            snapshot_index,
            asset: MediaAsset::Attachment(attachment_id),
        },
        channels,
        revealed,
        false,
        now_ms,
    )
}
#[allow(clippy::too_many_arguments)] // Explicit clicked scope plus the canonical spoiler match.
pub(super) fn resolve_embed(
    message: &Message,
    embed_index: usize,
    snapshot_index: Option<usize>,
    channels: &BTreeMap<Snowflake, Channel>,
    revealed: &BTreeSet<String>,
    spoilered: bool,
    now_ms: i64,
) -> Option<MediaContext> {
    resolve(
        message,
        &MediaTarget {
            snapshot_index,
            asset: MediaAsset::Embed(embed_index),
        },
        channels,
        revealed,
        spoilered,
        now_ms,
    )
}

fn selected_message(
    message: &Message,
    snapshot: Option<usize>,
) -> Option<(Cow<'_, Message>, String)> {
    let Some(index) = snapshot else {
        return Some((Cow::Borrowed(message), message.id.to_string()));
    };
    let value = message
        .extra
        .get("message_snapshots")?
        .as_array()?
        .get(index)?;
    let mut selected = Message {
        id: message.id,
        channel_id: message.channel_id,
        flags: value
            .get("flags")
            .and_then(Value::as_u64)
            .and_then(|flags| flags.try_into().ok())
            .unwrap_or(0),
        attachments: value
            .get("attachments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|attachment| serde_json::from_value(attachment.clone()).ok())
            .collect(),
        ..Message::default()
    };
    if let Some(embeds) = value.get("embeds").and_then(Value::as_array) {
        selected
            .extra
            .insert("embeds".into(), Value::Array(embeds.clone()));
    }
    Some((
        Cow::Owned(selected),
        format!("{}-snapshot-{index}", message.id),
    ))
}
fn expired(value: &Value, now: i64) -> bool {
    value.get("expired").and_then(Value::as_bool) == Some(true)
        || value
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            .is_some_and(|time| time.timestamp_millis() <= now)
}
fn flags(value: &Value) -> u64 {
    value.get("flags").and_then(Value::as_u64).unwrap_or(0)
}
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|value| !value.is_empty())
}
fn hidden(value: &Value) -> Option<Concealment> {
    if flags(value) & 8 != 0
        || text(value, "filename").is_some_and(|name| name.starts_with("SPOILER_"))
    {
        Some(Concealment::Spoiler)
    } else if flags(value) & 16 != 0 || value.get("nsfw").and_then(Value::as_bool) == Some(true) {
        Some(Concealment::Sensitive)
    } else {
        None
    }
}
fn combine(a: Option<Concealment>, b: Option<Concealment>) -> Option<Concealment> {
    if a == Some(Concealment::Spoiler) || b == Some(Concealment::Spoiler) {
        Some(Concealment::Spoiler)
    } else {
        a.or(b)
    }
}
fn animated(value: &Value, fallback: &str) -> bool {
    crate::media_animation::is_animated_image(
        text(value, "content_type"),
        text(value, "url").unwrap_or(fallback),
        flags(value),
    )
}
fn playable_kind(kind: PlayableKind) -> MediaKind {
    match kind {
        PlayableKind::Audio => MediaKind::Audio,
        PlayableKind::Video => MediaKind::Video,
        PlayableKind::Gifv => MediaKind::Gifv,
    }
}
fn revision(key: &str, original: Option<&str>, download: Option<&str>) -> String {
    let mut hash = DefaultHasher::new();
    original.hash(&mut hash);
    download.hash(&mut hash);
    format!("{key}:media:{:016x}", hash.finish())
}
fn attachment_context(
    attachment: &Attachment,
    namespace: &str,
    target: MediaTarget,
    now: i64,
) -> MediaContext {
    let value = serde_json::to_value(attachment).unwrap_or_default();
    let playable = playable_media::attachment(attachment, namespace, now);
    let mime =
        playable_media::normalized_mime(attachment.content_type.as_deref()).unwrap_or_default();
    let kind = playable_media::attachment_kind(attachment)
        .map(playable_kind)
        .unwrap_or_else(|| {
            if animated(&value, &attachment.filename) {
                MediaKind::Gif
            } else if mime.starts_with("image/") {
                MediaKind::Image
            } else {
                MediaKind::File
            }
        });
    let original_url = attachment
        .url
        .as_deref()
        .and_then(playable_media::safe_media_url);
    let download_url = text(&value, "proxy_url")
        .and_then(playable_media::playback_proxy_url)
        .or_else(|| original_url.clone());
    let reveal_key = format!(
        "attachment:{}:{namespace}:{}",
        namespace.len(),
        attachment.id
    );
    let resource_key = playable
        .map(|playable| playable.resource_key)
        .unwrap_or_else(|| {
            revision(
                &reveal_key,
                original_url.as_deref(),
                download_url.as_deref(),
            )
        });
    MediaContext {
        target,
        kind,
        filename: attachment.filename.clone(),
        attachment_id: Some(attachment.id),
        image_source: (kind == MediaKind::Image)
            .then(|| download_url.clone())
            .flatten(),
        original_url,
        download_url,
        concealment: hidden(&value),
        reveal_key,
        resource_key,
        access: if expired(&value, now) {
            MediaAccess::Expired
        } else {
            MediaAccess::Available
        },
    }
}

fn selected_image(embed: &Value, now: i64) -> Option<(&Value, String)> {
    let keys = if text(embed, "type") == Some("image")
        && embed
            .get("image")
            .is_none_or(|image| hidden(image).is_none() && !expired(image, now))
        && embed
            .get("thumbnail")
            .is_some_and(|image| flags(image) & 32 != 0)
    {
        ["thumbnail", "image"]
    } else {
        ["image", "thumbnail"]
    };
    for image in keys.into_iter().filter_map(|key| embed.get(key)) {
        if expired(image, now) {
            return Some((image, String::new()));
        }
        if let Some(proxy) = text(image, "proxy_url").and_then(playable_media::safe_media_url) {
            return Some((image, proxy));
        }
        if hidden(image).is_some() {
            return Some((image, String::new()));
        }
    }
    None
}
fn filename_from_url(raw: &str) -> Option<String> {
    let url = reqwest::Url::parse(raw).ok()?;
    let name = url.path_segments()?.next_back()?;
    if name.is_empty() {
        None
    } else {
        Some(decode_path_segment(name).unwrap_or_else(|| name.into()))
    }
}
fn decode_path_segment(raw: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = (*bytes.get(index + 1)? as char).to_digit(16)?;
            let low = (*bytes.get(index + 2)? as char).to_digit(16)?;
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}
fn embed_concealment(embed: &Value, spoilered: bool) -> Option<Concealment> {
    let mut concealment = hidden(embed);
    for media in ["image", "thumbnail", "video", "audio"]
        .into_iter()
        .filter_map(|key| embed.get(key))
    {
        concealment = combine(concealment, hidden(media));
    }
    if spoilered {
        Some(Concealment::Spoiler)
    } else {
        concealment
    }
}

/// A browser fallback is not native playability: it uses only the declared
/// video resource, even when there is no usable video proxy. The containing
/// article URL and sibling thumbnail can never replace this clicked asset.
fn embed_video_context(
    embed: &Value,
    namespace: &str,
    index: usize,
    target: MediaTarget,
    spoilered: bool,
    now: i64,
) -> Option<MediaContext> {
    if embed.get("suppressed").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let video = embed.get("video")?.as_object()?;
    let video = Value::Object(video.clone());
    let proxy = text(&video, "proxy_url").and_then(playable_media::playback_proxy_url);
    let original_url = text(&video, "url")
        .and_then(playable_media::safe_media_url)
        .or_else(|| proxy.clone());
    let download_url = proxy.or_else(|| original_url.clone());
    let reveal_key = format!("embed:{}:{namespace}:{index}", namespace.len());
    let resource_key = revision(
        &format!("{reveal_key}:video"),
        original_url.as_deref(),
        download_url.as_deref(),
    );
    let filename = original_url
        .as_deref()
        .and_then(filename_from_url)
        .or_else(|| text(&video, "description").map(str::to_owned))
        .unwrap_or_else(|| "Video".into());
    Some(MediaContext {
        target,
        kind: if text(embed, "type") == Some("gifv") {
            MediaKind::Gifv
        } else {
            MediaKind::Video
        },
        filename,
        attachment_id: None,
        original_url,
        download_url,
        image_source: None,
        concealment: embed_concealment(embed, spoilered),
        reveal_key,
        resource_key,
        access: if expired(embed, now) || expired(&video, now) {
            MediaAccess::Expired
        } else {
            MediaAccess::Available
        },
    })
}

fn embed_context(
    embed: &Value,
    namespace: &str,
    index: usize,
    target: MediaTarget,
    spoilered: bool,
    now: i64,
) -> Option<MediaContext> {
    if embed.get("suppressed").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let concealment = embed_concealment(embed, spoilered);
    let reveal_key = format!("embed:{}:{namespace}:{index}", namespace.len());
    if let Some(playable) = playable_media::embed(embed, namespace, index, spoilered, now) {
        let kind = playable_kind(playable.kind);
        let original_url = playable.original;
        let download_url = Some(playable.source);
        let filename = original_url
            .as_deref()
            .and_then(filename_from_url)
            .unwrap_or(playable.title);
        return Some(MediaContext {
            target,
            kind,
            filename,
            attachment_id: None,
            original_url,
            download_url,
            image_source: None,
            concealment,
            reveal_key,
            resource_key: playable.resource_key,
            access: MediaAccess::Available,
        });
    }
    // A provider webpage/iframe never becomes native media. Only the actual
    // image selected by the renderer may supply copy/save/open image actions.
    let (image, proxy) = selected_image(embed, now)?;
    let kind = if animated(image, &proxy) {
        MediaKind::Gif
    } else {
        MediaKind::Image
    };
    let download_url = playable_media::playback_proxy_url(&proxy);
    let original_url = text(image, "url")
        .and_then(playable_media::safe_media_url)
        .or_else(|| download_url.clone());
    let filename = original_url
        .as_deref()
        .and_then(filename_from_url)
        .or_else(|| text(image, "description").map(str::to_owned))
        .or_else(|| text(embed, "title").map(str::to_owned))
        .unwrap_or_else(|| "Image".into());
    let resource_key = revision(
        &reveal_key,
        original_url.as_deref(),
        download_url.as_deref(),
    );
    Some(MediaContext {
        target,
        kind,
        filename,
        attachment_id: None,
        image_source: (kind == MediaKind::Image)
            .then(|| download_url.clone())
            .flatten(),
        original_url,
        download_url,
        concealment,
        reveal_key,
        resource_key,
        access: if expired(embed, now) || expired(image, now) {
            MediaAccess::Expired
        } else {
            MediaAccess::Available
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const NOW: i64 = 1_700_000_000_000;
    fn message(attachments: Value, embeds: Value) -> Message {
        serde_json::from_value(json!({"id":"100","channel_id":"20","author":{"id":"1","username":"A"},"attachments":attachments,"embeds":embeds})).unwrap()
    }
    fn image() -> Value {
        json!({"id":"7","filename":"drawing.png","content_type":"IMAGE/PNG; charset=binary","url":"https://cdn.example/original.png?sig=a%2Fb%20c&format=source","proxy_url":"https://proxy.example/drawing.png?width=400&sig=a%2Fb%20c&format=webp&height=300&animated=false"})
    }
    fn channels(parent_type: ChannelType, parent_flags: u64) -> BTreeMap<Snowflake, Channel> {
        BTreeMap::from([
            (
                Snowflake(20),
                Channel {
                    id: Snowflake(20),
                    kind: ChannelType::PublicThread,
                    parent_id: Some(Snowflake(10)),
                    ..Channel::default()
                },
            ),
            (
                Snowflake(10),
                Channel {
                    id: Snowflake(10),
                    kind: parent_type,
                    extra: BTreeMap::from([("flags".into(), json!(parent_flags))]),
                    ..Channel::default()
                },
            ),
        ])
    }
    #[test]
    fn original_link_and_save_proxy_have_distinct_signed_url_contracts() {
        let message = message(json!([image()]), json!([]));
        let context = resolve_attachment(
            &message,
            Snowflake(7),
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            NOW,
        )
        .unwrap();
        assert_eq!(context.kind, MediaKind::Image);
        assert!(context.available());
        assert!(context.can_copy_image());
        assert_eq!(
            context.original_url.as_deref(),
            Some("https://cdn.example/original.png?sig=a%2Fb%20c&format=source")
        );
        assert_eq!(
            context.download_url.as_deref(),
            Some("https://proxy.example/drawing.png?sig=a%2Fb%20c")
        );
        assert_eq!(context.image_source, context.download_url);
        assert_eq!(context.filename, "drawing.png");
        assert_eq!(context.attachment_id, Some(Snowflake(7)));
        assert!(!context.resource_key.contains("sig="));
    }
    #[test]
    fn spoilers_and_mature_media_require_the_exact_reveal_scope_even_for_links() {
        for patch in [
            json!({"filename":"SPOILER_drawing.png"}),
            json!({"flags":8}),
            json!({"flags":16}),
            json!({"nsfw":true}),
        ] {
            let mut image = image();
            image
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            let message = message(json!([image]), json!([]));
            let hidden = resolve_attachment(
                &message,
                Snowflake(7),
                None,
                &BTreeMap::new(),
                &BTreeSet::new(),
                NOW,
            )
            .unwrap();
            assert_eq!(hidden.access, MediaAccess::Concealed);
            assert_eq!(
                (
                    hidden.original_url,
                    hidden.download_url,
                    hidden.image_source
                ),
                (None, None, None)
            );
            let revealed = BTreeSet::from([hidden.reveal_key]);
            assert!(
                resolve_attachment(
                    &message,
                    Snowflake(7),
                    None,
                    &BTreeMap::new(),
                    &revealed,
                    NOW
                )
                .unwrap()
                .available()
            );
        }
    }
    #[test]
    fn expired_and_unsafe_sources_cannot_be_copied_saved_or_opened() {
        for patch in [
            json!({"expired":true}),
            json!({"expires_at":"2000-01-01T00:00:00Z"}),
        ] {
            let mut image = image();
            image
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            let message = message(json!([image]), json!([]));
            let context = resolve_attachment(
                &message,
                Snowflake(7),
                None,
                &BTreeMap::new(),
                &BTreeSet::new(),
                NOW,
            )
            .unwrap();
            assert_eq!(context.access, MediaAccess::Expired);
            assert_eq!(context.original_url, None);
            assert_eq!(context.download_url, None);
        }
        let message = message(
            json!([{"id":"7","filename":"x.png","content_type":"image/png","url":"https://user:password@cdn.example/x.png","proxy_url":"file:///tmp/x.png"}]),
            json!([]),
        );
        let context = resolve_attachment(
            &message,
            Snowflake(7),
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            NOW,
        )
        .unwrap();
        assert_eq!(context.access, MediaAccess::Unavailable);
        assert!(!context.can_copy_image());
    }
    #[test]
    fn media_thread_download_policy_hides_visual_actions_only_and_cannot_be_revealed_away() {
        let policy = channels(ChannelType::Media, 1 << 15);
        for kind in [
            MediaKind::Image,
            MediaKind::Gif,
            MediaKind::Gifv,
            MediaKind::Video,
        ] {
            assert!(hide_media_downloads(Snowflake(20), kind, &policy));
        }
        for kind in [MediaKind::Audio, MediaKind::File] {
            assert!(!hide_media_downloads(Snowflake(20), kind, &policy));
        }
        assert!(!hide_media_downloads(
            Snowflake(10),
            MediaKind::Image,
            &policy
        ));
        assert!(!hide_media_downloads(
            Snowflake(20),
            MediaKind::Image,
            &channels(ChannelType::Forum, 1 << 15)
        ));
        assert!(!hide_media_downloads(
            Snowflake(20),
            MediaKind::Image,
            &channels(ChannelType::Media, 0)
        ));
        let message = message(json!([image()]), json!([]));
        let revealed = BTreeSet::from(["attachment:3:100:7".into()]);
        let context =
            resolve_attachment(&message, Snowflake(7), None, &policy, &revealed, NOW).unwrap();
        assert_eq!(context.access, MediaAccess::HiddenByChannel);
        assert_eq!(context.original_url, None);
        assert_eq!(context.image_source, None);
    }
    #[test]
    fn forward_snapshots_keep_attachment_positions_reveals_and_suppression_independent() {
        let mut message = message(
            json!([image()]),
            json!([{"image":{"url":"https://cdn.example/outer.png","proxy_url":"https://proxy.example/outer.png"}}]),
        );
        message.extra.insert("message_snapshots".into(),json!([
            {"attachments":[{"bad":true},{"id":"7","filename":"SPOILER_forward.png","content_type":"image/png","url":"https://cdn.example/forward.png"}],"embeds":[{"image":{"url":"https://cdn.example/forward.png","proxy_url":"https://proxy.example/forward.png"}}]},
            {"flags":4,"embeds":[{"image":{"url":"https://cdn.example/suppressed.png","proxy_url":"https://proxy.example/suppressed.png"}}]}
        ]));
        let normal = resolve_attachment(
            &message,
            Snowflake(7),
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            NOW,
        )
        .unwrap();
        let forwarded = resolve_attachment(
            &message,
            Snowflake(7),
            Some(0),
            &BTreeMap::new(),
            &BTreeSet::from([normal.reveal_key]),
            NOW,
        )
        .unwrap();
        assert_eq!(forwarded.access, MediaAccess::Concealed);
        let revealed = BTreeSet::from([forwarded.reveal_key]);
        let forwarded = resolve_attachment(
            &message,
            Snowflake(7),
            Some(0),
            &BTreeMap::new(),
            &revealed,
            NOW,
        )
        .unwrap();
        assert_eq!(
            forwarded.original_url.as_deref(),
            Some("https://cdn.example/forward.png")
        );
        assert!(
            resolve_embed(
                &message,
                0,
                Some(1),
                &BTreeMap::new(),
                &revealed,
                false,
                NOW
            )
            .is_none()
        );
        message.flags = 4;
        assert!(
            resolve_embed(&message, 0, None, &BTreeMap::new(), &revealed, false, NOW).is_none()
        );
        assert!(
            resolve_embed(
                &message,
                0,
                Some(0),
                &BTreeMap::new(),
                &revealed,
                false,
                NOW
            )
            .unwrap()
            .available()
        );
        assert!(
            resolve_attachment(
                &message,
                Snowflake(7),
                Some(99),
                &BTreeMap::new(),
                &revealed,
                NOW
            )
            .is_none()
        );
    }
    #[test]
    fn image_embed_selection_matches_renderer_and_never_uses_an_article_as_original() {
        let embed = json!({"type":"image","url":"https://article.example/page","image":{"url":"https://cdn.example/still.png","proxy_url":"https://proxy.example/still.png"},"thumbnail":{"url":"https://cdn.example/moving.webp","proxy_url":"https://proxy.example/moving.webp?format=png","flags":32}});
        let mut message = message(json!([]), json!([embed]));
        let context = resolve_embed(
            &message,
            0,
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(context.kind, MediaKind::Gif);
        assert_eq!(
            context.original_url.as_deref(),
            Some("https://cdn.example/moving.webp")
        );
        message.extra.get_mut("embeds").unwrap()[0]["image"]["flags"] = json!(16);
        let context = resolve_embed(
            &message,
            0,
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(context.access, MediaAccess::Concealed);
        let revealed = BTreeSet::from([context.reveal_key]);
        let context =
            resolve_embed(&message, 0, None, &BTreeMap::new(), &revealed, false, NOW).unwrap();
        assert_eq!(
            context.original_url.as_deref(),
            Some("https://cdn.example/still.png")
        );
        assert_eq!(context.kind, MediaKind::Image);
    }
    #[test]
    fn playable_and_plain_file_contexts_retain_correct_type_ids_and_resource_identity() {
        for (mime, flags, expected) in [
            ("video/mp4", 0, MediaKind::Video),
            ("video/mp4", 32, MediaKind::Gifv),
            ("audio/ogg", 32, MediaKind::Audio),
            ("application/pdf", 0, MediaKind::File),
        ] {
            let message = message(
                json!([{"id":"7","filename":"../folder\\asset.bin","content_type":mime,"flags":flags,"url":"https://cdn.example/media?signature=x","proxy_url":"https://proxy.example/media?width=400&signature=x"}]),
                json!([]),
            );
            let context = resolve_attachment(
                &message,
                Snowflake(7),
                None,
                &BTreeMap::new(),
                &BTreeSet::new(),
                NOW,
            )
            .unwrap();
            assert_eq!(context.kind, expected);
            assert!(context.available());
            assert!(!context.can_copy_image());
            assert_eq!(context.suggested_filename(), "asset.bin");
            if let Some(playable) = playable_media::attachment(&message.attachments[0], "100", NOW)
            {
                assert_eq!(context.resource_key, playable.resource_key);
            }
        }
        let message = message(
            json!([]),
            json!([{"type":"video","url":"https://article.example/page","video":{"url":"https://cdn.example/clip.webm?signature=x","proxy_url":"https://proxy.example/clip.webm?format=webp&signature=x"}}]),
        );
        let context = resolve_embed(
            &message,
            0,
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(context.kind, MediaKind::Video);
        assert_eq!(context.filename, "clip.webm");
        assert_eq!(
            context.original_url.as_deref(),
            Some("https://cdn.example/clip.webm?signature=x")
        );
        assert_eq!(
            context.download_url.as_deref(),
            Some("https://proxy.example/clip.webm?signature=x")
        );
        let hidden = resolve_embed(
            &message,
            0,
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            NOW,
        )
        .unwrap();
        assert_eq!(hidden.access, MediaAccess::Concealed);
    }
    #[test]
    fn save_suggestions_follow_source_extensions_and_decode_url_names_safely() {
        let message = message(
            json!([]),
            json!([{"image":{"url":"https://cdn.example/My%20image","proxy_url":"https://proxy.example/a"}}]),
        );
        let context = resolve_embed(
            &message,
            0,
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(context.filename, "My image");
        assert_eq!(context.suggested_filename(), "My image.png");
        assert_eq!(decode_path_segment("a+b%20c"), Some("a+b c".into()));
        assert_eq!(decode_path_segment("bad%zz"), None);
        let mut context = context;
        context.filename = "../secret.gif".into();
        assert_eq!(context.suggested_filename(), "secret.gif");
        context.filename = "opaque.webp".into();
        context.kind = MediaKind::Gif;
        assert_eq!(context.suggested_filename(), "opaque.gif");
    }
    fn browser_video() -> Value {
        json!({"type":"rich","url":"https://article.example/page","title":"Article title",
            "image":{"url":"https://cdn.example/article.png","proxy_url":"https://proxy.example/article.png"},
            "thumbnail":{"url":"https://cdn.example/thumb.png","proxy_url":"https://proxy.example/thumb.png"},
            "video":{"url":"https://cdn.example/Actual%20video.mp4?sig=a%2Fb%20c&width=123"}})
    }
    fn video_target(snapshot_index: Option<usize>) -> MediaTarget {
        MediaTarget {
            snapshot_index,
            asset: MediaAsset::EmbedVideo(0),
        }
    }
    #[test]
    fn legacy_video_card_targets_its_video_without_a_proxy_and_never_the_article_or_image() {
        let mut message = message(json!([]), json!([browser_video()]));
        let image = resolve_embed(
            &message,
            0,
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(image.kind, MediaKind::Image);
        let video = resolve(
            &message,
            &video_target(None),
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(video.kind, MediaKind::Video);
        assert_eq!(video.filename, "Actual video.mp4");
        assert_eq!(
            video.original_url.as_deref(),
            Some("https://cdn.example/Actual%20video.mp4?sig=a%2Fb%20c&width=123")
        );
        assert_eq!(video.download_url, video.original_url);
        assert_eq!(video.image_source, None);
        assert_ne!(image.resource_key, video.resource_key);
        assert!(!video.resource_key.contains("sig="));
        message.extra.get_mut("embeds").unwrap()[0]["video"]["proxy_url"] =
            json!("https://proxy.example/video.mp4?width=400&format=webp&sig=a%2Fb%20c");
        message.extra.get_mut("embeds").unwrap()[0]["type"] = json!("gifv");
        let video = resolve(
            &message,
            &video_target(None),
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(video.kind, MediaKind::Gifv);
        assert_eq!(
            video.download_url.as_deref(),
            Some("https://proxy.example/video.mp4?sig=a%2Fb%20c")
        );
        assert!(
            video
                .original_url
                .as_ref()
                .unwrap()
                .starts_with("https://cdn.example/Actual")
        );
    }
    #[test]
    fn legacy_video_cards_revalidate_suppression_nested_expiry_maturity_and_channel_policy() {
        for patch in [
            json!({"expired":true}),
            json!({"video":{"url":"https://cdn.example/v.mp4","expired":true}}),
            json!({"video":{"url":"https://cdn.example/v.mp4","expires_at":"2000-01-01T00:00:00Z"}}),
        ] {
            let mut embed = browser_video();
            embed
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            let message = message(json!([]), json!([embed]));
            let video = resolve(
                &message,
                &video_target(None),
                &BTreeMap::new(),
                &BTreeSet::new(),
                false,
                NOW,
            )
            .unwrap();
            assert_eq!(video.access, MediaAccess::Expired);
            assert_eq!(video.original_url, None);
            assert_eq!(video.download_url, None);
        }
        for patch in [
            json!({"flags":8}),
            json!({"nsfw":true}),
            json!({"video":{"url":"https://cdn.example/v.mp4","flags":16}}),
        ] {
            let mut embed = browser_video();
            embed
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            let message = message(json!([]), json!([embed]));
            let video = resolve(
                &message,
                &video_target(None),
                &BTreeMap::new(),
                &BTreeSet::new(),
                false,
                NOW,
            )
            .unwrap();
            assert_eq!(video.access, MediaAccess::Concealed);
            assert_eq!(video.original_url, None);
            assert_eq!(video.download_url, None);
            assert!(
                resolve(
                    &message,
                    &video_target(None),
                    &BTreeMap::new(),
                    &BTreeSet::from([video.reveal_key]),
                    false,
                    NOW
                )
                .unwrap()
                .available()
            );
        }
        let mut message = message(json!([]), json!([browser_video()]));
        let hidden = resolve(
            &message,
            &video_target(None),
            &channels(ChannelType::Media, 1 << 15),
            &BTreeSet::new(),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(hidden.access, MediaAccess::HiddenByChannel);
        assert_eq!(hidden.original_url, None);
        let hidden = resolve(
            &message,
            &video_target(None),
            &BTreeMap::new(),
            &BTreeSet::new(),
            true,
            NOW,
        )
        .unwrap();
        assert_eq!(hidden.access, MediaAccess::Concealed);
        message.flags = 4;
        assert!(
            resolve(
                &message,
                &video_target(None),
                &BTreeMap::new(),
                &BTreeSet::new(),
                false,
                NOW
            )
            .is_none()
        );
        message.flags = 0;
        message.extra.get_mut("embeds").unwrap()[0]["suppressed"] = json!(true);
        assert!(
            resolve(
                &message,
                &video_target(None),
                &BTreeMap::new(),
                &BTreeSet::new(),
                false,
                NOW
            )
            .is_none()
        );
    }
    #[test]
    fn legacy_video_cards_use_the_exact_forward_snapshot_and_reveal_scope() {
        let mut message = message(json!([]), json!([browser_video()]));
        message.extra.insert(
            "message_snapshots".into(),
            json!([
                {"embeds":[{"video":{"url":"https://cdn.example/forward.mp4","flags":16}}]},
                {"flags":4,"embeds":[{"video":{"url":"https://cdn.example/suppressed.mp4"}}]}
            ]),
        );
        let normal = resolve(
            &message,
            &video_target(None),
            &BTreeMap::new(),
            &BTreeSet::new(),
            false,
            NOW,
        )
        .unwrap();
        let hidden = resolve(
            &message,
            &video_target(Some(0)),
            &BTreeMap::new(),
            &BTreeSet::from([normal.reveal_key]),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(hidden.access, MediaAccess::Concealed);
        let shown = resolve(
            &message,
            &video_target(Some(0)),
            &BTreeMap::new(),
            &BTreeSet::from([hidden.reveal_key]),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(
            shown.original_url.as_deref(),
            Some("https://cdn.example/forward.mp4")
        );
        assert!(
            resolve(
                &message,
                &video_target(Some(1)),
                &BTreeMap::new(),
                &BTreeSet::new(),
                false,
                NOW
            )
            .is_none()
        );
        assert!(
            resolve(
                &message,
                &video_target(Some(2)),
                &BTreeMap::new(),
                &BTreeSet::new(),
                false,
                NOW
            )
            .is_none()
        );
    }
}
