// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{Client, Error, Method, Result, Url};
use fluxer_core::{ChannelType, Snowflake};
use serde::Deserialize;
use serde_json::json;

#[derive(Clone, Debug, Deserialize)]
pub struct InviteChannel {
    pub id: Snowflake,
    #[serde(rename = "type")]
    pub kind: ChannelType,
    pub name: Option<String>,
    #[serde(default)]
    pub recipients: Vec<InviteRecipient>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InviteRecipient {
    // ChannelPartialRecipientResponse only guarantees the username.
    pub username: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InviteGuild {
    pub id: Snowflake,
    pub name: String,
    pub icon: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InvitePreview {
    pub code: String,
    #[serde(rename = "type")]
    pub kind: u8,
    pub channel: InviteChannel,
    pub guild: Option<InviteGuild>,
    pub member_count: u32,
    pub presence_count: Option<u32>,
    pub expires_at: Option<String>,
    pub temporary: bool,
}

const OFFICIAL_INVITE_BASES: &[&str] = &[
    "https://fluxer.app/invite",
    "https://canary.fluxer.app/invite",
    "https://web.fluxer.app/invite",
    "https://web.canary.fluxer.app/invite",
    "https://fluxer.com/invite",
    "https://canary.fluxer.com/invite",
    "https://fluxer.gg",
    "https://fluxer.gg/invite",
];

pub fn valid_invite_code(code: &str) -> bool {
    (2..=32).contains(&code.len())
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn code_from_link(input: &str, invite_base: &str, self_hosted: bool) -> Option<String> {
    if valid_invite_code(input) {
        return Some(input.to_owned());
    }

    let input = if input.contains("://") {
        Url::parse(input).ok()?
    } else {
        Url::parse(&format!("https://{input}")).ok()?
    };
    if !matches!(input.scheme(), "https" | "http")
        || !input.username().is_empty()
        || input.password().is_some()
    {
        return None;
    }
    // CodeLinkUtils.ts matches the configured invite endpoint. Official
    // aliases are also recognized on Fluxer's own instance.
    std::iter::once(invite_base)
        .chain(
            OFFICIAL_INVITE_BASES
                .iter()
                .copied()
                .filter(|_| !self_hosted),
        )
        .find_map(|base| {
            let base = Url::parse(base).ok()?;
            if base.host_str() != input.host_str() || base.port() != input.port() {
                return None;
            }
            let route = if input.path() == "/" {
                input
                    .fragment()
                    .filter(|fragment| fragment.starts_with('/'))
                    .unwrap_or("/")
            } else {
                input.path()
            };
            let path = route.strip_prefix(base.path().trim_end_matches('/'))?;
            let code = path.strip_prefix('/')?.trim_end_matches('/');
            valid_invite_code(code).then(|| code.to_owned())
        })
}

impl Client {
    pub fn resolve_invite_code(&self, input: &str) -> Option<String> {
        let base = self.instance().document["endpoints"]["invite"]
            .as_str()
            .unwrap_or_default();
        let self_hosted = self.instance().features["self_hosted"]
            .as_bool()
            .unwrap_or(true);
        code_from_link(input.trim(), base, self_hosted)
    }

    pub async fn create_channel_invite(&self, channel: Snowflake) -> Result<String> {
        let value = self
            .request(
                Method::POST,
                &format!("/channels/{channel}/invites"),
                Some(&json!({"max_age": 604800, "max_uses": 0, "temporary": false})),
            )
            .await?;
        let code = value["code"]
            .as_str()
            .filter(|code| valid_invite_code(code))
            .ok_or(Error::InvalidInviteResponse)?;
        let base = self.instance().document["endpoints"]["invite"]
            .as_str()
            .ok_or(Error::InvalidInviteResponse)?;
        let base = Url::parse(base).map_err(|_| Error::InvalidInviteResponse)?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(Error::InvalidInviteResponse);
        }
        Ok(format!("{}/{code}", base.as_str().trim_end_matches('/')))
    }

    pub async fn preview_invite(&self, code: &str) -> Result<InvitePreview> {
        self.invite_request(Method::GET, code).await
    }

    pub async fn accept_invite(&self, code: &str) -> Result<InvitePreview> {
        self.invite_request(Method::POST, code).await
    }

    async fn invite_request(&self, method: Method, code: &str) -> Result<InvitePreview> {
        if !valid_invite_code(code) {
            return Err(Error::InvalidEndpoint("invalid invite code".into()));
        }
        let body = (method == Method::POST).then(|| json!({}));
        let value = self
            .request(method, &format!("/invites/{code}"), body.as_ref())
            .await?;
        let preview: InvitePreview = serde_json::from_value(value)?;
        match (preview.kind, &preview.guild, preview.channel.kind) {
            (0, Some(_), kind) if !kind.is_private() => Ok(preview),
            (1, None, ChannelType::GroupDm) => Ok(preview),
            _ => Err(Error::InvalidInviteResponse),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_codes_and_instance_links_without_changing_case() {
        assert_eq!(code_from_link("Aa-91", "", true).as_deref(), Some("Aa-91"));
        let local = "https://chat.example:8443/invite";
        assert_eq!(
            code_from_link("chat.example:8443/invite/Aa-91?ref=share", local, true).as_deref(),
            Some("Aa-91")
        );
        assert_eq!(code_from_link("https://fluxer.gg/Aa-91", local, true), None);
        assert_eq!(
            code_from_link(
                "https://fluxer.gg/Aa-91",
                "https://fluxer.app/invite",
                false
            )
            .as_deref(),
            Some("Aa-91")
        );
        for input in [
            "https://chat.example/invite/Aa-91",
            "https://chat.example:8443/other/Aa-91",
            "https://chat.example:8443/invite/code/extra",
            "https://other.example/invite/Aa-91",
            "https://chat.example:8443@other.example/invite/Aa-91",
            "https://chat.example:8443/invite/ab%2Fcd",
        ] {
            assert_eq!(code_from_link(input, local, true), None, "{input}");
        }
    }

    #[tokio::test]
    async fn invite_wire_preview_then_accept_uses_original_code_and_authenticated_json() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let instance = crate::Instance {
            api: base.clone(),
            gateway: base.clone(),
            web_app: base,
            api_version: 1,
            features: json!({}),
            document: json!({}),
        };
        let server = tokio::spawn(async move {
            let responses = [
                json!({
                    "code":"ab-c", "type":1, "channel":{"type":3,"id":"42","name":null,
                        "recipients":[{"username":"friend"}]},
                    "temporary":false, "expires_at":null, "member_count":2
                }),
                json!({
                    "code":"ab-c", "type":1, "channel":{"type":3,"id":"42","name":null},
                    "temporary":false, "expires_at":null, "member_count":3
                }),
            ];
            for (method, response) in ["GET", "POST"].into_iter().zip(responses) {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                let head = String::from_utf8(request).unwrap();
                assert!(
                    head.starts_with(&format!("{method} /v1/invites/Ab-C HTTP/1.1\r\n")),
                    "{head}"
                );
                assert!(
                    head.to_ascii_lowercase()
                        .contains("authorization: test-session\r\n"),
                    "{head}"
                );
                if method == "POST" {
                    assert!(
                        head.to_ascii_lowercase()
                            .contains("content-type: application/json\r\n"),
                        "{head}"
                    );
                    let mut body = [0; 2];
                    stream.read_exact(&mut body).await.unwrap();
                    assert_eq!(&body, b"{}");
                }
                let response = response.to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(instance)
            .unwrap()
            .with_token(crate::SessionToken::new("test-session").unwrap());
        let preview = client.preview_invite("Ab-C").await.unwrap();
        assert_eq!(preview.channel.recipients[0].username, "friend");
        let joined = client.accept_invite("Ab-C").await.unwrap();
        assert_eq!(joined.member_count, 3);
        server.await.unwrap();
    }
    #[tokio::test]
    async fn create_invite_uses_classic_defaults_and_instance_endpoint() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let instance = crate::Instance {
            api: base.clone(),
            gateway: base.clone(),
            web_app: base,
            api_version: 1,
            features: json!({}),
            document: json!({"endpoints":{"invite":"https://chat.example/invite"}}),
        };
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
            }
            let head = String::from_utf8(request).unwrap();
            assert!(head.starts_with("POST /v1/channels/42/invites HTTP/1.1\r\n"));
            assert!(
                head.to_ascii_lowercase()
                    .contains("authorization: test-session\r\n")
            );
            let length: usize = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|n| n.parse().ok())
                })
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                json!({"max_age":604800,"max_uses":0,"temporary":false})
            );
            let response = r#"{"code":"Ab-C"}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        });
        let client = Client::new(instance)
            .unwrap()
            .with_token(crate::SessionToken::new("test-session").unwrap());
        assert_eq!(
            client.create_channel_invite(Snowflake(42)).await.unwrap(),
            "https://chat.example/invite/Ab-C"
        );
        server.await.unwrap();
    }
}
