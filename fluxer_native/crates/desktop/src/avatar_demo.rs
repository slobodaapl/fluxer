// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local-only imagery fixture: no session restoration, voice connection or capture.
use super::*;

impl Fluxer {
    pub(super) fn maybe_demo_avatars(&mut self, cx: &mut Context<Self>) {
        if !self.demo
            || self.state.current_user.is_none()
            || !std::env::args().any(|arg| arg == "--demo-avatars")
        {
            return;
        }
        if self.avatar_demo_endpoint.is_none() {
            self.avatar_demo_endpoint = fixture_endpoint();
        }
        self.media_endpoint = self.avatar_demo_endpoint.clone();
        if let Some(guild) = self.state.guilds.get_mut(&Snowflake(1)) {
            guild.icon = Some("community".into());
        }
        if let Some(guild) = self.state.guilds.get_mut(&Snowflake(2)) {
            guild.icon = Some("missing".into());
        }
        for (id, hash) in [(100, "self"), (101, "global"), (102, "sam")] {
            if let Some(user) = self.state.users.get_mut(&Snowflake(id)) {
                user.avatar = Some(hash.into());
            }
        }
        if let Some(user) = self.state.current_user.as_mut() {
            user.avatar = Some("self".into());
        }
        if let Some(member) = self.state.members.get_mut(&(Snowflake(1), Snowflake(101))) {
            member.avatar = Some("member".into());
        }
        if let Some(member) = self.state.members.get_mut(&(Snowflake(1), Snowflake(103))) {
            member.avatar = Some("should-not-be-requested".into());
            member.profile_flags = Some(1);
        }
        for id in [100, 101, 102, 103] {
            self.state.voice_states.insert(
                (Snowflake(id), "avatar-demo".into()),
                fluxer_core::VoiceState {
                    user_id: Snowflake(id),
                    guild_id: Some(Snowflake(1)),
                    channel_id: Some(Snowflake(14)),
                    connection_id: Some("avatar-demo".into()),
                    ..Default::default()
                },
            );
        }
        self.state
            .voice_states
            .remove(&(Snowflake(101), "demo".into()));
        self.state.selected_channel = Some(Snowflake(14));
        self.state.voice.channel_id = Some(Snowflake(14));
        self.voice_status = "Offline voice preview".into();
        self.voice_connecting = false;
        self.active_speakers.insert("user_101_avatar-demo".into());
        cx.notify();
    }
}
fn fixture_endpoint() -> Option<String> {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let address = listener.local_addr().ok()?;
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let mut request = [0; 4096];
            let Ok(count) = stream.read(&mut request) else {
                continue;
            };
            let line = String::from_utf8_lossy(&request[..count]);
            let bytes: Option<&[u8]> = if line.starts_with("GET /icons/1/community.webp?") {
                Some(include_bytes!(
                    "../../../../fluxer_app/src/media/images/avatars/0.png"
                ))
            } else if line.starts_with("GET /guilds/1/users/101/avatars/member.webp?") {
                Some(include_bytes!(
                    "../../../../fluxer_app/src/media/images/avatars/2.png"
                ))
            } else if line.starts_with("GET /avatars/101/global.webp?") {
                Some(include_bytes!(
                    "../../../../fluxer_app/src/media/images/avatars/1.png"
                ))
            } else if line.starts_with("GET /avatars/102/sam.webp?") {
                Some(include_bytes!(
                    "../../../../fluxer_app/src/media/images/avatars/3.png"
                ))
            } else if line.starts_with("GET /avatars/100/self.webp?") {
                Some(include_bytes!(
                    "../../../../fluxer_app/src/media/images/avatars/4.png"
                ))
            } else {
                None
            };
            let status = if bytes.is_some() {
                "200 OK"
            } else {
                "404 Not Found"
            };
            let bytes = bytes.unwrap_or_default();
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(bytes);
        }
    });
    Some(format!("http://{address}"))
}
