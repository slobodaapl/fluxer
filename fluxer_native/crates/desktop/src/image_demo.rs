// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explicit offline fixture. Only generated PNGs and a static article are
//! served, on an ephemeral loopback port, without credentials or remote IO.
use super::*;
use fluxer_core::Attachment;
use serde_json::json;
use std::{
    io::{Cursor, Read, Write},
    net::TcpListener,
};

impl Fluxer {
    pub(super) fn maybe_demo_images(&mut self, cx: &mut Context<Self>) {
        if self.image_demo_started
            || !self.demo
            || self.state.current_user.is_none()
            || !std::env::args().any(|arg| arg == "--demo-images")
        {
            return;
        }
        let Some(channel) = self.state.selected_channel else {
            return;
        };
        self.image_demo_started = true;
        let large = std::env::args().any(|arg| arg == "--demo-images-large");
        let (landscape_width, landscape_height) = landscape_dimensions(large);
        let origin = match image_fixture_server(large) {
            Ok(origin) => origin,
            Err(error) => {
                self.error = Some(format!("Offline image fixture could not start: {error}"));
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
        let make_message = |id: u64, second: u8, text: &str| {
            let mut message = Message::text(Snowflake(id), channel, author.clone(), text);
            message.timestamp = format!("2026-11-12T18:00:{second:02}Z");
            message
        };
        let mut direct = make_message(
            30_000,
            0,
            "Click either image to view it inside Fluxer. Arrow keys browse this message's images.",
        );
        direct.attachments = vec![
            fixture_attachment(
                31_000,
                "landscape.png",
                &origin,
                "/landscape.png",
                landscape_width,
                landscape_height,
            ),
            fixture_attachment(31_001, "portrait.png", &origin, "/portrait.png", 90, 150),
        ];
        if std::env::args().any(|arg| arg == "--demo-image-error") {
            direct.attachments.push(fixture_attachment(
                31_006,
                "missing.png",
                &origin,
                "/missing.png",
                150,
                80,
            ));
        }
        let mut embed = make_message(
            30_001,
            1,
            "Embedded image: the picture and linked article have different destinations.",
        );
        embed.extra.insert("embeds".into(),json!([{"type":"article","title":"Offline linked article","url":format!("{origin}/article"),"image":{"url":format!("{origin}/embed.png"),"proxy_url":format!("{origin}/embed.png"),"width":120,"height":60}}]));
        let mut forwarded = make_message(30_002, 2, "Forwarded image");
        forwarded.extra.insert("message_snapshots".into(),json!([{"content":"A small forwarded picture.","flags":0,"timestamp":"2026-11-12T17:00:00Z","attachments":[fixture_attachment(31_002,"forwarded.png",&origin,"/forwarded.png",120,60)]}]));
        let mut blocked = make_message(
            30_003,
            3,
            "These concealed and expired images must not be fetched automatically.",
        );
        let mut spoiler = fixture_attachment(
            31_003,
            "SPOILER_hidden.png",
            &origin,
            "/must-not-fetch/spoiler.png",
            120,
            60,
        );
        spoiler.flags = 1 << 3;
        let mut sensitive = fixture_attachment(
            31_004,
            "sensitive.png",
            &origin,
            "/must-not-fetch/sensitive.png",
            120,
            60,
        );
        sensitive.flags = 1 << 4;
        sensitive.extra.insert("nsfw".into(), json!(true));
        let mut expired = fixture_attachment(
            31_005,
            "expired.png",
            &origin,
            "/must-not-fetch/expired.png",
            120,
            60,
        );
        expired.extra.insert("expired".into(), json!(true));
        expired
            .extra
            .insert("expires_at".into(), json!("2020-01-01T00:00:00Z"));
        blocked.attachments = vec![spoiler, sensitive, expired];
        self.state.messages.insert(
            channel,
            [direct, embed, forwarded, blocked]
                .into_iter()
                .map(|message| (message.id, message))
                .collect(),
        );
        self.media_endpoint = Some(origin);
        self.pane = Pane::Chat;
        self.status = "OFFLINE IMAGE FIXTURE · generated loopback images only".into();
        self.timeline.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }
}

fn fixture_attachment(
    id: u64,
    name: &str,
    origin: &str,
    path: &str,
    width: u32,
    height: u32,
) -> Attachment {
    let url = format!("{origin}{path}");
    Attachment {
        id: Snowflake(id),
        filename: name.into(),
        size: (width * height * 4) as u64,
        url: Some(url.clone()),
        content_type: Some("image/png".into()),
        width: Some(width),
        height: Some(height),
        extra: BTreeMap::from([("proxy_url".into(), json!(url))]),
        ..Default::default()
    }
}

fn pattern_png(width: u32, height: u32, variant: u8) -> Result<Vec<u8>, String> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| {
        let border = x < 3 || y < 3 || x + 3 >= width || y + 3 >= height;
        let grid = x % 24 < 2 || y % 24 < 2;
        let center = x > width / 4 && x < width * 3 / 4 && y > height / 4 && y < height * 3 / 4;
        image::Rgba(if border {
            [236, 231, 249, 255]
        } else if center {
            [100, 205, 170, 255]
        } else if grid {
            [145, 119, 214, 255]
        } else {
            [35 + variant * 12, 29 + variant * 8, 54 + variant * 16, 255]
        })
    });
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(output.into_inner())
}

