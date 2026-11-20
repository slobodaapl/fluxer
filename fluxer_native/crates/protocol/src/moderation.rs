// SPDX-License-Identifier: AGPL-3.0-or-later
//! GuildMemberCommands and GuildCommands moderation wire contracts.
use crate::{Client, Result, http::segment};
use reqwest::Method;
use serde_json::{Value, json};

impl Client {
    /// Kick without an audit reason (GuildMemberCommands.kick).
    pub async fn kick_member(&self, guild: &str, user: &str) -> Result<()> {
        self.request(Method::DELETE, &member_path(guild, user), None)
            .await?;
        Ok(())
    }

    /// Permanently ban, preserving message history (GuildCommands.banMember defaults).
    pub async fn ban_member(&self, guild: &str, user: &str) -> Result<()> {
        self.request(
            Method::PUT,
            &format!("/guilds/{}/bans/{}", segment(guild), segment(user)),
            Some(&json!({"delete_message_seconds": 0, "reason": null})),
        )
        .await?;
        Ok(())
    }

    /// List community bans, including their audit reason and expiry.
    pub async fn guild_bans(&self, guild: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/guilds/{}/bans", segment(guild)),
            None,
        )
        .await
    }

    /// Fetch one page of the guild audit log.
    pub async fn guild_audit_log(&self, guild: &str, before: Option<&str>) -> Result<Value> {
        let query = {
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            query.append_pair("limit", "50");
            if let Some(before) = before {
                query.append_pair("before", before);
            }
            query.finish()
        };
        self.request(
            Method::GET,
            &format!("/guilds/{}/audit-logs?{}", segment(guild), query),
            None,
        )
        .await
    }

    /// Revoke a community ban without supplying an audit-log reason.
    pub async fn unban_member(&self, guild: &str, user: &str) -> Result<()> {
        self.request(
            Method::DELETE,
            &format!("/guilds/{}/bans/{}", segment(guild), segment(user)),
            None,
        )
        .await?;
        Ok(())
    }

    /// Set or clear the timeout; returns the updated guild member wire object.
    pub async fn set_member_timeout(
        &self,
        guild: &str,
        user: &str,
        until: Option<&str>,
    ) -> Result<Value> {
        self.request(
            Method::PATCH,
            &member_path(guild, user),
            Some(&json!({"communication_disabled_until": until})),
        )
        .await
    }
}
fn member_path(guild: &str, user: &str) -> String {
    format!("/guilds/{}/members/{}", segment(guild), segment(user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn ban_uses_authenticated_put_and_preserves_history() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let instance = crate::Instance {
            api: base.clone(),
            gateway: base.clone(),
            web_app: base,
            api_version: 1,
            features: json!({}),
            document: json!({}),
        };
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
            }
            let head = String::from_utf8(request).unwrap();
            assert!(
                head.starts_with("PUT /v1/guilds/42/bans/99 HTTP/1.1\r\n"),
                "{head}"
            );
            assert!(
                head.to_ascii_lowercase()
                    .contains("authorization: test-session\r\n")
            );
            let length: usize = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|v| v.parse().ok())
                })
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"delete_message_seconds": 0, "reason": null})
            );
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let client = Client::new(instance)
            .unwrap()
            .with_token(crate::SessionToken::new("test-session").unwrap());
        client.ban_member("42", "99").await.unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    async fn audit_log_pages_use_cursor_and_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let instance = crate::Instance {
            api: base.clone(),
            gateway: base.clone(),
            web_app: base,
            api_version: 1,
            features: json!({}),
            document: json!({}),
        };
        let server = tokio::spawn(async move {
            for path in [
                "/v1/guilds/42/audit-logs?limit=50",
                "/v1/guilds/42/audit-logs?limit=50&before=123",
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                let head = String::from_utf8(request).unwrap();
                assert!(
                    head.starts_with(&format!("GET {path} HTTP/1.1\r\n")),
                    "{head}"
                );
                assert!(
                    head.to_ascii_lowercase()
                        .contains("authorization: test-session\r\n")
                );
                let body = r#"{"audit_log_entries":[{"id":"123","action_type":22,"user_id":"1","target_id":"2"}],"users":[{"id":"1","username":"mod"}],"webhooks":[]}"#;
                stream.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(instance)
            .unwrap()
            .with_token(crate::SessionToken::new("test-session").unwrap());
        let page = client.guild_audit_log("42", None).await.unwrap();
        assert_eq!(page["audit_log_entries"][0]["action_type"], 22);
        client.guild_audit_log("42", Some("123")).await.unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    async fn list_and_revoke_bans_use_source_routes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let instance = crate::Instance {
            api: base.clone(),
            gateway: base.clone(),
            web_app: base,
            api_version: 1,
            features: json!({}),
            document: json!({}),
        };
        let server = tokio::spawn(async move {
            for (method, path, body) in [
                (
                    "GET",
                    "/v1/guilds/42/bans",
                    r#"[{"user":{"id":"99","username":"user"},"reason":"test","moderator_id":"1","banned_at":"2020-01-01T00:00:00Z","expires_at":null}]"#,
                ),
                ("DELETE", "/v1/guilds/42/bans/99", ""),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                let head = String::from_utf8(request).unwrap();
                assert!(
                    head.starts_with(&format!("{method} {path} HTTP/1.1\r\n")),
                    "{head}"
                );
                assert!(
                    head.to_ascii_lowercase()
                        .contains("authorization: test-session\r\n")
                );
                let status = if method == "GET" {
                    "200 OK"
                } else {
                    "204 No Content"
                };
                stream.write_all(format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(instance)
            .unwrap()
            .with_token(crate::SessionToken::new("test-session").unwrap());
        let bans = client.guild_bans("42").await.unwrap();
        assert_eq!(bans[0]["reason"], "test");
        client.unban_member("42", "99").await.unwrap();
        server.await.unwrap();
    }
}
