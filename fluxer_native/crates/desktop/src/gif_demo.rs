// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generated, credential-free loopback fixtures for animated media. Proxy
//! routes intentionally return a PNG unless the client sends animated=true.
use super::*;
use fluxer_core::Attachment;
use serde_json::json;
use std::{
    io::{Cursor, Read, Write},
    net::TcpListener,
};

impl Fluxer {
    pub(super) fn maybe_demo_gifs(&mut self, cx: &mut Context<Self>) {
        if self.gif_demo_started
            || !self.demo
            || self.state.current_user.is_none()
            || !std::env::args().any(|arg| arg == "--demo-gifs")
        {
            return;
        }
        let Some(channel) = self.state.selected_channel else {
            return;
        };
        self.gif_demo_started = true;
        let (origin, gif_size) = match gif_fixture_server() {
            Ok(server) => server,
            Err(error) => {
                self.error = Some(format!("Offline GIF fixture could not start: {error}"));
                cx.notify();
                return;
            }
        };
        let author = self
            .state
            .users
            .get(&Snowflake(101))
            .cloned()
            .or_else(|| self.state.current_user.clone())
            .expect("ready demo account");
        let message = |id: u64, seconds: u8, text: String| {
            let mut message = Message::text(Snowflake(id), channel, author.clone(), text);
            message.timestamp = format!("2026-11-12T18:00:{seconds:02}Z");
            message
        };
        let mut direct = message(
            40_000,
            0,
            "Animated GIF attachment · four colors, moving block, 120 ms frames.".into(),
        );
        direct.attachments.push(gif_attachment(
            41_000,
            "direct.gif",
            &origin,
            "/direct.gif",
            "/proxy-attachment.gif",
            gif_size,
        ));
        let mut embeds = message(
            40_001,
            1,
            "GIF and GIFV embeds · their image proxies must request animated=true.".into(),
        );
        embeds.extra.insert("embeds".into(),json!([
            {"type":"image","url":format!("{origin}/direct.gif"),"thumbnail":{"url":format!("{origin}/direct.gif"),"proxy_url":format!("{origin}/proxy-embed.gif"),"width":120,"height":60,"content_type":"image/gif","flags":32,"animated":true}},
            {"type":"gifv","url":format!("{origin}/gifv-metadata"),"thumbnail":{"url":format!("{origin}/direct.gif"),"proxy_url":format!("{origin}/proxy-gifv.gif"),"width":120,"height":60,"content_type":"image/gif","flags":32,"animated":true},"video":{"url":format!("{origin}/metadata-only.mp4"),"proxy_url":format!("{origin}/metadata-only.mp4"),"width":120,"height":60,"content_type":"video/mp4","flags":32}}
        ]));
        let mut hidden = message(
            40_002,
            2,
            "Concealed GIFs below must never be fetched automatically.".into(),
        );
        let mut spoiler = gif_attachment(
            41_001,
            "SPOILER_hidden.gif",
            &origin,
            "/must-not-fetch/spoiler.gif",
            "/must-not-fetch/spoiler.gif",
            gif_size,
        );
        spoiler.flags |= 1 << 3;
        let mut sensitive = gif_attachment(
            41_002,
            "sensitive.gif",
            &origin,
            "/must-not-fetch/sensitive.gif",
            "/must-not-fetch/sensitive.gif",
            gif_size,
        );
        sensitive.flags |= 1 << 4;
        sensitive.extra.insert("nsfw".into(), json!(true));
        hidden.attachments = vec![spoiler, sensitive];
        let mut messages = BTreeMap::from([
            (direct.id, direct),
            (embeds.id, embeds),
            (hidden.id, hidden),
        ]);
        let spacers = if std::env::args().any(|arg| arg == "--demo-gifs-overflow") {
            80
        } else {
            0
        };
        for index in 0..spacers {
            let row = message(
                40_003 + index as u64,
                3 + index as u8,
                format!("Static history row {:02} · no animated content.", index + 1),
            );
            messages.insert(row.id, row);
        }
        let mut end = message(
            40_003 + spacers as u64,
            3 + spacers as u8,
            "STATIC END MARKER · all animated content is above this message.".into(),
        );
        end.attachments.push(Attachment {
            id: Snowflake(41_003),
            filename: "static-end.png".into(),
            url: Some(format!("{origin}/static-end.png")),
            content_type: Some("image/png".into()),
            width: Some(120),
            height: Some(24),
            ..Default::default()
        });
        messages.insert(end.id, end);
        self.state.messages.insert(channel, messages);
        if std::env::args().any(|arg| arg == "--demo-gifs-paused") {
            self.state.user_settings = json!({"gif_auto_play":false});
        }
        self.media_endpoint = Some(origin);
        self.pane = Pane::Chat;
        self.status = "OFFLINE GIF FIXTURE · generated loopback media only".into();
        self.timeline.scroll_to_top_of_item(0);
        // READY's pending scroll-to-bottom is applied after active-item scroll
        // during the first layout. Position this fixture once that has settled.
        cx.spawn(async move |entity, cx| {
            Timer::after(Duration::from_millis(250)).await;
            let _ = entity.update(cx, |view, cx| {
                if view.state.selected_channel == Some(channel) {
                    view.timeline.scroll_to_top_of_item(0);
                    eprintln!("GIF_DEMO_SCROLL_START");
                    cx.notify();
                }
            });
        })
        .detach();
        // Optional, fixture-only transition: exercise clipping without sending
        // pointer/keyboard events to any other desktop application.
        if let Some(delay) = std::env::var("FLUXER_GIF_DEMO_SCROLL_MS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
        {
            let last = self
                .state
                .channel_messages(channel)
                .count()
                .saturating_sub(1);
            cx.spawn(async move |entity, cx| {
                Timer::after(Duration::from_millis(delay)).await;
                let _ = entity.update(cx, |view, cx| {
                    view.timeline.scroll_to_top_of_item(last);
                    eprintln!("GIF_DEMO_SCROLL_END");
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }
}

fn gif_attachment(
    id: u64,
    name: &str,
    origin: &str,
    path: &str,
    proxy: &str,
    size: u64,
) -> Attachment {
    Attachment {
        id: Snowflake(id),
        filename: name.into(),
        size,
        url: Some(format!("{origin}{path}")),
        content_type: Some("image/gif".into()),
        width: Some(120),
        height: Some(60),
        flags: 1 << 5,
        extra: BTreeMap::from([("proxy_url".into(), json!(format!("{origin}{proxy}")))]),
    }
}
fn gif_frame(index: u32) -> image::RgbaImage {
    let colors = [
        [30, 70, 170, 255],
        [20, 150, 70, 255],
        [130, 35, 170, 255],
        [180, 80, 20, 255],
    ];
    image::RgbaImage::from_fn(120, 60, |x, y| {
        let block = (12 + index * 24..30 + index * 24).contains(&x) && (20..38).contains(&y);
        image::Rgba(if block {
            [245, 245, 245, 255]
        } else {
            colors[index as usize]
        })
    })
}
fn animated_bytes() -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder
            .set_repeat(image::codecs::gif::Repeat::Infinite)
            .map_err(|error| error.to_string())?;
        encoder
            .encode_frames((0..4).map(|index| {
                image::Frame::from_parts(
                    gif_frame(index),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(120, 1),
                )
            }))
            .map_err(|error| error.to_string())?;
    }
    Ok(bytes)
}
fn png_bytes(image: image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(bytes.into_inner())
}

fn gif_fixture_server() -> Result<(String, u64), String> {
    let gif = animated_bytes()?;
    let gif_size = gif.len() as u64;
    let poster = png_bytes(gif_frame(0))?;
    let marker = png_bytes(image::RgbaImage::from_pixel(
        120,
        24,
        image::Rgba([90, 90, 90, 255]),
    ))?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let trace = std::env::var_os("FLUXER_GIF_PERF").is_some();
    std::thread::Builder::new().name("fluxer-offline-gif-fixture".into()).spawn(move||{
        let mut requests=0u64;
        for mut socket in listener.incoming().flatten(){
            let _=socket.set_read_timeout(Some(Duration::from_secs(2)));let _=socket.set_write_timeout(Some(Duration::from_secs(2)));
            let mut bytes=[0u8;4096];let Ok(length)=socket.read(&mut bytes)else{continue;};
            let request=String::from_utf8_lossy(&bytes[..length]);let mut first=request.lines().next().unwrap_or_default().split_whitespace();let method=first.next().unwrap_or_default();let target=first.next().unwrap_or_default();
            let parsed=reqwest::Url::parse(&format!("http://localhost{target}"));
            let path=target.split('?').next().unwrap_or_default();
            let animated=parsed.as_ref().is_ok_and(|url|url.query_pairs().any(|(key,value)|key=="animated"&&value=="true"));
            let proxy=matches!(path,"/proxy-attachment.gif"|"/proxy-embed.gif"|"/proxy-gifv.gif");
            let direct=path=="/direct.gif";
            let (status,mime,body):(&str,&str,&[u8])=if method!="GET"{("405 Method Not Allowed","text/plain",b"GET only")}
                else if path.starts_with("/must-not-fetch/"){
                    let case=if path=="/must-not-fetch/spoiler.gif"{"spoiler"}else{"sensitive"};eprintln!("[fluxer-demo-gifs] UNEXPECTED_BLOCKED_FETCH case={case}");("403 Forbidden","text/plain",b"Concealed GIF must not be fetched")
                }else if path=="/metadata-only.mp4"{eprintln!("[fluxer-demo-gifs] UNEXPECTED_VIDEO_FETCH");("404 Not Found","text/plain",b"Video metadata only; use the GIF image proxy")}
                else if direct || proxy&&animated{("200 OK","image/gif",&gif)}
                else if proxy{("200 OK","image/png",&poster)}
                else if path=="/static-end.png"{("200 OK","image/png",&marker)}
                else{("404 Not Found","text/plain",b"No such GIF fixture asset")};
            if trace&&(direct||proxy){requests+=1;let kind=match path{"/direct.gif"=>"original","/proxy-attachment.gif"=>"attachment","/proxy-embed.gif"=>"embed",_=>"gifv"};eprintln!("[fluxer-demo-gifs] request={requests} kind={kind} animated={animated} response={mime}");}
            let header=format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: public, max-age=3600\r\nConnection: close\r\n\r\n",body.len());let _=socket.write_all(header.as_bytes());let _=socket.write_all(body);
        }
    }).map_err(|error|error.to_string())?;
    Ok((format!("http://{address}"), gif_size))
}

#[cfg(test)]
mod tests {
    use super::animated_bytes;
    use image::{AnimationDecoder, codecs::gif::GifDecoder};
    use std::io::Cursor;
    #[test]
    fn generated_fixture_has_real_distinct_120ms_frames_and_moving_pixels() {
        let decoder = GifDecoder::new(Cursor::new(animated_bytes().unwrap())).unwrap();
        let frames = decoder.into_frames().collect_frames().unwrap();
        assert_eq!(frames.len(), 4);
        for frame in &frames {
            assert_eq!(frame.delay().numer_denom_ms(), (120, 1));
            assert_eq!(frame.buffer().dimensions(), (120, 60));
        }
        for adjacent in frames.windows(2) {
            assert_ne!(
                adjacent[0].buffer().get_pixel(0, 0),
                adjacent[1].buffer().get_pixel(0, 0)
            );
        }
        assert_eq!(frames[0].buffer().get_pixel(15, 25).0, [245, 245, 245, 255]);
        assert_ne!(frames[1].buffer().get_pixel(15, 25).0, [245, 245, 245, 255]);
        assert_eq!(frames[1].buffer().get_pixel(39, 25).0, [245, 245, 245, 255]);
    }
}
