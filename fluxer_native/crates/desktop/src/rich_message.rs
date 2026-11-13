// SPDX-License-Identifier: AGPL-3.0-or-later
//! Credential-free message media. URLs come from messages or the selected
//! instance's discovery document; account credentials never enter this client.
use crate::{
    Fluxer,
    image_gallery::{ImageItem, safe_image_url},
    media_animation::{animated_proxy_url, is_animated_image},
    visible_image::VisibleImage,
};
use fluxer_core::{Attachment, Message, User};
use gpui::{http_client, prelude::*, *};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    io::Read,
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::Duration,
};

const TEXT: u32 = 0xece9f6;
const MUTED: u32 = 0xa19bb3;
const SURFACE: u32 = 0x252330;
const LINK: u32 = 0xa999ff;
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// The GPUI default client cannot fetch URLs. This adapter intentionally accepts
/// only GET and discards all request headers (especially Authorization).
struct MediaHttpClient;

pub fn http_client() -> Arc<dyn http_client::HttpClient> {
    Arc::new(MediaHttpClient)
}

impl http_client::HttpClient for MediaHttpClient {
    fn type_name(&self) -> &'static str {
        "FluxerMediaHttpClient"
    }
    fn user_agent(&self) -> Option<&http_client::http::HeaderValue> {
        None
    }
    fn proxy(&self) -> Option<&http_client::Url> {
        None
    }
    fn send(
        &self,
        request: http_client::Request<http_client::AsyncBody>,
    ) -> futures::future::BoxFuture<
        'static,
        http_client::Result<http_client::Response<http_client::AsyncBody>>,
    > {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let job = Box::new(move || {
            if !sender.is_canceled() {
                let _ = sender.send(fetch_image(request));
            }
        });
        let _ = image_workers().send(job);
        Box::pin(async move {
            receiver
                .await
                .map_err(|error| http_client::anyhow!(error))?
        })
    }
}

type MediaJob = Box<dyn FnOnce() + Send>;
fn image_workers() -> &'static mpsc::Sender<MediaJob> {
    static WORKERS: OnceLock<mpsc::Sender<MediaJob>> = OnceLock::new();
    WORKERS.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<MediaJob>();
        let receiver = Arc::new(Mutex::new(receiver));
        // A fixed pool keeps channel histories from creating one thread per
        // attachment. Cancelled GPUI loads are skipped before fetching.
        for _ in 0..4 {
            let receiver = receiver.clone();
            std::thread::spawn(move || {
                loop {
                    let job = receiver.lock().expect("media queue poisoned").recv();
                    match job {
                        Ok(job) => job(),
                        Err(_) => break,
                    }
                }
            });
        }
        sender
    })
}

fn network_client(follow: bool) -> http_client::Result<&'static reqwest::blocking::Client> {
    static FOLLOW: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    static NO_FOLLOW: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    let cell = if follow { &FOLLOW } else { &NO_FOLLOW };
    cell.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(if follow {
                reqwest::redirect::Policy::limited(5)
            } else {
                reqwest::redirect::Policy::none()
            })
            .build()
            .map_err(|error| error.to_string())
    })
    .as_ref()
    .map_err(|error| http_client::anyhow!("{error}"))
}

fn fetch_image(
    request: http_client::Request<http_client::AsyncBody>,
) -> http_client::Result<http_client::Response<http_client::AsyncBody>> {
    if request.method() != http_client::Method::GET {
        return Err(http_client::anyhow!("Media requests must use GET"));
    }
    let url = safe_url(&request.uri().to_string())
        .ok_or_else(|| http_client::anyhow!("Invalid media URL"))?;
    let follow = request
        .extensions()
        .get::<http_client::RedirectPolicy>()
        .is_some_and(|policy| !matches!(policy, http_client::RedirectPolicy::NoFollow));
    let response = network_client(follow)?.get(url).send()?;
    let status = response.status();
    if !status.is_success() {
        return Ok(http_client::Response::builder()
            .status(status.as_u16())
            .body(().into())?);
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_IMAGE_BYTES)
    {
        return Err(http_client::anyhow!(
            "Media exceeds the 16 MiB preview limit"
        ));
    }
    let bytes = read_bounded(response, MAX_IMAGE_BYTES)?;
    Ok(http_client::Response::builder()
        .status(status.as_u16())
        .body(bytes.into())?)
}

fn read_bounded(reader: impl Read, limit: u64) -> http_client::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(http_client::anyhow!(
            "Media exceeds the 16 MiB preview limit"
        ));
    }
    Ok(bytes)
}

fn safe_url(raw: &str) -> Option<String> {
    let url = http_client::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url.to_string())
}

