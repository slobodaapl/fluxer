// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explicit --demo --demo-emoji-time loopback fixtures. No account/API calls.
use super::*;
use fluxer_core::User;
use std::{
    io::{Cursor, Read, Write},
    net::TcpListener,
    time::Instant,
};

pub(super) fn enabled() -> bool {
    std::env::args().any(|arg| arg == "--demo-emoji-time")
}
pub(super) fn populate(view: &mut Fluxer, channel: Snowflake, author: &User) -> Option<String> {
    if !view.demo || !enabled() {
        return None;
    }
    let now = chrono::Utc::now().timestamp();
    let rows = vec![
        "👋🏽 🙂 👨‍👩‍👧‍👦 🇺🇸 🫱🏻‍🫲🏼".to_owned(),
        "<a:rainbow:900001> <:still:900002>".into(),
        "Inline café 日本語 👋🏽 <a:rainbow:900001> and ||<a:rainbow:900001> concealed||".into(),
        "🙂 ".repeat(30),
        "🙂 ".repeat(31),
        "**🙂** ||🙂|| `🙂` (formatting and spoilers stay inline)".into(),
        format!(
            "Time <t:{now}:t> · seconds <t:{now}:T>\nDates <t:{now}:d> · <t:{now}:D>\nDate/time <t:{now}:f> · <t:{now}:F>\nNumeric <t:{now}:s> · <t:{now}:S>\nRelative <t:{}:R> · <t:{}:R>",
            now - 15,
            now + 80
        ),
        format!("Concealed timestamp ||<t:{now}:R>|| · invalid <t:99999999999999999999:F>"),
        "Reply/profile inline fixtures remain small 🙂 <a:rainbow:900001>".into(),
    ];
    let mut messages = BTreeMap::new();
    for (index, content) in rows.into_iter().enumerate() {
        let id = Snowflake(70_000 + index as u64);
        let mut message = Message::text(id, channel, author.clone(), content);
        message.timestamp = format!("2026-11-16T12:00:{index:02}Z");
        if index == 8 {
            message.kind = 19;
            message.message_reference = Some(fluxer_core::MessageReference {
                channel_id: channel,
                message_id: Some(Snowflake(70_001)),
                guild_id: Some(Snowflake(1)),
                kind: 0,
            });
        }
        messages.insert(id, message);
    }
    view.state.messages.insert(channel, messages);
    if std::env::args().any(|arg| arg == "--demo-timestamp-fr") {
        view.state.user_settings["locale"] = serde_json::json!("fr");
    }
    if std::env::args().any(|arg| arg == "--demo-timestamp-24h") {
        view.state.user_settings["time_format"] = serde_json::json!(2);
    }
    if std::env::args().any(|arg| arg == "--demo-emoji-paused") {
        view.state.user_settings["animate_emoji"] = serde_json::json!(false);
    }
    fixture_server()
}
fn push_chunk(bytes: &mut Vec<u8>, name: &[u8; 4], payload: &[u8]) {
    bytes.extend_from_slice(name);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    if payload.len() % 2 != 0 {
        bytes.push(0);
    }
}
/// Build a real animated lossless WebP from independently encoded VP8L frames.
fn animated_webp() -> Vec<u8> {
    let mut chunks = Vec::new();
    push_chunk(&mut chunks, b"VP8X", &[2, 0, 0, 0, 31, 0, 0, 31, 0, 0]);
    push_chunk(&mut chunks, b"ANIM", &[0, 0, 0, 0, 0, 0]);
    for (index, color) in [
        [130, 100, 240, 255],
        [40, 190, 150, 255],
        [250, 180, 70, 255],
        [240, 90, 120, 255],
    ]
    .into_iter()
    .enumerate()
    {
        let image = image::RgbaImage::from_fn(32, 32, |x, y| {
            if x / 8 == index as u32 || y / 8 == index as u32 {
                image::Rgba([245, 245, 245, 255])
            } else {
                image::Rgba(color)
            }
        });
        let mut static_webp = Cursor::new(Vec::new());
        image
            .write_to(&mut static_webp, image::ImageFormat::WebP)
            .expect("fixture webp");
        let mut frame = vec![0, 0, 0, 0, 0, 0, 31, 0, 0, 31, 0, 0, 160, 0, 0, 2];
        frame.extend_from_slice(&static_webp.into_inner()[12..]);
        push_chunk(&mut chunks, b"ANMF", &frame);
    }
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&((chunks.len() + 4) as u32).to_le_bytes());
    bytes.extend_from_slice(b"WEBP");
    bytes.extend(chunks);
    bytes
}
fn fixture_server() -> Option<String> {
    let animated = animated_webp();
    let poster = image::load_from_memory(&animated).ok()?;
    let mut still = Cursor::new(Vec::new());
    poster.write_to(&mut still, image::ImageFormat::WebP).ok()?;
    let still = still.into_inner();
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let address = listener.local_addr().ok()?;
    listener.set_nonblocking(true).ok()?;
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(1800);
        let mut requests = 0;
        while Instant::now() < deadline && requests < 256 {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(30));
                    continue;
                }
                Err(_) => break,
            };
            requests += 1;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            let mut request = [0u8; 4096];
            let length = stream.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..length]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("");
            let valid = request.starts_with("GET ")
                && (path.starts_with("/emojis/900001.webp?")
                    || path.starts_with("/emojis/900002.webp?"));
            let (status, body) = if !valid {
                ("404 Not Found", &[][..])
            } else if path.contains("/900001.") && path.contains("animated=true") {
                ("200 OK", animated.as_slice())
            } else {
                ("200 OK", still.as_slice())
            };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: image/webp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    Some(format!("http://{address}"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use image::AnimationDecoder;
    #[test]
    fn community_emoji_fixture_is_actual_animated_webp_with_distinct_timed_frames() {
        let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(animated_webp())).unwrap();
        assert!(decoder.has_animation());
        let frames = decoder.into_frames().collect_frames().unwrap();
        assert_eq!(frames.len(), 4);
        for frame in &frames {
            assert_eq!(frame.buffer().dimensions(), (32, 32));
            assert_eq!(frame.delay().numer_denom_ms(), (160, 1));
        }
        for pair in frames.windows(2) {
            assert_ne!(pair[0].buffer(), pair[1].buffer());
        }
    }
}
