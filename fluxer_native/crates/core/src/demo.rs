use crate::*;

impl ClientState {
    /// Explicitly offline sample data. Never mixed with an authenticated session.
    pub fn demo() -> Self {
        let mut state = Self::default();
        let me = User {
            id: 100.into(),
            username: "you".into(),
            global_name: Some("You".into()),
            ..User::default()
        };
        let alex = User {
            id: 101.into(),
            username: "alex".into(),
            global_name: Some("Alex".into()),
            ..User::default()
        };
        let sam = User {
            id: 102.into(),
            username: "sam".into(),
            global_name: Some("Sam".into()),
            ..User::default()
        };
        let river = User {
            id: 103.into(),
            username: "river".into(),
            global_name: Some("River".into()),
            ..User::default()
        };
        state.current_user = Some(me.clone());
        for user in [&me, &alex, &sam, &river] {
            state.users.insert(user.id, user.clone());
        }
        for (id, name, owner) in [
            (1, "Fluxer Community", 101),
            (2, "The Workshop", 102),
            (3, "Friends", 100),
        ] {
            state.guilds.insert(
                id.into(),
                Guild {
                    id: id.into(),
                    name: name.into(),
                    owner_id: owner.into(),
                    roles: vec![Role {
                        id: id.into(),
                        name: "@everyone".into(),
                        permissions: Permissions::DEFAULT,
                        ..Role::default()
                    }],
                    ..Guild::default()
                },
            );
            for user in [&me, &alex, &sam, &river] {
                state.members.insert(
                    (id.into(), user.id),
                    Member {
                        user: user.clone(),
                        ..Member::default()
                    },
                );
            }
        }
        for (id, guild, name, kind, topic, position) in [
            (10, Some(1), "WELCOME", ChannelType::Category, None, 0),
            (
                11,
                Some(1),
                "general",
                ChannelType::Text,
                Some("A place for everyone. Say hello!"),
                1,
            ),
            (
                12,
                Some(1),
                "announcements",
                ChannelType::Announcement,
                Some("The latest from Fluxer"),
                2,
            ),
            (
                13,
                Some(1),
                "off-topic",
                ChannelType::Text,
                Some("Everything else"),
                3,
            ),
            (
                14,
                Some(1),
                "Lounge",
                ChannelType::Voice,
                Some("Drop in and hang out"),
                4,
            ),
            (
                15,
                Some(1),
                "ideas",
                ChannelType::Forum,
                Some("What should we build next?"),
                5,
            ),
            (
                21,
                Some(2),
                "projects",
                ChannelType::Text,
                Some("Share what you're working on"),
                0,
            ),
            (22, Some(2), "Pair programming", ChannelType::Voice, None, 1),
            (31, Some(3), "weekend-plans", ChannelType::Text, None, 0),
            (40, None, "Alex", ChannelType::Dm, None, 0),
            (
                41,
                None,
                "Personal Notes",
                ChannelType::PersonalNotes,
                None,
                1,
            ),
        ] {
            state.channels.insert(
                id.into(),
                Channel {
                    id: id.into(),
                    guild_id: guild.map(Into::into),
                    name: name.into(),
                    kind,
                    topic: topic.map(str::to_owned),
                    position,
                    ..Channel::default()
                },
            );
        }
        for (id, channel, author, content, timestamp) in [
            (
                1000,
                11,
                &alex,
                "Welcome to Fluxer Community! This is an offline preview of the native Rust client.",
                "2026-11-12T09:30:00Z",
            ),
            (
                1001,
                11,
                &sam,
                "The guild list, channels, messages and local drafts are ready to explore.",
                "2026-11-12T09:31:00Z",
            ),
            (
                1002,
                11,
                &river,
                "I love having a native app. Anyone up for a call later?",
                "2026-11-12T09:32:00Z",
            ),
            (1003, 11, &me, "Count me in! 👋", "2026-11-12T09:33:00Z"),
            (
                1010,
                12,
                &alex,
                "Native client migration is underway. The existing client remains the feature reference.",
                "2026-11-12T08:00:00Z",
            ),
            (
                1020,
                13,
                &river,
                "What's everyone listening to today?",
                "2026-11-12T09:20:00Z",
            ),
            (
                1030,
                21,
                &sam,
                "Share your latest project here.",
                "2026-11-12T09:00:00Z",
            ),
            (
                1040,
                31,
                &me,
                "How about a games night this weekend?",
                "2026-11-12T09:00:00Z",
            ),
            (
                1050,
                40,
                &alex,
                "Hey! Welcome to the native preview.",
                "2026-11-12T09:00:00Z",
            ),
        ] {
            let mut message = Message::text(id.into(), channel.into(), author.clone(), content);
            message.timestamp = timestamp.into();
            state.insert_message(message);
        }
        state.toggle_reaction(11.into(), 1000.into(), Emoji::unicode("👋"));
        state.toggle_reaction(11.into(), 1002.into(), Emoji::unicode("👍"));
        let mut rich_message = Message::text(
            1004.into(),
            11.into(),
            sam.clone(),
            "Chat selection works across wrapped lines and explicit line breaks.\nTry selecting this sentence, then copying it with Ctrl/Cmd+C. Unicode stays intact: café, 日本語, 👩🏽‍💻.",
        );
        rich_message.timestamp = "2026-11-12T09:34:00Z".into();
        rich_message.extra.insert(
            "embeds".into(),
            serde_json::json!([{
                "type": "rich",
                "title": "A richer native conversation",
                "description": "Link previews, attachments, replies and forwards now have a place in the timeline.",
                "color": 8942574,
                "footer": {"text": "Offline sample · no network content"}
            }]),
        );
        state.insert_message(rich_message);
        let mut reply = Message::text(
            1005.into(),
            11.into(),
            river.clone(),
            "Replying keeps the original message in view. 🎉",
        );
        reply.timestamp = "2026-11-12T09:35:00Z".into();
        reply.kind = 19;
        reply.message_reference = Some(MessageReference {
            channel_id: 11.into(),
            message_id: Some(1004.into()),
            guild_id: Some(1.into()),
            kind: 0,
        });
        reply.extra.insert(
            "referenced_message".into(),
            serde_json::to_value(&state.messages[&Snowflake::from(11)][&Snowflake::from(1004)])
                .expect("serialize offline reply target"),
        );
        state.insert_message(reply);
        let mut forwarded = Message::text(1006.into(), 11.into(), alex.clone(), "");
        forwarded.timestamp = "2026-11-12T09:36:00Z".into();
        forwarded.message_reference = Some(MessageReference {
            channel_id: 21.into(),
            message_id: Some(1030.into()),
            guild_id: Some(2.into()),
            kind: 1,
        });
        forwarded.extra.insert(
            "message_snapshots".into(),
            serde_json::json!([{
                "content": "Share your latest project here.",
                "timestamp": "2026-11-12T09:00:00Z",
                "type": 0,
                "flags": 0,
                "attachments": [{
                    "id": "9001", "filename": "native-client-notes.txt", "size": 2048,
                    "content_type": "text/plain", "flags": 0
                }]
            }]),
        );
        state.insert_message(forwarded);
        state.relationships.insert(
            alex.id,
            Relationship {
                id: alex.id,
                kind: 1,
                user: Some(alex.clone()),
                nickname: None,
            },
        );
        state.relationships.insert(
            sam.id,
            Relationship {
                id: sam.id,
                kind: 1,
                user: Some(sam.clone()),
                nickname: None,
            },
        );
        for user in [&me, &alex, &sam] {
            state.presences.insert(
                user.id,
                Presence {
                    user: user.clone(),
                    status: PresenceStatus::Online,
                    ..Presence::default()
                },
            );
        }
        state.voice_states.insert(
            (alex.id, "demo".into()),
            VoiceState {
                user_id: alex.id,
                guild_id: Some(1.into()),
                channel_id: Some(14.into()),
                ..VoiceState::default()
            },
        );
        state.select_channel(11.into());
        state
    }
}