pub fn avatar(user: &User, media_endpoint: Option<&str>, size: f32) -> AnyElement {
    crate::avatar_images::user_avatar(user, media_endpoint, size)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MediaKind {
    Image,
    Video,
    Audio,
    File,
}

fn attachment_kind(attachment: &Attachment) -> MediaKind {
    if attachment.flags & ((1 << 3) | (1 << 4)) != 0
        || attachment.filename.starts_with("SPOILER_")
        || attachment.extra.get("nsfw").and_then(Value::as_bool) == Some(true)
    {
        return MediaKind::File;
    }
    let mime = attachment
        .content_type
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    if mime.starts_with("image/") {
        MediaKind::Image
    } else if mime.starts_with("video/") {
        MediaKind::Video
    } else if mime.starts_with("audio/") {
        MediaKind::Audio
    } else {
        MediaKind::File
    }
}

fn attachment_expired(attachment: &Attachment) -> bool {
    attachment.extra.get("expired").and_then(Value::as_bool) == Some(true)
        || attachment
            .extra
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(|expires| chrono::DateTime::parse_from_rfc3339(expires).ok())
            .is_some_and(|expires| {
                expires.timestamp_millis() <= chrono::Utc::now().timestamp_millis()
            })
}

fn attachment_url(attachment: &Attachment, preview: bool) -> Option<String> {
    if attachment_expired(attachment) {
        return None;
    }
    let proxy = attachment
        .extra
        .get("proxy_url")
        .and_then(Value::as_str)
        .and_then(safe_url);
    let original = attachment.url.as_deref().and_then(safe_url);
    if preview {
        let url = proxy.or(original)?;
        if attachment_animated(attachment) {
            animated_proxy_url(&url, false)
        } else {
            Some(url)
        }
    } else {
        original.or(proxy)
    }
}

fn attachment_animated(attachment: &Attachment) -> bool {
    is_animated_image(
        attachment.content_type.as_deref(),
        &attachment.filename,
        u64::from(attachment.flags),
    )
}

fn dimensions(width: Option<u32>, height: Option<u32>, bounds: (f32, f32)) -> (f32, f32) {
    let width = width.filter(|n| *n > 0).unwrap_or(400) as f32;
    let height = height.filter(|n| *n > 0).unwrap_or(240) as f32;
    let ratio = (bounds.0 / width).min(bounds.1 / height).min(1.);
    ((width * ratio).max(1.), (height * ratio).max(1.))
}

fn byte_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.)
    } else {
        format!("{bytes} B")
    }
}

fn open_card(id: String, label: String, detail: String, url: Option<String>) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(0x423b50))
        .bg(rgb(SURFACE))
        .max_w(px(440.))
        .text_size(px(13.))
        .text_color(rgb(if url.is_some() { LINK } else { MUTED }))
        .child(label)
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(detail),
        )
        .when_some(url, |card, url| {
            card.cursor_pointer()
                .hover(|style| style.bg(rgb(0x302b3d)))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    cx.open_url(&url);
                })
        })
        .into_any_element()
}

struct MediaTooltip(String);
impl Render for MediaTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(320.))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(0x15121c))
            .text_size(px(12.))
            .text_color(rgb(TEXT))
            .child(self.0.clone())
    }
}

#[derive(Clone, Copy)]
struct PreviewSettings {
    natural_size: (Option<u32>, Option<u32>),
    max_size: (f32, f32),
    enabled: bool,
    autoplay: bool,
}

fn preview(
    id: String,
    url: String,
    gallery: Arc<Vec<ImageItem>>,
    index: usize,
    settings: PreviewSettings,
    cx: &mut Context<Fluxer>,
) -> AnyElement {
    let (width, height) = dimensions(
        settings.natural_size.0,
        settings.natural_size.1,
        settings.max_size,
    );
    let frame_id = crate::media_animation::image_id(&id, &url);
    div()
        .id(SharedString::from(id))
        .focusable()
        .tab_stop(true)
        .max_w_full()
        .overflow_hidden()
        .rounded_md()
        .on_hover(cx.listener(|_, _, _, cx| cx.notify()))
        .child(
            VisibleImage::new(frame_id, width, height, move || {
                img(url.clone())
                    .object_fit(ObjectFit::Contain)
                    .with_loading(|| {
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(MUTED))
                            .child("Loading preview…")
                            .into_any_element()
                    })
                    .with_fallback(|| {
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(MUTED))
                            .child("Preview unavailable · click to view image")
                            .into_any_element()
                    })
            })
            .enabled(settings.enabled)
            .autoplay(settings.autoplay),
        )
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.open_image_gallery(gallery.as_ref().clone(), index, window, cx);
        }))
        .into_any_element()
}

