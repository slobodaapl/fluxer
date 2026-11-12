use crate::{Error, Instance, Result, instance::bounded_body};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Method, header, redirect::Policy};
use serde_json::{Value, json};
use std::{fmt, sync::Arc, time::Duration};
use url::Url;

/// An account token. Debug output is always redacted. Tokens are deliberately
/// not serializable; persistence belongs in the operating system credential store.
#[derive(Clone)]
pub struct SessionToken(Arc<str>);

impl SessionToken {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() || header::HeaderValue::from_str(&value).is_err() {
            return Err(Error::InvalidAuthResponse);
        }
        Ok(Self(Arc::from(value)))
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken([REDACTED])")
    }
}

pub enum LoginIdentifier {
    Email(String),
    Login(String),
}

#[derive(Clone, Debug)]
pub struct AuthSession {
    pub token: SessionToken,
    pub user_id: String,
    pub user: Option<Value>,
}

#[derive(Clone, Debug)]
pub enum LoginOutcome {
    Authenticated {
        token: SessionToken,
        user_id: String,
        user: Option<Value>,
    },
    Mfa {
        ticket: String,
        totp: bool,
        webauthn: bool,
        backup_codes: bool,
    },
    IpAuthorization {
        ticket: String,
        email: String,
        resend_available_in: u64,
    },
}

#[derive(Clone)]
pub struct Client {
    instance: Instance,
    http: reqwest::Client,
    token: Option<SessionToken>,
    auth_locale: header::HeaderValue,
}