fn landscape_dimensions(large: bool) -> (u32, u32) {
    if large { (1800, 1000) } else { (180, 100) }
}

fn image_fixture_server(large: bool) -> Result<String, String> {
    let (width, height) = landscape_dimensions(large);
    let assets = BTreeMap::from([
        ("/landscape.png", pattern_png(width, height, 0)?),
        ("/portrait.png", pattern_png(90, 150, 1)?),
        ("/embed.png", pattern_png(120, 60, 2)?),
        ("/forwarded.png", pattern_png(120, 60, 3)?),
    ]);
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    std::thread::Builder::new().name("fluxer-offline-image-fixture".into()).spawn(move||{
        for mut socket in listener.incoming().flatten(){
            let _=socket.set_read_timeout(Some(Duration::from_secs(2)));let _=socket.set_write_timeout(Some(Duration::from_secs(2)));
            let mut request=[0u8;4096];let Ok(length)=socket.read(&mut request)else{continue;};
            let request=String::from_utf8_lossy(&request[..length]);let mut first_line=request.lines().next().unwrap_or_default().split_whitespace();
            let method=first_line.next().unwrap_or_default();let path=first_line.next().unwrap_or_default().split('?').next().unwrap_or_default();
            let (status,mime,body):(&str,&str,&[u8])=if method!="GET"{("405 Method Not Allowed","text/plain",b"GET only")}
                else if path.starts_with("/must-not-fetch/"){
                    let case=match path{"/must-not-fetch/spoiler.png"=>"spoiler","/must-not-fetch/sensitive.png"=>"sensitive","/must-not-fetch/expired.png"=>"expired",_=>"unknown"};
                    eprintln!("[fluxer-demo-images] UNEXPECTED_BLOCKED_FETCH case={case}");
                    ("403 Forbidden","text/plain",b"Concealed fixture must not be fetched")
                }else if let Some(bytes)=assets.get(path){("200 OK","image/png",bytes)}
                else if path=="/article"{("200 OK","text/html; charset=utf-8",b"<!doctype html><title>Offline fixture article</title><p>This is a generated local article. Clicking its image should open the native image viewer.</p>")}
                else{("404 Not Found","text/plain",b"No such fixture asset")};
            let header=format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: public, max-age=3600\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\n\r\n",body.len());
            let _=socket.write_all(header.as_bytes());let _=socket.write_all(body);
        }
    }).map_err(|error|error.to_string())?;
    Ok(format!("http://{address}"))
}
