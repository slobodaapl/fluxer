// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generated, loopback-only media. The application never invokes ffmpeg; fixture
//! assets were generated from testsrc2 and sine filters during development.
use super::*;
use fluxer_core::Attachment;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
};

const VIDEO: &[u8] = include_bytes!("../assets/media-demo/video.mp4");
const AUDIO: &[u8] = include_bytes!("../assets/media-demo/tone.ogg");
const POSTER: &[u8] = include_bytes!("../assets/media-demo/poster.png");

impl Fluxer {
    pub(super) fn maybe_demo_media(&mut self, cx: &mut Context<Self>) {
        if !self.demo
            || self.media_demo_started
            || self.state.current_user.is_none()
            || !std::env::args().any(|arg| arg == "--demo-media")
        {
            return;
        }
        let Some(channel) = self.state.selected_channel else {
            return;
        };
        self.media_demo_started = true;
        let origin = match fixture_server() {
            Ok(origin) => origin,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let author = self
            .state
            .current_user
            .clone()
            .expect("ready offline account");
        let attachment = |id, filename: &str, route: &str, mime: &str, size| Attachment {
            id: Snowflake(id),
            filename: filename.into(),
            size,
            url: Some(format!("{origin}/{route}")),
            content_type: Some(mime.into()),
            width: (mime.starts_with("video/")).then_some(480),
            height: (mime.starts_with("video/")).then_some(270),
            extra: BTreeMap::from([
                (
                    "proxy_url".into(),
                    json!(format!("{origin}/{route}?signature=fixture")),
                ),
                ("duration".into(), json!(3.0)),
            ]),
            ..Default::default()
        };
        let mut video = Message::text(
            Snowflake(70_000),
            channel,
            author.clone(),
            "Native video · play, seek, pause or open full view. This generated clip has no audio.",
        );
        video.attachments.push(attachment(
            71_000,
            "Generated silent MP4",
            "video.mp4",
            "video/mp4",
            VIDEO.len() as u64,
        ));
        let mut audio = Message::text(
            Snowflake(70_001),
            channel,
            author.clone(),
            "Native audio · an optional generated 440 Hz test tone. It starts only when you press Play.",
        );
        audio.attachments.push(attachment(
            71_001,
            "Generated test tone.ogg",
            "tone.ogg",
            "audio/ogg",
            AUDIO.len() as u64,
        ));
        let mut gifv = Message::text(
            Snowflake(70_002),
            channel,
            author.clone(),
            "MP4-backed GIFV · muted, looping and paused outside the viewport.",
        );
        gifv.extra.insert("embeds".into(), json!([{
            "type":"gifv", "title":"Generated GIFV", "url":format!("{origin}/gifv.mp4"),
            "video":{"url":format!("{origin}/gifv.mp4"),"proxy_url":format!("{origin}/gifv.mp4?signature=fixture"),"width":480,"height":270,"duration":3.0},
            "thumbnail":{"proxy_url":format!("{origin}/poster.png"),"width":480,"height":270}
        }]));
        let mut spoiler = Message::text(
            Snowflake(70_003),
            channel,
            author.clone(),
            "Concealed video preloads a blurred paused frame. Reveal before playing.",
        );
        let mut hidden = attachment(
            71_003,
            "SPOILER_generated.mp4",
            "concealed.mp4",
            "video/mp4",
            VIDEO.len() as u64,
        );
        hidden.flags = 8;
        spoiler.attachments.push(hidden);
        let mut expired = Message::text(
            Snowflake(70_004),
            channel,
            author,
            "Expired media remains unavailable and must not be fetched.",
        );
        let mut unavailable = attachment(
            71_004,
            "expired.mp4",
            "must-not-fetch.mp4",
            "video/mp4",
            VIDEO.len() as u64,
        );
        unavailable.extra.insert("expired".into(), json!(true));
        expired.attachments.push(unavailable);
        let mut messages = vec![video, audio, gifv, spoiler, expired];
        for (index, message) in messages.iter_mut().enumerate() {
            message.timestamp = format!("2026-11-13T12:{index:02}:00Z");
        }
        self.state.messages.insert(
            channel,
            messages
                .into_iter()
                .map(|message| (message.id, message))
                .collect(),
        );
        self.media_endpoint = Some(origin);
        self.timeline.scroll_to_top_of_item(0);
        cx.spawn(async move |entity, cx| {
            Timer::after(Duration::from_millis(250)).await;
            let _ = entity.update(cx, |this, cx| {
                this.timeline.scroll_to_top_of_item(0);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn fixture_server() -> Result<String, String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|_| "Could not open the offline media fixture")?;
    let origin = format!(
        "http://{}",
        listener.local_addr().map_err(|_| "No fixture address")?
    );
    std::thread::spawn(move || {
        let mut requests = 0_u64;
        for mut stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let mut request = [0_u8; 8192];
            let Ok(count) = stream.read(&mut request) else {
                continue;
            };
            let line = String::from_utf8_lossy(&request[..count]);
            let path = line.split_whitespace().nth(1).unwrap_or_default();
            let route = path.split('?').next().unwrap_or_default();
            let (mime, bytes, kind) = if path.contains("format=webp") || route == "/poster.png" {
                ("image/png", POSTER, "poster")
            } else if matches!(route, "/video.mp4" | "/gifv.mp4" | "/concealed.mp4") {
                (
                    "video/mp4",
                    VIDEO,
                    if route == "/concealed.mp4" {
                        "concealed-video"
                    } else {
                        "video"
                    },
                )
            } else if route == "/tone.ogg" {
                ("audio/ogg", AUDIO, "audio")
            } else {
                if route == "/must-not-fetch.mp4" {
                    eprintln!("FILE_MEDIA_DEMO_UNEXPECTED_EXPIRED_FETCH");
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            };
            requests += 1;
            eprintln!("FILE_MEDIA_DEMO_REQUEST count={requests} kind={kind}");
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: public, max-age=3600\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(bytes);
        }
    });
    Ok(origin)
}