impl Client {
    pub fn new(instance: Instance) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("Fluxer-Native/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            instance,
            http,
            token: None,
            auth_locale: header::HeaderValue::from_static("en-US"),
        })
    }

    pub fn with_token(mut self, token: SessionToken) -> Self {
        self.token = Some(token);
        self
    }
    pub fn instance(&self) -> &Instance {
        &self.instance
    }
    pub fn with_auth_locale(mut self, locale: impl Into<String>) -> Result<Self> {
        self.auth_locale = header::HeaderValue::from_str(&locale.into())
            .map_err(|_| Error::InvalidEndpoint("invalid authentication locale header".into()))?;
        Ok(self)
    }

    /// Generic JSON access for endpoints without a typed wrapper. Requests are
    /// pinned to this instance; redirects never forward the account token.
    /// No mutation is retried automatically, preventing accidental duplicate sends.
    pub async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        self.request_inner(method, path, body, true).await
    }

    fn route_url(&self, path: &str) -> Result<Url> {
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains('#')
            || path.contains('\\')
            || path
                .split('?')
                .next()
                .unwrap_or("")
                .split('/')
                .any(|segment| matches!(segment, "." | ".."))
        {
            return Err(Error::InvalidEndpoint(
                "API route must be an absolute path without traversal".into(),
            ));
        }
        Url::parse(&format!(
            "{}/v{}{path}",
            self.instance.api.as_str().trim_end_matches('/'),
            self.instance.api_version
        ))
        .map_err(|_| Error::InvalidEndpoint("invalid API route".into()))
    }

    async fn request_inner(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        authenticated: bool,
    ) -> Result<Value> {
        let url = self.route_url(path)?;
        let requires_origin = method != Method::GET && method != Method::HEAD;
        let mut request = self
            .http
            .request(method, url)
            .header(header::ACCEPT, "application/json")
            // RestTransport.assembleHeaders, same-origin first-party API calls.
            .header(
                "X-Fluxer-Features",
                "view_channel_members_permission,channel_threads",
            )
            .header("X-Fluxer-Capabilities", "guild_visits")
            .header(
                "X-Fluxer-Client-Installation-ID",
                crate::installation::get(),
            )
            .header(header::ACCEPT_ENCODING, "gzip, br");
        if !authenticated {
            // AuthenticationCommands.withAuthLocaleHeader (login and TOTP).
            request = request.header(header::ACCEPT_LANGUAGE, self.auth_locale.clone());
        }
        if requires_origin {
            // Match LocalAppAPIProxyRequestHandler.forward and
            // LocalAppProxyHTTPPolicy.buildAPIRequestHeaders: use the discovered
            // web app's origin, and omit Origin for GET and HEAD.
            request = request.header(
                header::ORIGIN,
                self.instance.web_app.origin().ascii_serialization(),
            );
        }
        if authenticated && let Some(token) = &self.token {
            let mut authorization = header::HeaderValue::from_str(token.expose_secret())
                .map_err(|_| Error::InvalidAuthResponse)?;
            authorization.set_sensitive(true);
            // Fluxer uses the raw session token, not an OAuth Bearer prefix.
            request = request.header(header::AUTHORIZATION, authorization);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        let retry_header = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<f64>().ok());
        let bytes = bounded_body(response, 16 * 1024 * 1024).await?;
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            match serde_json::from_slice(&bytes) {
                Ok(body) => body,
                Err(error) if status.is_success() => return Err(error.into()),
                Err(_) => Value::Null,
            }
        };
        if status.as_u16() == 429 {
            let seconds = body
                .get("retry_after")
                .and_then(Value::as_f64)
                .or(retry_header)
                .unwrap_or(1.0);
            let seconds = if seconds.is_finite() {
                seconds.clamp(0.0, 86400.0)
            } else {
                1.0
            };
            return Err(Error::RateLimited {
                retry_after: Duration::from_secs_f64(seconds),
                global: body.get("global").and_then(Value::as_bool).unwrap_or(false),
            });
        }
        if !status.is_success() {
            let code = body.get("code").map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            });
            return Err(Error::Api {
                status: status.as_u16(),
                code,
                body,
            });
        }
        Ok(body)
    }

    pub async fn login(&self, identifier: LoginIdentifier, password: &str) -> Result<LoginOutcome> {
        let mut body = json!({"password": password});
        match identifier {
            LoginIdentifier::Email(value) => body["email"] = value.into(),
            LoginIdentifier::Login(value) => body["login"] = value.into(),
        }
        let response = match self
            .request_inner(Method::POST, "/auth/login", Some(&body), false)
            .await
        {
            Ok(response) => response,
            Err(Error::Api { body, .. })
                if body.get("ip_authorization_required") == Some(&Value::Bool(true)) =>
            {
                body
            }
            Err(error) => return Err(error),
        };
        parse_login(response)
    }

    pub async fn login_mfa_totp(&self, ticket: &str, code: &str) -> Result<AuthSession> {
        let response = self
            .request_inner(
                Method::POST,
                "/auth/login/mfa/totp",
                Some(&json!({"ticket":ticket,"code":code})),
                false,
            )
            .await?;
        parse_session(response)
    }

    pub async fn logout(&self) -> Result<()> {
        self.request(Method::POST, "/auth/logout", None).await?;
        Ok(())
    }
    pub async fn current_user(&self) -> Result<Value> {
        self.request(Method::GET, "/users/@me", None).await
    }
    pub async fn guilds(&self) -> Result<Value> {
        self.request(Method::GET, "/users/@me/guilds", None).await
    }
    pub async fn private_channels(&self) -> Result<Value> {
        self.request(Method::GET, "/users/@me/channels", None).await
    }
    pub async fn guild_channels(&self, guild: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/guilds/{}/channels", segment(guild)),
            None,
        )
        .await
    }
    pub async fn channel(&self, channel: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/channels/{}", segment(channel)),
            None,
        )
        .await
    }
    pub async fn messages(&self, channel: &str, before: Option<&str>, limit: u8) -> Result<Value> {
        let mut path = format!(
            "/channels/{}/messages?limit={}",
            segment(channel),
            limit.clamp(1, 100)
        );
        if let Some(before) = before {
            path.push_str(&format!("&before={}", segment(before)));
        }
        self.request(Method::GET, &path, None).await
    }
    pub async fn send_message(
        &self,
        channel: &str,
        content: &str,
        nonce: Option<&str>,
    ) -> Result<Value> {
        let mut body = json!({"content":content});
        if let Some(nonce) = nonce {
            body["nonce"] = nonce.into();
        }
        self.request(
            Method::POST,
            &format!("/channels/{}/messages", segment(channel)),
            Some(&body),
        )
        .await
    }
    pub async fn edit_message(&self, channel: &str, message: &str, content: &str) -> Result<Value> {
        self.request(
            Method::PATCH,
            &message_path(channel, message),
            Some(&json!({"content":content})),
        )
        .await
    }
    pub async fn delete_message(&self, channel: &str, message: &str) -> Result<()> {
        self.request(Method::DELETE, &message_path(channel, message), None)
            .await?;
        Ok(())
    }
    pub async fn acknowledge(&self, channel: &str, message: &str) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("{}/ack", message_path(channel, message)),
            Some(&json!({"manual":false,"mention_count":0})),
        )
        .await
    }
    pub async fn typing(&self, channel: &str) -> Result<()> {
        self.request(
            Method::POST,
            &format!("/channels/{}/typing", segment(channel)),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn add_reaction(&self, channel: &str, message: &str, emoji: &str) -> Result<()> {
        self.request(
            Method::PUT,
            &format!(
                "{}/reactions/{}/@me",
                message_path(channel, message),
                segment(emoji)
            ),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn remove_reaction(&self, channel: &str, message: &str, emoji: &str) -> Result<()> {
        self.request(
            Method::DELETE,
            &format!(
                "{}/reactions/{}/@me",
                message_path(channel, message),
                segment(emoji)
            ),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn pins(&self, channel: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/channels/{}/messages/pins", segment(channel)),
            None,
        )
        .await
    }
    pub async fn create_dm(&self, recipient: &str) -> Result<Value> {
        self.request(
            Method::POST,
            "/users/@me/channels",
            Some(&json!({"recipients":[recipient]})),
        )
        .await
    }
    pub async fn relationships(&self) -> Result<Value> {
        self.request(Method::GET, "/users/@me/relationships", None)
            .await
    }
    pub async fn settings(&self) -> Result<Value> {
        self.request(Method::GET, "/users/@me/settings", None).await
    }
    pub async fn update_settings(&self, patch: &Value) -> Result<Value> {
        self.request(Method::PATCH, "/users/@me/settings", Some(patch))
            .await
    }
}

fn parse_session(value: Value) -> Result<AuthSession> {
    Ok(AuthSession {
        token: SessionToken::new(
            value
                .get("token")
                .and_then(Value::as_str)
                .ok_or(Error::InvalidAuthResponse)?
                .to_owned(),
        )?,
        user_id: value
            .get("user_id")
            .and_then(Value::as_str)
            .ok_or(Error::InvalidAuthResponse)?
            .to_owned(),
        user: value.get("user").filter(|user| !user.is_null()).cloned(),
    })
}

fn parse_login(value: Value) -> Result<LoginOutcome> {
    let string = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(Error::InvalidAuthResponse)
    };
    if value
        .get("ip_authorization_required")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Ok(LoginOutcome::IpAuthorization {
            ticket: string("ticket")?,
            email: string("email")?,
            resend_available_in: value
                .get("resend_available_in")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        });
    }
    if value.get("mfa").and_then(Value::as_bool) == Some(true) {
        return Ok(LoginOutcome::Mfa {
            ticket: string("ticket")?,
            totp: value.get("totp").and_then(Value::as_bool).unwrap_or(false),
            webauthn: value
                .get("webauthn")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            backup_codes: value
                .get("backup_codes")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    let session = parse_session(value)?;
    Ok(LoginOutcome::Authenticated {
        token: session.token,
        user_id: session.user_id,
        user: session.user,
    })
}

fn segment(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}
fn message_path(channel: &str, message: &str) -> String {
    format!(
        "/channels/{}/messages/{}",
        segment(channel),
        segment(message)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_redacted_and_header_safe() {
        assert!(!format!("{:?}", SessionToken::new("secret").unwrap()).contains("secret"));
        assert!(SessionToken::new("secret\nInjected: value").is_err());
    }

    #[test]
    fn api_base_path_and_version_are_preserved() {
        let client = Client::new(
            Instance::from_endpoints(
                "https://chat.example/custom/api",
                "wss://chat.example/gateway",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            client.route_url("/users/@me").unwrap().as_str(),
            "https://chat.example/custom/api/v1/users/@me"
        );
        assert!(client.route_url("//evil.example").is_err());
        assert!(client.route_url("/../token").is_err());
        assert!(message_path("1", "2").ends_with("/1/messages/2"));
        assert_eq!(segment("a/b:c😀"), "a%2Fb%3Ac%F0%9F%98%80");
    }

    #[test]
    fn login_challenges_are_not_treated_as_sessions() {
        assert!(matches!(
            parse_login(json!({"mfa":true,"ticket":"t","totp":true})).unwrap(),
            LoginOutcome::Mfa { totp: true, .. }
        ));
        assert!(parse_login(json!({"user_id":"1"})).is_err());
    }
}