/// Forward snapshots are flat wire objects with their own flags and media.
/// Build only the fields used by this renderer, so nested snapshots cannot
/// recurse and malformed media entries do not discard a valid sibling.
fn snapshot_media(snapshot: &Value) -> Message {
    let attachments = snapshot
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|attachment| serde_json::from_value::<Attachment>(attachment.clone()).ok())
        .collect();
    let mut message = Message {
        content: snapshot
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        attachments,
        flags: snapshot
            .get("flags")
            .and_then(Value::as_u64)
            .and_then(|flags| flags.try_into().ok())
            .unwrap_or(0),
        ..Message::default()
    };
    if let Some(embeds) = snapshot.get("embeds").and_then(Value::as_array) {
        message
            .extra
            .insert("embeds".into(), Value::Array(embeds.clone()));
    }
    message
}

pub fn media(
    message: &Message,
    tooltips_enabled: bool,
    autoplay: bool,
    cx: &mut Context<Fluxer>,
) -> AnyElement {
    let snapshots = message
        .extra
        .get("message_snapshots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, snapshot)| {
            media_parts(
                &snapshot_media(snapshot),
                &format!("{}-snapshot-{index}", message.id),
                tooltips_enabled,
                autoplay,
                cx,
            )
        })
        .collect::<Vec<_>>();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .min_w_0()
        .child(media_parts(
            message,
            &message.id.to_string(),
            tooltips_enabled,
            autoplay,
            cx,
        ))
        .children(snapshots)
        .into_any_element()
}

/// Gallery entries retain their source attachment position so filtered files
/// cannot shift the clicked image to a different attachment.
fn attachment_gallery(message: &Message) -> Vec<(usize, ImageItem)> {
    message
        .attachments
        .iter()
        .enumerate()
        .filter_map(|(index, attachment)| {
            if attachment_kind(attachment) != MediaKind::Image || attachment_expired(attachment) {
                return None;
            }
            let mut source = attachment
                .extra
                .get("proxy_url")
                .and_then(Value::as_str)
                .and_then(safe_image_url)
                .or_else(|| attachment.url.as_deref().and_then(safe_image_url))?;
            let original = attachment.url.as_deref().and_then(safe_image_url);
            let animated = attachment_animated(attachment);
            if animated {
                source = animated_proxy_url(&source, false)?;
            }
            Some((
                index,
                ImageItem {
                    source,
                    original,
                    name: attachment.filename.clone(),
                    width: attachment.width,
                    height: attachment.height,
                    animated,
                },
            ))
        })
        .collect()
}

