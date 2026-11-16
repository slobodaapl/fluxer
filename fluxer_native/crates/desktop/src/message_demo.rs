// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source-shaped, explicitly offline conversation presentation fixture.
use super::*;
use chrono::{DateTime, Duration as DateDuration};

impl Fluxer {
    pub(super) fn maybe_demo_messages(&mut self, cx: &mut Context<Self>) {
        if self.message_demo_started
            || !self.demo
            || self.state.current_user.is_none()
            || !(std::env::args()
                .any(|arg| matches!(arg.as_str(), "--demo-messages" | "--demo-mentions"))
                || crate::emoji_time_demo::enabled())
        {
            return;
        }
        let Some(channel) = self.state.selected_channel else {
            return;
        };
        self.message_demo_started = true;
        // Synthetic links for the explicit offline fixture, never a live host.
        self.message_links.configure_instance(&serde_json::json!({
            "endpoints":{"webapp":"https://profile-review.example"},
            "features":{"self_hosted":true}
        }));
        let base = DateTime::parse_from_rfc3339("2026-11-11T18:00:00Z").expect("fixture time");
        let alex = self
            .state
            .users
            .get(&Snowflake(101))
            .cloned()
            .expect("demo Alex");
        let sam = self
            .state
            .users
            .get(&Snowflake(102))
            .cloned()
            .expect("demo Sam");
        let messages = [
            (&alex, 0, "Conversations should be easy to scan."),
            (
                &alex,
                60,
                "Consecutive messages share a header, with a subtle timestamp in the gutter.",
            ),
            (&alex, 419, "This is still in the first seven-minute group."),
            (
                &alex,
                421,
                "This starts a fresh group, measured from the original group start.",
            ),
            (
                &sam,
                480,
                "**Bold**, *italic*, __underline__, ~~strikethrough~~ and **nested *formatting* together**.",
            ),
            (
                &sam,
                500,
                "Inline `code` keeps its formatting; escaped \\*asterisks\\* stay literal. café 日本語 👋🏽",
            ),
            (
                &sam,
                530,
                "# A readable heading\nParagraphs, mentions <@101>, channel <#12> and role <@&555>.",
            ),
            (
                &sam,
                560,
                "> A quoted thought\n> with a second line.\n\nBack to the conversation.",
            ),
            (
                &sam,
                590,
                "- A short list\n- Another item\n  - A nested item\n\n1. First step\n2. Second step",
            ),
            (
                &alex,
                660,
                "```rust\nfn main() {\n    println!(\"native message rendering\");\n}\n```",
            ),
            (
                &alex,
                680,
                "A spoiler stays concealed: ||private spoiler text||. Selecting and copying visible text should omit it.",
            ),
            (
                &alex,
                700,
                "Custom emoji: <:native_wave:123456789012345678> and standard Unicode 👋. **Formatting remains selectable.**",
            ),
            (
                &sam,
                760,
                "[A labeled external link](https://example.invalid/review) and https://example.invalid/plain. Drag to select; a click shows the destination warning.",
            ),
            (
                &sam,
                800,
                "A reply starts a new group and keeps the original message accessible.",
            ),
            (
                &sam,
                820,
                "The next message can continue the reply author's group.",
            ),
            (&alex, 86_400, "A new calendar day gets a quiet divider."),
            (&alex, 86_430, "And the new day's messages group normally."),
        ];
        let mention_fixture = std::env::args().any(|arg| arg == "--demo-mentions");
        let mut fixture = BTreeMap::new();
        for (index, (author, seconds, content)) in messages.into_iter().enumerate() {
            let id = Snowflake(20_000 + index as u64);
            let mut message = Message::text(id, channel, author.clone(), content);
            message.timestamp = (base + DateDuration::seconds(seconds)).to_rfc3339();
            if index == 13 {
                message.kind = 19;
                message.message_reference = Some(fluxer_core::MessageReference {
                    channel_id: channel,
                    message_id: Some(Snowflake(20_000)),
                    guild_id: Some(Snowflake(1)),
                    kind: 0,
                });
            }
            if index == 5 {
                message.edited_timestamp = Some(message.timestamp.clone());
            }
            fixture.insert(id, message);
        }
        if mention_fixture {
            for (index, content) in [
                "Populated: <@101> <@&555> adjacent text; unresolved <@999999> <@&999999>.",
                "Empty cache: <@&556>. Long role: <@&557> at a narrow window edge.",
                "Concealed ||<@101> <@&555>|| and code `<@101> <@&555>`.",
            ]
            .into_iter()
            .enumerate()
            {
                let id = Snowflake(21_000 + index as u64);
                let mut message = Message::text(id, channel, sam.clone(), content);
                message.timestamp =
                    (base + DateDuration::seconds(90_000 + index as i64)).to_rfc3339();
                fixture.insert(id, message);
            }
            let mut long = String::from("Keyboard reveal: <@101> at the beginning.\n\n");
            for index in 0..32 {
                long.push_str(&format!(
                    "Line {index}: scrolling should keep keyboard focus visible.\n"
                ));
            }
            long.push_str("\n```rust\nfn example() { println!(\"code stays inert\"); }\n```\n\n| One | Two | Three | Four | Five | Six |\n| --- | --- | --- | --- | --- | --- |\n| text | text | text | text | text | <@&555> |\n\nAt the end <@101>.");
            let id = Snowflake(21_010);
            let mut message = Message::text(id, channel, sam.clone(), &long);
            message.timestamp = (base + DateDuration::seconds(90_100)).to_rfc3339();
            fixture.insert(id, message);
            let member = self
                .state
                .members
                .entry((Snowflake(1), alex.id))
                .or_insert_with(|| fluxer_core::Member {
                    user: alex.clone(),
                    ..Default::default()
                });
            if !member.roles.contains(&Snowflake(555)) {
                member.roles.push(Snowflake(555));
            }
        }
        self.state.messages.insert(channel, fixture);
        if let Some(guild) = self.state.guilds.get_mut(&Snowflake(1)) {
            guild.roles.push(fluxer_core::Role {
                id: Snowflake(555),
                name: "Design".into(),
                color: 0x9b87f5,
                ..Default::default()
            });
            if mention_fixture {
                guild.roles.push(fluxer_core::Role {
                    id: Snowflake(556),
                    name: "No cached assignments".into(),
                    ..Default::default()
                });
                guild.roles.push(fluxer_core::Role {
                    id: Snowflake(557),
                    name: "A very long role name for narrow window placement and truncation".into(),
                    color: 0x5fbda9,
                    ..Default::default()
                });
            }
        }
        self.media_endpoint = if crate::emoji_time_demo::enabled() {
            crate::emoji_time_demo::populate(self, channel, &alex)
        } else {
            fixture_emoji_endpoint()
        };
        self.timeline.set_offset(point(px(0.), px(0.)));
        if std::env::var("FLUXER_MESSAGE_LINK_DEMO").as_deref() == Ok("1")
            && let (Some(handle), Some(message)) = (
                cx.windows().first().copied(),
                self.state.channel_messages(channel).next().cloned(),
            )
        {
            cx.spawn(async move |entity, cx| {
                Timer::after(Duration::from_millis(500)).await;
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = entity.update(cx, |view, cx| {
                        view.open_message_menu(
                            message,
                            point(px(430.), px(250.)),
                            message_actions::MessageMenuContext::Timeline,
                            window,
                            cx,
                        )
                    });
                });
            })
            .detach();
        }
        cx.notify();
    }
}

/// Serve only generated PNG bytes on loopback, with no credentials or external
/// requests. Kept separate from the authenticated discovery/transport path.
fn fixture_emoji_endpoint() -> Option<String> {
    use std::{
        io::{Cursor, Read, Write},
        net::TcpListener,
    };
    let image = image::RgbaImage::from_fn(24, 24, |x, y| {
        if (5..19).contains(&x) && (5..19).contains(&y) {
            image::Rgba([137, 115, 238, 255])
        } else {
            image::Rgba([60, 196, 160, 255])
        }
    });
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    let bytes = bytes.into_inner();
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let address = listener.local_addr().ok()?;
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let mut request = [0u8; 4096];
            if stream.read(&mut request).is_ok() {
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&bytes);
            }
        }
    });
    Some(format!("http://{address}"))
}
