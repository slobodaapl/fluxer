//! Channel webhook management. Tokens are deliberately excluded from Debug output.
use crate::{Client, Error, Method, Result, http::segment};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt;

#[derive(Clone, Deserialize)]
pub struct Webhook {
    pub id: String,
    pub guild_id: String,
    pub channel_id: String,
    pub name: String,
    pub avatar: Option<String>,
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default)]
    token: Option<String>,
}

impl fmt::Debug for Webhook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Webhook")
            .field("id", &self.id)
            .field("guild_id", &self.guild_id)
            .field("channel_id", &self.channel_id)
            .field("name", &self.name)
            .field("avatar", &self.avatar)
            .field("kind", &self.kind)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}
impl Webhook {
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }
    /// Construct only for an explicit copy action; never display or log the result.
    pub fn copy_url(&self, public_api_endpoint: &str) -> Option<String> {
        let token = self.token.as_ref()?;
        Some(format!(
            "{}/webhooks/{}/{}",
            public_api_endpoint.trim_end_matches('/'),
            segment(&self.id),
            segment(token)
        ))
    }
}

// API error bodies may echo credentials. Keep status and code but discard the body.
fn safe_error(error: Error) -> Error {
    match error {
        Error::Api { status, code, .. } => Error::Api {
            status,
            code,
            body: Value::Null,
        },
        other => other,
    }
}
impl Client {
    pub async fn channel_webhooks(&self, channel: &str) -> Result<Vec<Webhook>> {
        let body = self
            .request(
                Method::GET,
                &format!("/channels/{}/webhooks", segment(channel)),
                None,
            )
            .await
            .map_err(safe_error)?;
        Ok(serde_json::from_value(body)?)
    }
    pub async fn create_webhook(&self, channel: &str, name: &str) -> Result<Webhook> {
        let body = self
            .request(
                Method::POST,
                &format!("/channels/{}/webhooks", segment(channel)),
                Some(&json!({"name": name, "avatar": null})),
            )
            .await
            .map_err(safe_error)?;
        Ok(serde_json::from_value(body)?)
    }
    pub async fn update_webhook(
        &self,
        id: &str,
        name: &str,
        avatar: Option<&str>,
    ) -> Result<Webhook> {
        let body = self
            .request(
                Method::PATCH,
                &format!("/webhooks/{}", segment(id)),
                Some(&json!({"name": name, "avatar": avatar})),
            )
            .await
            .map_err(safe_error)?;
        Ok(serde_json::from_value(body)?)
    }
    pub async fn delete_webhook(&self, id: &str) -> Result<()> {
        self.request(Method::DELETE, &format!("/webhooks/{}", segment(id)), None)
            .await
            .map_err(safe_error)?;
        Ok(())
    }
}