fn media_parts(
    message: &Message,
    namespace: &str,
    tooltips_enabled: bool,
    autoplay: bool,
    cx: &mut Context<Fluxer>,
) -> AnyElement {
    let indexed_gallery = attachment_gallery(message);
    let gallery = Arc::new(
        indexed_gallery
            .iter()
            .map(|(_, item)| item.clone())
            .collect::<Vec<_>>(),
    );
    let attachments = message
        .attachments
        .iter()
        .enumerate()
        .map(|(source_index, attachment)| {
            let kind = attachment_kind(attachment);
            let original = attachment_url(attachment, false);
            if kind == MediaKind::Image
                && let Some(url) = attachment_url(attachment, true)
                && let Some(index) = indexed_gallery
                    .iter()
                    .position(|(index, _)| *index == source_index)
            {
                let tooltip = format!("{} · {}", attachment.filename, byte_size(attachment.size));
                return div()
                    .id(SharedString::from(format!(
                        "attachment-image-{namespace}-{}",
                        attachment.id
                    )))
                    .max_w_full()
                    .min_w_0()
                    // GPUI's active tooltip uses source bounds during prepaint;
                    // removing its builder prevents it leaking above the modal.
                    .when(tooltips_enabled, |view| {
                        view.tooltip(move |_, cx| cx.new(|_| MediaTooltip(tooltip.clone())).into())
                    })
                    .child(preview(
                        format!("image-{namespace}-{}", attachment.id),
                        url,
                        gallery.clone(),
                        index,
                        PreviewSettings {
                            natural_size: (attachment.width, attachment.height),
                            max_size: (550., 350.),
                            enabled: tooltips_enabled,
                            autoplay,
                        },
                        cx,
                    ))
                    .into_any_element();
            }
            let view = div().flex().flex_col().gap_1().min_w_0();
            let prefix = if attachment.flags & (1 << 3) != 0
                || attachment.filename.starts_with("SPOILER_")
            {
                "Spoiler · "
            } else if attachment.flags & (1 << 4) != 0
                || attachment.extra.get("nsfw").and_then(Value::as_bool) == Some(true)
            {
                "Sensitive media · "
            } else {
                match kind {
                    MediaKind::Video => "Video · ",
                    MediaKind::Audio => "Audio · ",
                    _ => "",
                }
            };
            view.child(open_card(
                format!("attachment-{namespace}-{}", attachment.id),
                format!("{prefix}{}", attachment.filename),
                format!(
                    "{} · {}",
                    byte_size(attachment.size),
                    if original.is_some() {
                        "Open original"
                    } else {
                        "File unavailable"
                    }
                ),
                original,
            ))
            .into_any_element()
        })
        .collect::<Vec<_>>();
    let spoilered = spoilered_embed_urls(&message.content);
    let embeds = message
        .extra
        .get("embeds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|_| message.flags & (1 << 2) == 0)
        .map(|(index, embed)| {
            if embed_is_spoiler(embed, &spoilered) {
                div()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child("Spoiler · preview hidden")
                    .into_any_element()
            } else {
                render_embed(namespace, index, embed, tooltips_enabled, autoplay, cx)
            }
        })
        .collect::<Vec<_>>();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .min_w_0()
        .children(attachments)
        .children(embeds)
        .into_any_element()
}

fn string_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// SpoilerUtils.canonicalizeMediaUrl: YouTube variants share a video key;
/// attachment signing fields do not change which concealed media is matched.
fn canonical_media_url(raw: &str) -> Option<String> {
    let mut url = http_client::Url::parse(raw).ok()?;
    let host = url.host_str().unwrap_or_default();
    if matches!(
        host,
        "www.youtube.com" | "youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be"
    ) {
        let video = if host == "youtu.be" {
            url.path()
                .strip_prefix('/')
                .and_then(|path| path.split('/').next())
                .map(str::to_owned)
        } else if url.path() == "/watch" {
            url.query_pairs()
                .find(|(key, _)| key == "v")
                .map(|(_, value)| value.into_owned())
        } else {
            ["/shorts/", "/v/", "/embed/"].iter().find_map(|prefix| {
                url.path()
                    .strip_prefix(prefix)
                    .and_then(|path| path.split('/').next())
                    .map(str::to_owned)
            })
        };
        if let Some(video) = video.filter(|video| !video.is_empty()) {
            return Some(format!("youtube:{video}"));
        }
    }
    if let Some(query) = url.query() {
        let fields: Vec<_> = query
            .split('&')
            .filter(|field| {
                if field.is_empty() || *field == "=" {
                    return false;
                }
                let key = http_client::Url::parse(&format!("https://field.invalid/?{field}"))
                    .ok()
                    .and_then(|url| url.query_pairs().next().map(|(key, _)| key.into_owned()));
                !matches!(key.as_deref(), Some("ex" | "is" | "hm" | "uc"))
            })
            .collect();
        let query = fields.join("&");
        url.set_query((!query.is_empty()).then_some(&query));
    }
    let mut canonical = url.to_string();
    if canonical.ends_with('/') {
        canonical.pop();
    }
    Some(canonical)
}

/// Mirrors the source's non-greedy ||...|| and HTTP URL scans without parsing
/// message Markdown a second time. Only closed spoiler pairs participate.
fn spoilered_embed_urls(content: &str) -> BTreeSet<String> {
    let mut urls = BTreeSet::new();
    let mut rest = content;
    while let Some(open) = rest.find("||") {
        rest = &rest[open + 2..];
        let Some(close) = rest.find("||") else { break };
        let body = &rest[..close];
        let mut index = 0;
        while index < body.len() {
            let tail = &body[index..];
            if !(tail
                .get(..7)
                .is_some_and(|start| start.eq_ignore_ascii_case("http://"))
                || tail
                    .get(..8)
                    .is_some_and(|start| start.eq_ignore_ascii_case("https://")))
            {
                index += tail.chars().next().unwrap().len_utf8();
                continue;
            }
            let end = tail
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\''))
                .unwrap_or(tail.len());
            if let Some(url) = canonical_media_url(&tail[..end]) {
                urls.insert(url);
            }
            // URL_REGEX is global and nonoverlapping: a second http:// inside
            // a URL's path does not start another URL match.
            index += end;
        }
        rest = &rest[close + 2..];
    }
    urls
}

fn embed_is_spoiler(embed: &Value, spoilered: &BTreeSet<String>) -> bool {
    if spoilered.is_empty() {
        return false;
    }
    let mut urls = Vec::new();
    urls.extend(string_at(embed, "url"));
    urls.extend(
        embed
            .get("provider")
            .and_then(|provider| string_at(provider, "url")),
    );
    for key in ["image", "thumbnail", "video", "audio"] {
        if let Some(media) = embed.get(key) {
            urls.extend(string_at(media, "url"));
            urls.extend(string_at(media, "proxy_url"));
        }
    }
    urls.into_iter()
        .filter_map(canonical_media_url)
        .any(|url| spoilered.contains(&url))
}

