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
        let atlas_stress = std::env::var("FLUXER_MEDIA_DEMO_ATLAS_STRESS").as_deref() == Ok("1");
        let cache_scroll = std::env::var("FLUXER_MEDIA_DEMO_CACHE_SCROLL").as_deref() == Ok("1");
        let origin = match fixture_server(atlas_stress) {
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
        let (gifv_route, width, height, duration) = if atlas_stress {
            ("gifv.webm", 640, 640, 0.16)
        } else {
            ("gifv.mp4", 480, 270, 3.0)
        };
        gifv.extra.insert("embeds".into(), json!([{
            "type":"gifv", "title":"Generated GIFV", "url":format!("{origin}/{gifv_route}"),
            "video":{"url":format!("{origin}/{gifv_route}"),"proxy_url":format!("{origin}/{gifv_route}?signature=fixture"),"width":width,"height":height,"duration":duration},
            "thumbnail":{"proxy_url":format!("{origin}/poster.png"),"width":width,"height":height}
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
        let mut messages = if cache_scroll {
            let mut messages = vec![gifv];
            for index in 1..64 {
                messages.push(Message::text(
                    Snowflake(70_002 + index),
                    channel,
                    messages[0].author.clone(),
                    format!("Offscreen media cache fixture · filler row {index}"),
                ));
            }
            messages
        } else if atlas_stress {
            (0..16)
                .map(|index| {
                    let mut message = gifv.clone();
                    message.id = Snowflake(70_000 + index);
                    message.content = format!("Short WebM atlas eviction fixture · {index}");
                    message
                })
                .collect()
        } else if std::env::var("FLUXER_MEDIA_DEMO_GIF_ONLY").as_deref() == Ok("1") {
            vec![gifv]
        } else {
            vec![video, audio, gifv, spoiler, expired]
        };
        for (index, message) in messages.iter_mut().enumerate() {
            message.timestamp = format!("2026-11-13T12:{:02}:{:02}Z", index / 60, index % 60);
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
        if cache_scroll {
            cx.spawn(async move |entity, cx| {
                Timer::after(Duration::from_secs(2)).await;
                for step in 0..12 {
                    let _ = entity.update(cx, |this, cx| {
                        this.timeline
                            .scroll_to_top_of_item(if step % 2 == 0 { 63 } else { 0 });
                        eprintln!("MEDIA_CACHE_SCROLL step={step}");
                        cx.notify();
                    });
                    Timer::after(Duration::from_millis(800)).await;
                }
                eprintln!("MEDIA_CACHE_SCROLL complete");
            })
            .detach();
        }
        if atlas_stress {
            cx.spawn(async move |entity, cx| {
                for step in 0..48 {
                    Timer::after(Duration::from_millis(450)).await;
                    if entity
                        .update(cx, |this, cx| {
                            this.timeline
                                .scroll_to_top_of_item(if step % 2 == 0 { 15 } else { 0 });
                            eprintln!("ATLAS_STRESS scroll={step}");
                            cx.notify();
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                eprintln!("ATLAS_STRESS complete");
            })
            .detach();
        }
        cx.notify();
    }
}

fn fixture_server(atlas_stress: bool) -> Result<String, String> {
    // Only reached through explicit offline --demo-media; never a live account.
    let stress_video = if atlas_stress {
        let path = std::env::var("FLUXER_MEDIA_DEMO_VIDEO")
            .map_err(|_| "Set FLUXER_MEDIA_DEMO_VIDEO to a generated 640x640 short VP8 WebM")?;
        Some(std::fs::read(path).map_err(|_| "Could not read the generated stress fixture")?)
    } else {
        None
    };
    let response_delay = std::env::var("FLUXER_MEDIA_DEMO_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|millis| Duration::from_millis(millis.min(5_000)))
        .unwrap_or_default();
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
            } else if route == "/gifv.webm" && stress_video.is_some() {
                ("video/webm", stress_video.as_deref().unwrap(), "video")
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
            if kind == "video" && !response_delay.is_zero() {
                std::thread::sleep(response_delay);
            }
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
