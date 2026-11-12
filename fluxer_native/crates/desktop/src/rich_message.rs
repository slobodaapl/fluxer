// SPDX-License-Identifier: AGPL-3.0-or-later
//! Credential-free message media. URLs come from messages or the selected
//! instance's discovery document; account credentials never enter this client.
use fluxer_core::{Attachment, Message, User};
use gpui::{http_client, prelude::*, *};
use serde_json::Value;
use std::{
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

fn avatar_url(user: &User, endpoint: Option<&str>) -> Option<String> {
    let mut base = http_client::Url::parse(&safe_url(endpoint?)?).ok()?;
    let hash = user
        .avatar
        .as_deref()?
        .strip_prefix("a_")
        .unwrap_or(user.avatar.as_deref()?);
    // Hashes are path components, never arbitrary source URLs.
    if hash.is_empty()
        || !hash
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
    {
        return None;
    }
    base.set_query(None);
    base.set_fragment(None);
    base.path_segments_mut().ok()?.pop_if_empty().extend([
        "avatars",
        &user.id.to_string(),
        &format!("{hash}.webp"),
    ]);
    base.query_pairs_mut().append_pair("size", "128");
    Some(base.into())
}

pub fn avatar(user: &User, media_endpoint: Option<&str>, size: f32) -> AnyElement {
    let name = user.display_name().to_owned();
    let id = user.id;
    let fallback = move || super::voice_avatar(&name, id, false, size);
    if let Some(url) = avatar_url(user, media_endpoint) {
        img(url)
            .size(px(size))
            .flex_shrink_0()
            .rounded_full()
            .object_fit(ObjectFit::Cover)
            .with_fallback(fallback)
            .into_any_element()
    } else {
        fallback()
    }
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

fn attachment_url(attachment: &Attachment, preview: bool) -> Option<String> {
    if attachment.extra.get("expired").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let proxy = attachment
        .extra
        .get("proxy_url")
        .and_then(Value::as_str)
        .and_then(safe_url);
    let original = attachment.url.as_deref().and_then(safe_url);
    if preview {
        proxy.or(original)
    } else {
        original.or(proxy)
    }
}

fn dimensions(width: Option<u32>, height: Option<u32>) -> (f32, f32) {
    let width = width.filter(|n| *n > 0).unwrap_or(400) as f32;
    let height = height.filter(|n| *n > 0).unwrap_or(240) as f32;
    let ratio = (400. / width).min(280. / height).min(1.);
    ((width * ratio).max(24.), (height * ratio).max(24.))
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

fn preview(
    id: String,
    url: String,
    target: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
) -> AnyElement {
    let (width, height) = dimensions(width, height);
    div()
        .id(SharedString::from(id))
        .max_w_full()
        .overflow_hidden()
        .rounded_md()
        .child(
            img(url)
                .w(px(width))
                .h(px(height))
                .max_w_full()
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
                        .child("Preview unavailable · open original below")
                        .into_any_element()
                }),
        )
        .when_some(target, |view, url| {
            view.cursor_pointer().on_click(move |_, _, cx| {
                cx.stop_propagation();
                cx.open_url(&url);
            })
        })
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

pub fn media(message: &Message) -> AnyElement {
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
            )
        });
    div()
        .flex()
        .flex_col()
        .gap_2()
        .min_w_0()
        .child(media_parts(message, &message.id.to_string()))
        .children(snapshots)
        .into_any_element()
}

fn media_parts(message: &Message, namespace: &str) -> AnyElement {
    let attachments = message.attachments.iter().map(|attachment| {
        let kind = attachment_kind(attachment);
        let original = attachment_url(attachment, false);
        let mut view = div().flex().flex_col().gap_1().min_w_0();
        if kind == MediaKind::Image
            && let Some(url) = attachment_url(attachment, true)
        {
            view = view.child(preview(
                format!("image-{namespace}-{}", attachment.id),
                url,
                original.clone(),
                attachment.width,
                attachment.height,
            ));
        }
        let prefix =
            if attachment.flags & (1 << 3) != 0 || attachment.filename.starts_with("SPOILER_") {
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
    });
    let embeds = message
        .extra
        .get("embeds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|_| message.flags & (1 << 2) == 0)
        .map(|(index, embed)| render_embed(namespace, index, embed));
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
fn embed_image(embed: &Value) -> Option<(&Value, String)> {
    ["image", "thumbnail"]
        .into_iter()
        .filter_map(|key| embed.get(key))
        .find_map(|image| {
            if image.get("flags").and_then(Value::as_u64).unwrap_or(0) & (1 << 4) != 0 {
                return None;
            }
            string_at(image, "proxy_url")
                .and_then(safe_url)
                .map(|url| (image, url))
        })
}
fn render_embed(message: &str, index: usize, embed: &Value) -> AnyElement {
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
    if let Some((image, url)) = embed_image(embed) {
        view = view.child(preview(
            format!("embed-image-{message}-{index}"),
            url.clone(),
            link.clone().or(Some(url)),
            image
                .get("width")
                .and_then(Value::as_u64)
                .and_then(|n| n.try_into().ok()),
            image
                .get("height")
                .and_then(Value::as_u64)
                .and_then(|n| n.try_into().ok()),
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
            avatar_url(&user, Some("https://self.example/media/")),
            Some("https://self.example/media/avatars/123/abc123.webp?size=128".into())
        );
        assert_eq!(avatar_url(&user, None), None);
        user.avatar = Some("../bad".into());
        assert_eq!(avatar_url(&user, Some("https://self.example/media/")), None);
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
    fn preview_dimensions_and_response_reads_are_bounded() {
        assert_eq!(dimensions(Some(4000), Some(2000)), (400., 200.));
        assert_eq!(dimensions(Some(0), Some(0)), (400., 240.));
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