fn embed_image(embed: &Value) -> Option<(&Value, String)> {
    if embed.get("nsfw").and_then(Value::as_bool) == Some(true)
        || embed.get("expired").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    // Choose the source's animated thumbnail for IMAGE embeds. Check privacy
    // before reordering so choosing animation cannot reveal a concealed asset.
    for image in ["image", "thumbnail"]
        .into_iter()
        .filter_map(|key| embed.get(key))
    {
        if image.get("flags").and_then(Value::as_u64).unwrap_or(0) & ((1 << 3) | (1 << 4)) != 0
            || image.get("nsfw").and_then(Value::as_bool) == Some(true)
            || image.get("expired").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
    }
    let keys = if string_at(embed, "type") == Some("image")
        && embed
            .get("thumbnail")
            .and_then(|image| image.get("flags"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            & (1 << 5)
            != 0
    {
        ["thumbnail", "image"]
    } else {
        ["image", "thumbnail"]
    };
    for image in keys.into_iter().filter_map(|key| embed.get(key)) {
        if let Some(url) = string_at(image, "proxy_url").and_then(safe_image_url) {
            return Some((image, url));
        }
    }
    None
}
fn embed_gallery_item(embed: &Value) -> Option<ImageItem> {
    let (image, mut source) = embed_image(embed)?;
    let animated = is_animated_image(
        string_at(image, "content_type"),
        string_at(image, "url").unwrap_or(&source),
        image.get("flags").and_then(Value::as_u64).unwrap_or(0),
    );
    if animated {
        source = animated_proxy_url(&source, false)?;
    }
    Some(ImageItem {
        original: string_at(image, "url")
            .and_then(safe_image_url)
            .or_else(|| Some(source.clone())),
        source,
        animated,
        name: string_at(image, "description")
            .or_else(|| string_at(embed, "title"))
            .unwrap_or("Image")
            .to_owned(),
        width: image
            .get("width")
            .and_then(Value::as_u64)
            .and_then(|value| value.try_into().ok()),
        height: image
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|value| value.try_into().ok()),
    })
}
fn render_embed(
    message: &str,
    index: usize,
    embed: &Value,
    enabled: bool,
    autoplay: bool,
    cx: &mut Context<Fluxer>,
) -> AnyElement {
    let link = string_at(embed, "url").and_then(safe_url);
    let color = embed
        .get("color")
        .and_then(Value::as_u64)
        .filter(|n| *n <= 0xffffff)
        .unwrap_or(0x8873ee) as u32;
    let mut view = div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .max_w(px(440.))
        .min_w_0()
        .rounded_md()
        .border_l_4()
        .border_color(rgb(color))
        .bg(rgb(SURFACE))
        .text_color(rgb(TEXT))
        .text_size(px(13.));
    if let Some(provider) = embed
        .get("provider")
        .and_then(|provider| string_at(provider, "name"))
    {
        view = view.child(
            div()
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(provider.to_owned()),
        );
    }
    if let Some(author) = embed
        .get("author")
        .and_then(|author| string_at(author, "name"))
    {
        view = view.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(author.to_owned()),
        );
    }
    if let Some(title) = string_at(embed, "title") {
        view = view.child(
            div()
                .id(SharedString::from(format!("embed-title-{message}-{index}")))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(LINK))
                .child(title.to_owned())
                .when_some(link.clone(), |title, url| {
                    title.cursor_pointer().on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cx.open_url(&url);
                    })
                }),
        );
    }
    if let Some(description) = string_at(embed, "description") {
        view = view.child(description.to_owned());
    }
    if let Some(fields) = embed.get("fields").and_then(Value::as_array) {
        for field in fields.iter().take(25) {
            view = view.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when_some(string_at(field, "name"), |view, name| {
                        view.child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(name.to_owned()),
                        )
                    })
                    .when_some(string_at(field, "value"), |view, value| {
                        view.child(value.to_owned())
                    }),
            );
        }
    }
    if let Some(item) = embed_gallery_item(embed) {
        view = view.child(preview(
            format!("embed-image-{message}-{index}"),
            item.source.clone(),
            Arc::new(vec![item.clone()]),
            0,
            PreviewSettings {
                natural_size: (item.width, item.height),
                max_size: (400., 300.),
                enabled,
                autoplay,
            },
            cx,
        ));
    }
    if let Some(video) = embed
        .get("video")
        .and_then(|video| string_at(video, "url"))
        .and_then(safe_url)
    {
        view = view.child(open_card(
            format!("embed-video-{message}-{index}"),
            "▶ Open video".into(),
            "Plays in your browser or default player".into(),
            link.clone().or(Some(video)),
        ));
    }
    if let Some(footer) = embed
        .get("footer")
        .and_then(|footer| string_at(footer, "text"))
    {
        view = view.child(
            div()
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(footer.to_owned()),
        );
    }
    view.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use serde_json::json;
    #[test]
    fn gif_attachment_and_embed_request_animation_in_chat_and_full_view() {
        let attachment: Attachment = serde_json::from_value(json!({
            "id":"7", "filename":"dance.gif", "content_type":"image/gif",
            "url":"https://cdn.example/dance.gif?signature=a%2Fb",
            "proxy_url":"https://proxy.example/dance.gif?width=320&format=png"
        }))
        .unwrap();
        assert_eq!(
            attachment_url(&attachment, true).as_deref(),
            Some("https://proxy.example/dance.gif?width=320&animated=true")
        );
        let message = Message {
            attachments: vec![attachment],
            ..Default::default()
        };
        let items = attachment_gallery(&message)
            .into_iter()
            .map(|(_, item)| item)
            .collect();
        let gallery = crate::image_gallery::ImageGallery::new(items, 0).unwrap();
        let item = gallery.current().unwrap();
        assert!(item.animated);
        assert_eq!(item.source, "https://proxy.example/dance.gif?animated=true");
        assert_eq!(
            item.original.as_deref(),
            Some("https://cdn.example/dance.gif?signature=a%2Fb")
        );
        let embed = embed_gallery_item(&json!({"image":{
            "url":"https://cdn.example/moving.webp", "proxy_url":"https://proxy.example/moving.webp?format=png", "flags":32
        }})).unwrap();
        assert!(embed.animated);
        assert_eq!(
            embed.source,
            "https://proxy.example/moving.webp?animated=true"
        );
        let thumbnail = embed_gallery_item(&json!({"type":"image",
            "image":{"proxy_url":"https://proxy.example/still.png"},
            "thumbnail":{"proxy_url":"https://proxy.example/moving.gif","flags":32}
        }))
        .unwrap();
        assert_eq!(
            thumbnail.source,
            "https://proxy.example/moving.gif?animated=true"
        );
    }
    #[test]
    fn urls_allow_only_credential_free_http() {
        for invalid in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:image/png,x",
            "https://user:pass@example.com/a",
            "https://user@example.com/a",
            "//example.com/a",
        ] {
            assert!(safe_url(invalid).is_none(), "{invalid}");
        }
        assert_eq!(
            safe_url("https://media.example/a?signature=abc").as_deref(),
            Some("https://media.example/a?signature=abc")
        );
    }
    #[test]
    fn avatar_uses_discovered_media_prefix_and_static_hash() {
        let mut user = User {
            id: fluxer_core::Snowflake(123),
            avatar: Some("a_abc123".into()),
            ..User::default()
        };
        assert_eq!(
            crate::avatar_images::user_avatar_url(&user, Some("https://self.example/media/")),
            Some("https://self.example/media/avatars/123/abc123.webp?size=160".into())
        );
        assert_eq!(crate::avatar_images::user_avatar_url(&user, None), None);
        user.avatar = Some("../bad".into());
        assert_eq!(
            crate::avatar_images::user_avatar_url(&user, Some("https://self.example/media/")),
            None
        );
    }
    #[test]
    fn spoiler_and_sensitive_attachments_never_inline_preview() {
        let mut attachment = Attachment {
            content_type: Some("image/png".into()),
            ..Attachment::default()
        };
        assert_eq!(attachment_kind(&attachment), MediaKind::Image);
        attachment.flags = 1 << 3;
        assert_eq!(attachment_kind(&attachment), MediaKind::File);
        attachment.flags = 1 << 4;
        assert_eq!(attachment_kind(&attachment), MediaKind::File);
        attachment.flags = 0;
        attachment.filename = "SPOILER_secret.png".into();
        assert_eq!(attachment_kind(&attachment), MediaKind::File);
    }
    #[test]
    fn previews_prefer_proxy_but_open_original_preserves_signature() {
        let attachment: Attachment = serde_json::from_value(json!({"id":"1","filename":"a.png","url":"https://cdn.example/a?signature=x","proxy_url":"https://proxy.example/a?width=400"})).unwrap();
        assert_eq!(
            attachment_url(&attachment, true).as_deref(),
            Some("https://proxy.example/a?width=400")
        );
        assert_eq!(
            attachment_url(&attachment, false).as_deref(),
            Some("https://cdn.example/a?signature=x")
        );
        assert!(
            embed_image(&json!({"image":{"url":"https://third-party.example/tracking.png"}}))
                .is_none()
        );
        assert!(
            embed_image(&json!({"image":{"proxy_url":"https://proxy.example/safe.png"}})).is_some()
        );
    }
    #[test]
    fn flat_forward_snapshots_keep_media_without_recursing() {
        let snapshot = snapshot_media(
            &json!({"content":"forwarded", "flags":4, "attachments":[{"id":"9","filename":"note.txt","url":"https://cdn.example/note"}, {"invalid":true}], "embeds":[{"title":"Link"}], "message_snapshots":[{"attachments":[{"id":"11","filename":"nested.txt"}]}]}),
        );
        assert_eq!(snapshot.attachments.len(), 1);
        assert_eq!(snapshot.content, "forwarded");
        assert_eq!(snapshot.attachments[0].filename, "note.txt");
        assert_eq!(snapshot.flags, 4);
        assert_eq!(snapshot.extra.get("embeds").unwrap()[0]["title"], "Link");
        assert!(!snapshot.extra.contains_key("message_snapshots"));
    }
    #[test]
    fn nsfw_and_expired_schema_fields_hide_media() {
        let attachment: Attachment = serde_json::from_value(json!({"id":"1","filename":"a.png","content_type":"image/png","url":"https://cdn.example/a","nsfw":true})).unwrap();
        assert_eq!(attachment_kind(&attachment), MediaKind::File);
        let attachment: Attachment = serde_json::from_value(
            json!({"id":"2","filename":"expired.png","url":"https://cdn.example/a","expired":true}),
        )
        .unwrap();
        assert_eq!(attachment_url(&attachment, true), None);
        assert_eq!(attachment_url(&attachment, false), None);
        assert!(
            embed_image(&json!({"image":{"proxy_url":"https://proxy.example/a","flags":16}}))
                .is_none()
        );
    }
    #[test]
    fn image_gallery_filters_hidden_files_and_maps_exact_clicked_attachment_position() {
        let message:Message=serde_json::from_value(json!({"id":"100","channel_id":"10","author":{"id":"1","username":"A"},"attachments":[
            {"id":"1","filename":"first.png","content_type":"image/png","url":"https://cdn.example/one?signature=x","proxy_url":"https://proxy.example/one?width=400"},
            {"id":"2","filename":"note.txt","content_type":"text/plain","url":"https://cdn.example/note"},
            {"id":"3","filename":"SPOILER_secret.png","content_type":"image/png","url":"https://cdn.example/secret"},
            {"id":"4","filename":"second.png","content_type":"image/png","url":"https://cdn.example/two"},
            {"id":"5","filename":"sensitive.png","content_type":"image/png","flags":16,"url":"https://cdn.example/sensitive"},
            {"id":"6","filename":"expired.png","content_type":"image/png","expired":true,"url":"https://cdn.example/expired"},
            {"id":"7","filename":"nsfw.png","content_type":"image/png","nsfw":true,"url":"https://cdn.example/nsfw"},
            {"id":"8","filename":"old.png","content_type":"image/png","expires_at":"2000-01-01T00:00:00Z","url":"https://cdn.example/old"}
        ]})).unwrap();
        let gallery = attachment_gallery(&message);
        assert_eq!(
            gallery.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            vec![0, 3]
        );
        let clicked = gallery.iter().position(|(index, _)| *index == 3).unwrap();
        let opened = crate::image_gallery::ImageGallery::new(
            gallery.into_iter().map(|(_, item)| item).collect(),
            clicked,
        )
        .unwrap();
        assert_eq!(opened.current().unwrap().name, "second.png");
        assert_eq!(opened.items[0].source, "https://proxy.example/one");
        assert_eq!(
            opened.items[0].original.as_deref(),
            Some("https://cdn.example/one?signature=x")
        );
        assert_eq!(
            attachment_url(&message.attachments[0], true).as_deref(),
            Some("https://proxy.example/one?width=400")
        );
        assert!(attachment_url(&message.attachments[7], true).is_none());
    }
    #[test]
    fn embedded_image_viewer_uses_image_original_not_the_article_and_stays_scoped() {
        let embed = json!({"url":"https://article.example/post","title":"Article","image":{"url":"https://images.example/original.png?signature=x","proxy_url":"https://proxy.example/image?width=400","width":800,"height":600}});
        let item = embed_gallery_item(&embed).unwrap();
        assert_eq!(
            item.original.as_deref(),
            Some("https://images.example/original.png?signature=x")
        );
        let gallery = crate::image_gallery::ImageGallery::new(vec![item], 0).unwrap();
        assert_eq!(gallery.items.len(), 1);
        assert_eq!(
            gallery.current().unwrap().source,
            "https://proxy.example/image"
        );
        let no_original=embed_gallery_item(&json!({"url":"https://article.example/post","thumbnail":{"proxy_url":"https://proxy.example/thumb"}})).unwrap();
        assert_eq!(
            no_original.original.as_deref(),
            Some("https://proxy.example/thumb")
        );
        for hidden in [
            json!({"nsfw":true,"image":{"proxy_url":"https://proxy.example/hidden"}}),
            json!({"image":{"proxy_url":"https://proxy.example/hidden","nsfw":true}}),
            json!({"image":{"proxy_url":"https://proxy.example/hidden","flags":16},"thumbnail":{"proxy_url":"https://proxy.example/also-hidden"}}),
            json!({"image":{"proxy_url":"https://proxy.example/hidden","expired":true}}),
        ] {
            assert!(embed_gallery_item(&hidden).is_none());
        }
    }
    #[test]
    fn forward_snapshots_keep_gallery_and_spoiler_privacy_in_their_own_message_scope() {
        let outer = snapshot_media(
            &json!({"attachments":[{"id":"1","filename":"outer.png","content_type":"image/png","url":"https://cdn.example/outer"}]}),
        );
        let forwarded = snapshot_media(
            &json!({"content":"||https://article.example/private||","attachments":[{"id":"1","filename":"forwarded.png","content_type":"image/png","url":"https://cdn.example/forwarded"}],"embeds":[{"url":"https://article.example/private","image":{"proxy_url":"https://proxy.example/private"}}]}),
        );
        assert_eq!(attachment_gallery(&outer)[0].1.name, "outer.png");
        assert_eq!(attachment_gallery(&forwarded)[0].1.name, "forwarded.png");
        let embed = &forwarded.extra["embeds"][0];
        assert!(embed_is_spoiler(
            embed,
            &spoilered_embed_urls(&forwarded.content)
        ));
        assert!(!embed_is_spoiler(
            embed,
            &spoilered_embed_urls(&outer.content)
        ));
    }
    #[test]
    fn spoiler_url_matching_is_closed_nonoverlapping_and_canonicalizes_source_signatures() {
        let urls = spoilered_embed_urls(
            "public https://public.example ||HTTPS://cdn.example/attachments/1/2/image.png?ex=old&is=old&hm=old|| ||https://youtu.be/abc|| ||https://host.example/http://nested.example|| ||https://unclosed.example",
        );
        assert!(urls.contains("https://cdn.example/attachments/1/2/image.png"));
        assert!(urls.contains("youtube:abc"));
        assert!(urls.contains("https://host.example/http://nested.example"));
        assert!(!urls.contains("http://nested.example"));
        assert!(!urls.contains("https://unclosed.example"));
        assert!(!urls.contains("https://public.example"));
        assert!(embed_is_spoiler(
            &json!({"image":{"url":"https://cdn.example/attachments/1/2/image.png?hm=new&ex=new&is=new"}}),
            &urls
        ));
        assert!(embed_is_spoiler(
            &json!({"url":"https://www.youtube.com/watch?v=abc"}),
            &urls
        ));
        assert!(
            spoilered_embed_urls(&format!(
                "||https://host.example/{}||",
                "http://nested.example/".repeat(2000)
            ))
            .len()
                == 1
        );
    }
    #[test]
    fn preview_dimensions_and_response_reads_are_bounded() {
        assert_eq!(
            dimensions(Some(4000), Some(2000), (550., 350.)),
            (550., 275.)
        );
        assert_eq!(dimensions(Some(0), Some(0), (550., 350.)), (400., 240.));
        assert_eq!(read_bounded(&b"1234"[..], 4).unwrap(), b"1234");
        assert!(read_bounded(&b"12345"[..], 4).is_err());
    }
    #[test]
    fn media_http_client_rejects_large_declared_responses_and_non_gets() {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut bytes = [0; 4096];
            let length = socket.read(&mut bytes).unwrap();
            assert!(length > 0);
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_IMAGE_BYTES + 1
            )
            .unwrap();
        });
        let request = http_client::Request::builder()
            .uri(format!("http://{address}/large"))
            .body(().into())
            .unwrap();
        assert!(
            fetch_image(request)
                .err()
                .unwrap()
                .to_string()
                .contains("16 MiB")
        );
        server.join().unwrap();
        let request = http_client::Request::builder()
            .method("POST")
            .uri("https://example.test/a")
            .body(().into())
            .unwrap();
        assert!(
            fetch_image(request)
                .err()
                .unwrap()
                .to_string()
                .contains("GET")
        );
    }
    #[test]
    fn media_http_client_worker_returns_decodable_image_bytes() {
        use futures::AsyncReadExt;
        use gpui::http_client::HttpClient as _;
        use std::{
            io::{Cursor, Write},
            net::TcpListener,
        };

        let pixels = image::RgbaImage::from_pixel(2, 3, image::Rgba([44, 88, 132, 255]));
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        let expected = png.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).unwrap() > 0);
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", png.len()).unwrap();
            socket.write_all(&png).unwrap();
        });
        let request = http_client::Request::builder()
            .uri(format!("http://{address}/preview.png"))
            .body(().into())
            .unwrap();
        let bytes = futures::executor::block_on(async {
            let mut response = MediaHttpClient.send(request).await.unwrap();
            assert_eq!(response.status(), http_client::StatusCode::OK);
            let mut bytes = Vec::new();
            response.body_mut().read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        assert_eq!(bytes, expected);
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 3));
        assert_eq!(decoded.get_pixel(1, 2).0, [44, 88, 132, 255]);
        server.join().unwrap();
    }
    #[test]
    fn media_http_client_preserves_failure_status_and_omits_credentials() {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut bytes = [0; 4096];
            let length = socket.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..length]).to_ascii_lowercase();
            assert!(!request.contains("authorization:"));
            assert!(!request.contains("cookie:"));
            socket
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let request = http_client::Request::builder()
            .uri(format!("http://{address}/missing"))
            .header("Authorization", "secret")
            .header("Cookie", "secret")
            .body(().into())
            .unwrap();
        assert_eq!(
            fetch_image(request).unwrap().status(),
            http_client::StatusCode::NOT_FOUND
        );
        server.join().unwrap();
    }
}
