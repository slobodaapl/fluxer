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

/// An initiating device's handoff proof. It never enters the browser URL or logs.
#[derive(Clone)]
pub struct HandoffPollSecret(Arc<str>);
impl HandoffPollSecret {
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for HandoffPollSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HandoffPollSecret([REDACTED])")
    }
}

/// Native currently has no registered return deep link, so initiation deliberately
/// omits return_uri and follows the existing desktop CODE return path.
#[derive(Clone, Debug)]
pub struct DesktopHandoffSession {
    pub code: String,
    pub expires_at: String,
    pub poll_secret: Option<HandoffPollSecret>,
    expires_at_ms: i64,
    api: Url,
    api_version: u64,
    web_app: Url,
    active: bool,
}
impl DesktopHandoffSession {
    pub fn expires_at_millis(&self) -> i64 {
        self.expires_at_ms
    }
    pub fn is_expired(&self) -> bool {
        unix_millis() >= self.expires_at_ms
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopHandoffStatus {
    Pending,
    Completed,
    Denied,
    Expired,
}

#[derive(Clone)]
pub struct DesktopHandoffPoll {
    pub status: DesktopHandoffStatus,
    pub token: Option<SessionToken>,
    pub user_id: Option<String>,
    pub user: Option<Value>,
}
impl fmt::Debug for DesktopHandoffPoll {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DesktopHandoffPoll")
            .field("status", &self.status)
            .field("token", &self.token)
            .field("user_id", &self.user_id)
            .field("user_present", &self.user.is_some())
            .finish()
    }
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

/// Proofs are deliberately not Debug or serializable. TOTP also carries backup codes.
pub enum SessionRevocationProof<'a> {
    Password(&'a str),
    Totp(&'a str),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SessionRevocationOutcome {
    Revoked,
    Challenge(crate::SudoChallenge),
}

#[derive(Clone)]
pub struct Client {
    instance: Instance,
    http: reqwest::Client,
    token: Option<SessionToken>,
    auth_locale: header::HeaderValue,
    handoff_http: reqwest::Client,
}

impl Client {
    pub fn new(instance: Instance) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("Fluxer-Native/", env!("CARGO_PKG_VERSION")))
            .build()?;
        // BrowserHandoff.requestJSON is a main-process request, not renderer
        // RestTransport: no feature/installation/locale/auth or user-agent headers.
        let handoff_http = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(15))
            .no_gzip()
            .no_brotli()
            .build()?;
        Ok(Self {
            instance,
            http,
            token: None,
            auth_locale: header::HeaderValue::from_static("en-US"),
            handoff_http,
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
    pub async fn gifs(&self, feed: crate::GifFeed, locale: &str) -> Result<crate::GifResults> {
        let mut request = self
            .api_request_using(&self.http, Method::GET, feed.endpoint(), true)?
            .query(&[("locale", locale)]);
        if let crate::GifFeed::Search(query) = &feed {
            request = request.query(&[("q", query)]);
        }
        let response = request.send().await?;
        let discovery = self.instance.document.get("gif");
        let configured = |key| discovery.and_then(|value| value.get(key));
        let header = |key| {
            response
                .headers()
                .get(key)
                .and_then(|value| value.to_str().ok())
        };
        let provider = header("x-fluxer-gif-provider")
            .or_else(|| configured("provider").and_then(Value::as_str))
            .unwrap_or("klipy");
        let display_name = header("x-fluxer-gif-provider-display-name")
            .filter(|name| !name.is_empty())
            .or_else(|| {
                (configured("provider").and_then(Value::as_str) == Some(provider))
                    .then(|| configured("display_name").and_then(Value::as_str))
                    .flatten()
            })
            .unwrap_or(if provider == "tenor" {
                "Tenor"
            } else {
                "Klipy"
            });
        let provider = crate::GifProvider {
            display_name: display_name.to_owned(),
            attribution_required: header("x-fluxer-gif-provider-attribution-required")
                .and_then(|value| value.parse().ok())
                .or_else(|| configured("attribution_required").and_then(Value::as_bool))
                .unwrap_or(false),
        };
        let body = Self::parse_api_response(response).await?;
        let (gifs, categories) = if feed == crate::GifFeed::Featured {
            #[derive(serde::Deserialize)]
            struct Featured {
                gifs: Vec<crate::Gif>,
                categories: Vec<crate::GifCategory>,
            }
            let featured: Featured = serde_json::from_value(body)?;
            (featured.gifs, featured.categories)
        } else {
            (serde_json::from_value(body)?, Vec::new())
        };
        Ok(crate::GifResults {
            provider,
            gifs,
            categories,
        })
    }

    pub async fn register_gif_share(&self, id: &str, query: &str, locale: &str) -> Result<()> {
        self.request(
            Method::POST,
            "/gifs/register-share",
            Some(&json!({"id": id, "q": query, "locale": locale})),
        )
        .await?;
        Ok(())
    }

    pub async fn channel_slowmode(&self, channel: &str) -> Result<Value> {
        self.request(Method::GET, &format!("/channels/{channel}/slowmode"), None)
            .await
    }

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

    pub(crate) fn upload_api_request(
        &self,
        http: &reqwest::Client,
        method: Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder> {
        self.api_request_using(http, method, path, true)
    }

    fn api_request_using(
        &self,
        http: &reqwest::Client,
        method: Method,
        path: &str,
        authenticated: bool,
    ) -> Result<reqwest::RequestBuilder> {
        let url = self.route_url(path)?;
        let requires_origin = method != Method::GET && method != Method::HEAD;
        let mut request = http
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
                crate::installation::get()?,
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
        Ok(request.header(
            header::USER_AGENT,
            concat!("Fluxer-Native/", env!("CARGO_PKG_VERSION")),
        ))
    }

    async fn request_inner(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        authenticated: bool,
    ) -> Result<Value> {
        let mut request = self.api_request_using(&self.http, method, path, authenticated)?;
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        Self::parse_api_response(response).await
    }

    pub(crate) async fn parse_api_response(response: reqwest::Response) -> Result<Value> {
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

    /// AuthenticationCommands.forgotPassword: never includes session authorization.
    pub async fn forgot_password(&self, email: &str) -> Result<()> {
        self.request_inner(
            Method::POST,
            "/auth/forgot",
            Some(&json!({"email": email})),
            false,
        )
        .await?;
        Ok(())
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
            Err(error) => {
                if let Error::Api { status, body, .. } = &error
                    && let Some(challenge) = ip_authorization_error(*status, body)
                {
                    return Ok(challenge);
                }
                return Err(error);
            }
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

    pub async fn resend_ip_authorization(&self, ticket: &str) -> Result<()> {
        self.request_inner(
            Method::POST,
            "/auth/ip-authorization/resend",
            Some(&json!({"ticket":ticket})),
            false,
        )
        .await?;
        Ok(())
    }

    pub async fn poll_ip_authorization(&self, ticket: &str) -> Result<Option<AuthSession>> {
        // Endpoints.AUTH_IP_AUTHORIZATION_POLL uses URLSearchParams (form
        // encoding), rather than path-segment encoding, for the ticket.
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("ticket", ticket)
            .finish();
        let response = self
            .request_inner(
                Method::GET,
                &format!("/auth/ip-authorization/poll?{query}"),
                None,
                false,
            )
            .await?;
        // IpAuthorizationScreen keeps polling incomplete or partial responses.
        if response.get("completed").and_then(Value::as_bool) != Some(true)
            || response
                .get("token")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            || response
                .get("user_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Ok(None);
        }
        parse_session(response).map(Some)
    }

    /// Exact BrowserHandoff.ts CODE initiation: no return_uri, auth, locale or
    /// renderer REST headers. The browser performs CAPTCHA and account approval.
    pub async fn initiate_desktop_handoff(&self) -> Result<DesktopHandoffSession> {
        let (status, payload) = self
            .handoff_request(Method::POST, "/auth/handoff/initiate", None)
            .await?;
        if status != 200 && status != 201 {
            return Err(handoff_http_error(status));
        }
        let payload = payload
            .filter(Value::is_object)
            .ok_or(Error::InvalidAuthResponse)?;
        let code = payload
            .get("code")
            .and_then(Value::as_str)
            .filter(|code| valid_handoff_code(code))
            .ok_or(Error::InvalidAuthResponse)?
            .to_owned();
        let expires_at = bounded_handoff_string(payload.get("expires_at"), 512)
            .ok_or(Error::InvalidAuthResponse)?;
        let expires_at_ms = chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_err(|_| Error::InvalidAuthResponse)?
            .timestamp_millis();
        let poll_secret = bounded_handoff_string(payload.get("poll_secret"), 512)
            .filter(|secret| {
                secret.len() <= 256
                    && secret
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            })
            .map(|secret| HandoffPollSecret(Arc::from(secret)));
        Ok(DesktopHandoffSession {
            code,
            expires_at: expires_at.into(),
            poll_secret,
            expires_at_ms,
            api: self.instance.api.clone(),
            api_version: self.instance.api_version,
            web_app: self.instance.web_app.clone(),
            active: true,
        })
    }

    /// Poll a session bound to its initiating instance. As in BrowserHandoff.ts
    /// 306-325, POST falls back only on 404/405 and permanently clears its proof.
    pub async fn poll_desktop_handoff(
        &self,
        session: &mut DesktopHandoffSession,
    ) -> Result<DesktopHandoffPoll> {
        if !session.active
            || session.api != self.instance.api
            || session.api_version != self.instance.api_version
            || session.web_app != self.instance.web_app
        {
            return Err(Error::InvalidAuthResponse);
        }
        if session.is_expired() {
            session.active = false;
            session.poll_secret = None;
            return Ok(empty_handoff_poll(DesktopHandoffStatus::Expired));
        }
        if !valid_handoff_code(&session.code) {
            return Err(Error::InvalidAuthResponse);
        }
        // Validated codes are ASCII alphanumeric/hyphen, all left intact by
        // BrowserHandoff.ts encodeURIComponent (including the display hyphen).
        let path = format!("/auth/handoff/{}/status", session.code);
        let mut response = None;
        if let Some(secret) = &session.poll_secret {
            let attempt = self
                .handoff_request(
                    Method::POST,
                    &path,
                    Some(&json!({"poll_secret":secret.expose_secret()})),
                )
                .await?;
            if matches!(attempt.0, 404 | 405) {
                session.poll_secret = None;
            } else {
                response = Some(attempt);
            }
        }
        let (status, payload) = match response {
            Some(response) => response,
            None => self.handoff_request(Method::GET, &path, None).await?,
        };
        if status != 200 {
            return Err(handoff_http_error(status));
        }
        let result = parse_handoff_poll(payload)?;
        if result.status != DesktopHandoffStatus::Pending {
            session.active = false;
            session.poll_secret = None;
        }
        Ok(result)
    }

    /// useBrowserLoginHandoff.ts:105-122. Only the public display code travels to
    /// the browser; poll secret and session token are never included.
    pub fn desktop_handoff_url(
        &self,
        code: &str,
        prefill: Option<&LoginIdentifier>,
    ) -> Result<Url> {
        if !valid_handoff_code(code) {
            return Err(Error::InvalidAuthResponse);
        }
        let mut url = self
            .instance
            .web_app
            .join("/login")
            .map_err(|_| Error::InvalidEndpoint("invalid handoff web app endpoint".into()))?;
        {
            let mut params = url.query_pairs_mut();
            params
                .append_pair("handoff", "1")
                .append_pair("code", code)
                .append_pair("api", &self.instance.api.origin().ascii_serialization());
            if let Some(prefill) = prefill {
                let (field, value) = match prefill {
                    LoginIdentifier::Email(value) => ("email", value),
                    LoginIdentifier::Login(value) => ("login", value),
                };
                if !value.is_empty() {
                    params.append_pair(field, value);
                }
            }
        }
        Ok(url)
    }

    async fn handoff_request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Option<Value>)> {
        // BrowserHandoff.ts:114-140,334-364. API-origin admission is implicit in
        // route_url; the POST Origin header itself is the discovered web app.
        if !(1..=1000).contains(&self.instance.api_version)
            || self.instance.api.as_str().len() > 2048
            || self.instance.web_app.as_str().len() > 2048
        {
            return Err(Error::InvalidEndpoint(
                "invalid desktop handoff instance".into(),
            ));
        }
        let mut request = self
            .handoff_http
            .request(method.clone(), self.route_url(path)?)
            .header(header::ACCEPT, "application/json");
        if method == Method::POST {
            request = request.header(
                header::ORIGIN,
                self.instance.web_app.origin().ascii_serialization(),
            );
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let bytes = bounded_body(response, 64 * 1024).await?;
        let payload = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .filter(Value::is_object);
        Ok((status, payload))
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
    /// Create a basic guild channel using the same payload as ChannelCommands.create.
    pub async fn create_guild_channel(
        &self,
        guild: &str,
        name: &str,
        kind: u16,
        parent: Option<&str>,
    ) -> Result<Value> {
        let voice = kind == 2;
        let body = serde_json::json!({"name": name, "type": kind, "parent_id": parent,
            "url": null, "bitrate": if voice { Some(64000) } else { None },
            "user_limit": if voice { Some(0) } else { None },
            "voice_connection_limit": if voice { Some(5) } else { None }});
        self.request(
            Method::POST,
            &format!("/guilds/{}/channels", segment(guild)),
            Some(&body),
        )
        .await
    }
    pub async fn delete_channel(&self, channel: &str) -> Result<Value> {
        self.request(
            Method::DELETE,
            &format!("/channels/{}", segment(channel)),
            None,
        )
        .await
    }
    pub async fn update_basic_channel(
        &self,
        channel: &str,
        name: &str,
        topic: Option<&str>,
    ) -> Result<Value> {
        let mut body = serde_json::json!({"name": name});
        if let Some(topic) = topic {
            body["topic"] = serde_json::json!(topic);
        }
        self.request(
            Method::PATCH,
            &format!("/channels/{}", segment(channel)),
            Some(&body),
        )
        .await
    }
    pub async fn set_channel_overwrite(
        &self,
        channel: &str,
        overwrite: &fluxer_core::PermissionOverwrite,
    ) -> Result<Value> {
        self.request(Method::PUT, &format!("/channels/{}/permissions/{}", segment(channel), segment(&overwrite.id.to_string())), Some(&serde_json::json!({"type": overwrite.kind, "allow": overwrite.allow, "deny": overwrite.deny}))).await
    }
    pub async fn channel(&self, channel: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/channels/{}", segment(channel)),
            None,
        )
        .await
    }
    /// Start a standalone public or announcement thread in a text channel.
    pub async fn create_public_thread(
        &self,
        channel: &str,
        name: &str,
        announcement: bool,
    ) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("/channels/{}/threads", segment(channel)),
            Some(&json!({"name": name, "type": if announcement { 10 } else { 11 }})),
        )
        .await
    }

    /// List archived public threads, optionally before an archive timestamp.
    pub async fn archived_public_threads(
        &self,
        channel: &str,
        before: Option<&str>,
    ) -> Result<Value> {
        let mut path = format!("/channels/{}/threads/archived/public", segment(channel));
        if let Some(before) = before {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("before", before)
                .finish();
            path.push('?');
            path.push_str(&query);
        }
        self.request(Method::GET, &path, None).await
    }

    /// Join or leave the current user's thread membership.
    pub async fn set_thread_membership(&self, channel: &str, join: bool) -> Result<Value> {
        self.request(
            if join { Method::PUT } else { Method::DELETE },
            &format!("/channels/{}/thread-members/@me", segment(channel)),
            None,
        )
        .await
    }

    /// Update thread lifecycle fields using the channel PATCH endpoint.
    pub async fn update_thread(
        &self,
        channel: &str,
        archived: Option<bool>,
        locked: Option<bool>,
    ) -> Result<Value> {
        let mut body = serde_json::Map::new();
        if let Some(value) = archived {
            body.insert("archived".into(), json!(value));
        }
        if let Some(value) = locked {
            body.insert("locked".into(), json!(value));
        }
        self.request(
            Method::PATCH,
            &format!("/channels/{}", segment(channel)),
            Some(&Value::Object(body)),
        )
        .await
    }

    pub async fn create_forum_post(
        &self,
        channel: &str,
        name: &str,
        content: &str,
        tags: &[String],
        archive: Option<u64>,
    ) -> Result<Value> {
        let body = forum_post_body(name, content, tags, archive);
        self.send_forum_post_body(channel, body).await
    }

    pub async fn send_forum_post_body(&self, channel: &str, body: Value) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("/channels/{}/threads", segment(channel)),
            Some(&body),
        )
        .await
    }

    pub async fn messages(&self, channel: &str, before: Option<&str>, limit: u8) -> Result<Value> {
        self.messages_page(channel, before, None, None, limit).await
    }

    pub async fn send_message(
        &self,
        channel: &str,
        content: &str,
        nonce: Option<&str>,
    ) -> Result<Value> {
        self.send_referenced_message(channel, content, nonce, None, false)
            .await
    }

    /// SharedMessageSchemas.ts / MessageCommands.forward: type 0 replies,
    /// type 1 asks the server to generate a forward snapshot.
    pub async fn send_referenced_message(
        &self,
        channel: &str,
        content: &str,
        nonce: Option<&str>,
        reference: Option<&fluxer_core::MessageReference>,
        mention_author: bool,
    ) -> Result<Value> {
        let body = message_body(content, nonce, reference, mention_author);
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
        // ReadStates.postAckEntries first uses the authoritative endpoint and
        // falls back only for missing routes (404/405), never on other errors.
        match self
            .request(
                Method::POST,
                "/read-states/ack",
                Some(&json!({"read_states":[{"channel_id":channel,"message_id":message}]})),
            )
            .await
        {
            Err(Error::Api {
                status: 404 | 405, ..
            }) => {
                self.request(
                    Method::POST,
                    "/read-states/ack-bulk",
                    Some(&json!({"read_states":[{"channel_id":channel,"message_id":message}]})),
                )
                .await?;
                Ok(Value::Null)
            }
            result => result,
        }
    }
    /// Decline an incoming call; classic sends an empty object for reject.
    pub async fn reject_call(&self, channel: &str) -> Result<()> {
        self.request(
            Method::POST,
            &format!("/channels/{channel}/call/stop-ringing"),
            Some(&json!({})),
        )
        .await?;
        Ok(())
    }

    /// Ring or stop ringing the specified private-call recipients.
    pub async fn call_ringing(
        &self,
        channel: &str,
        recipients: &[String],
        stop: bool,
    ) -> Result<()> {
        let action = if stop { "stop-ringing" } else { "ring" };
        self.request(
            Method::POST,
            &format!("/channels/{channel}/call/{action}"),
            Some(&json!({"recipients": recipients})),
        )
        .await?;
        Ok(())
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
        self.pins_page(channel, None).await
    }

    pub async fn create_dm(&self, recipient: &str) -> Result<Value> {
        self.request(
            Method::POST,
            "/users/@me/channels",
            Some(&json!({"recipient_id":recipient})),
        )
        .await
    }
    pub async fn create_group_dm(&self, recipients: &[String]) -> Result<Value> {
        self.request(
            Method::POST,
            "/users/@me/channels",
            Some(&json!({"recipients": recipients})),
        )
        .await
    }
    pub async fn add_group_recipient(&self, channel: &str, user: &str) -> Result<()> {
        self.request(
            Method::PUT,
            &format!(
                "/channels/{}/recipients/{}",
                segment(channel),
                segment(user)
            ),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn remove_group_recipient(&self, channel: &str, user: &str) -> Result<()> {
        self.request(
            Method::DELETE,
            &format!(
                "/channels/{}/recipients/{}",
                segment(channel),
                segment(user)
            ),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn relationships(&self) -> Result<Value> {
        self.request(Method::GET, "/users/@me/relationships", None)
            .await
    }
    /// Submit one explicitly confirmed session revocation. This operation may
    /// succeed without a challenge. Callers must confirm before calling it and
    /// must not automatically retry transport failures.
    pub async fn revoke_auth_session(
        &self,
        session_id_hash: &str,
        proof: Option<SessionRevocationProof<'_>>,
    ) -> Result<SessionRevocationOutcome> {
        if session_id_hash.is_empty()
            || !session_id_hash
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || self.token.is_none()
        {
            return Err(Error::InvalidEndpoint("invalid session revocation".into()));
        }
        let mut body = json!({"session_id_hashes": [session_id_hash]});
        match proof {
            Some(SessionRevocationProof::Password(value)) if !value.is_empty() => {
                body["password"] = value.into();
            }
            Some(SessionRevocationProof::Totp(value)) if !value.is_empty() => {
                body["mfa_method"] = "totp".into();
                body["mfa_code"] = value.into();
            }
            Some(_) => return Err(Error::InvalidEndpoint("empty verification proof".into())),
            None => {}
        }
        match self
            .request(Method::POST, "/auth/sessions/logout", Some(&body))
            .await
        {
            Ok(_) => Ok(SessionRevocationOutcome::Revoked),
            Err(Error::Api {
                status, ref body, ..
            }) if crate::SudoChallenge::from_response(status, body).is_some() => {
                Ok(SessionRevocationOutcome::Challenge(
                    crate::SudoChallenge::from_response(status, body).unwrap(),
                ))
            }
            Err(error) => Err(error),
        }
    }

    pub async fn connections(&self) -> Result<Value> {
        self.request(Method::GET, "/users/@me/connections", None)
            .await
    }

    pub async fn update_connection_visibility(
        &self,
        kind: &str,
        id: &str,
        flags: i64,
    ) -> Result<Value> {
        let path = format!("/users/@me/connections/{}/{}", segment(kind), segment(id));
        self.request(
            Method::PATCH,
            &path,
            Some(&serde_json::json!({"visibility_flags": flags})),
        )
        .await
    }

    pub async fn auth_sessions(&self) -> Result<Value> {
        self.request(Method::GET, "/auth/sessions", None).await
    }

    pub async fn settings(&self) -> Result<Value> {
        self.request(Method::GET, "/users/@me/settings", None).await
    }
    pub async fn update_settings(&self, patch: &Value) -> Result<Value> {
        self.request(Method::PATCH, "/users/@me/settings", Some(patch))
            .await
    }
}

// ResponseInspection.ipAuthorizationRequiredResponseFromError: modern error
// envelopes put the challenge under details.context, with legacy top-level
// context accepted only when the status/code match the documented challenge.
fn ip_authorization_error(status: u16, body: &Value) -> Option<LoginOutcome> {
    if status != 403 || body.get("code")?.as_str()? != "IP_AUTHORIZATION_REQUIRED" {
        return None;
    }
    let context = body
        .pointer("/details/context")
        .filter(|value| value.is_object())
        .unwrap_or(body);
    let ticket = context
        .get("ticket")?
        .as_str()
        .filter(|value| !value.is_empty())?
        .to_owned();
    let email = context
        .get("email")?
        .as_str()
        .filter(|value| !value.is_empty())?
        .to_owned();
    let resend_available_in = match context.get("resend_available_in").and_then(Value::as_f64) {
        Some(value) if (0.0..=9_007_199_254_740_991.0).contains(&value) && value.fract() == 0.0 => {
            value as u64
        }
        Some(_) => return None,
        None => 30,
    };
    Some(LoginOutcome::IpAuthorization {
        ticket,
        email,
        resend_available_in,
    })
}

fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
fn valid_handoff_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
fn bounded_handoff_string(value: Option<&Value>, max: usize) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.encode_utf16().count() <= max)
}
fn handoff_http_error(status: u16) -> Error {
    // Main-process handoff errors expose the status, not response tokens/proofs.
    Error::Api {
        status,
        code: None,
        body: Value::Null,
    }
}
fn empty_handoff_poll(status: DesktopHandoffStatus) -> DesktopHandoffPoll {
    DesktopHandoffPoll {
        status,
        token: None,
        user_id: None,
        user: None,
    }
}
fn parse_handoff_user(value: Option<&Value>) -> Option<Value> {
    let value = value.filter(|value| value.is_object())?;
    let username = bounded_handoff_string(value.get("username"), 512)?;
    let discriminator = bounded_handoff_string(value.get("discriminator"), 512)?;
    let mut user = json!({"username":username,"discriminator":discriminator,
        "global_name":bounded_handoff_string(value.get("global_name"),512),
        "avatar":bounded_handoff_string(value.get("avatar"),512)});
    if value.get("email").is_some() {
        user["email"] = json!(bounded_handoff_string(value.get("email"), 512));
    }
    Some(user)
}
fn parse_handoff_poll(payload: Option<Value>) -> Result<DesktopHandoffPoll> {
    let payload = payload
        .filter(Value::is_object)
        .ok_or(Error::InvalidAuthResponse)?;
    let status = match payload.get("status").and_then(Value::as_str) {
        Some("completed") => DesktopHandoffStatus::Completed,
        Some("expired") => DesktopHandoffStatus::Expired,
        Some("denied") => DesktopHandoffStatus::Denied,
        _ => DesktopHandoffStatus::Pending,
    };
    if status != DesktopHandoffStatus::Completed {
        return Ok(empty_handoff_poll(status));
    }
    let token =
        bounded_handoff_string(payload.get("token"), 4096).ok_or(Error::InvalidAuthResponse)?;
    let user_id =
        bounded_handoff_string(payload.get("user_id"), 512).ok_or(Error::InvalidAuthResponse)?;
    Ok(DesktopHandoffPoll {
        status,
        token: Some(SessionToken::new(token)?),
        user_id: Some(user_id.into()),
        user: parse_handoff_user(payload.get("user")),
    })
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

pub(crate) fn segment(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}
pub(crate) fn message_body(
    content: &str,
    nonce: Option<&str>,
    reference: Option<&fluxer_core::MessageReference>,
    mention_author: bool,
) -> Value {
    let mut body = json!({});
    if has_visible_message_content(content) {
        body["content"] = content.into();
    }
    if let Some(nonce) = nonce {
        body["nonce"] = nonce.into();
    }
    if let Some(reference) = reference {
        let mut value = json!({"channel_id":reference.channel_id, "message_id":reference.message_id, "type":reference.kind});
        if let Some(guild) = reference.guild_id {
            value["guild_id"] = json!(guild);
        }
        body["message_reference"] = value;
        if reference.kind == 0 {
            body["allowed_mentions"] = json!({"replied_user":mention_author});
        }
    }
    body
}

// MessageValidators.ts hasVisibleContent uses ECMAScript trim for each
// codepoint, then its explicit invisible-content set. Rust is_whitespace also
// includes U+0085, which ECMAScript intentionally does not trim.
fn has_visible_message_content(content: &str) -> bool {
    content.chars().any(|ch| {
        !matches!(ch as u32,
            0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a |
            0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 |
            0x0000 | 0x00ad | 0x034f | 0x061c | 0x115f | 0x1160 |
            0x17b4 | 0x17b5 | 0x180e | 0x200b..=0x200f | 0x202a..=0x202e |
            0x2060..=0x2069 | 0x2800 | 0x3164 | 0xfe00..=0xfe0f | 0xfeff |
            0xffa0 | 0xe0100..=0xe01ef
        )
    })
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

    #[test]
    fn ip_challenge_matches_response_inspection_status_context_and_integer_rules() {
        let modern = json!({"code":"IP_AUTHORIZATION_REQUIRED","details":{"context":{
            "ticket":"ticket", "email":"masked@example.test", "resend_available_in":12
        }}});
        assert!(matches!(
            ip_authorization_error(403, &modern),
            Some(LoginOutcome::IpAuthorization {
                resend_available_in: 12,
                ..
            })
        ));
        assert!(ip_authorization_error(401, &modern).is_none());
        let mut legacy =
            json!({"code":"IP_AUTHORIZATION_REQUIRED","ticket":"ticket","email":"email"});
        assert!(matches!(
            ip_authorization_error(403, &legacy),
            Some(LoginOutcome::IpAuthorization {
                resend_available_in: 30,
                ..
            })
        ));
        for invalid in [json!(-1), json!(0.5), json!(9_007_199_254_740_992_u64)] {
            legacy["resend_available_in"] = invalid;
            assert!(ip_authorization_error(403, &legacy).is_none());
        }
        legacy["code"] = json!("INVALID_API_ORIGIN");
        assert!(ip_authorization_error(403, &legacy).is_none());
    }

    #[test]
    fn handoff_public_url_uses_declared_webapp_and_api_origin_only() {
        let mut instance =
            Instance::from_endpoints("https://api.example/custom/api", "wss://gateway.example")
                .unwrap();
        instance.web_app = Url::parse("https://browser.example").unwrap();
        let client = Client::new(instance).unwrap();
        let url = client
            .desktop_handoff_url(
                "ABC-123",
                Some(&LoginIdentifier::Email("name+tag@example.test".into())),
            )
            .unwrap();
        assert_eq!(
            url.origin().ascii_serialization(),
            "https://browser.example"
        );
        assert_eq!(url.path(), "/login");
        let pairs: std::collections::BTreeMap<_, _> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(pairs["handoff"], "1");
        assert_eq!(pairs["code"], "ABC-123");
        assert_eq!(pairs["api"], "https://api.example");
        assert_eq!(pairs["email"], "name+tag@example.test");
        assert_eq!(pairs.len(), 4);
        assert!(client.desktop_handoff_url("../injected", None).is_err());
        let url = client
            .desktop_handoff_url("ABC-123", Some(&LoginIdentifier::Login("username".into())))
            .unwrap();
        assert!(
            url.query_pairs()
                .any(|(k, v)| k == "login" && v == "username")
        );
    }

    #[test]
    fn handoff_status_projection_and_debug_match_main_process_contract() {
        for status in [json!("pending"), json!("future-status"), Value::Null] {
            let poll =
                parse_handoff_poll(Some(json!({"status":status,"token":"must-not-be-adopted"})))
                    .unwrap();
            assert_eq!(poll.status, DesktopHandoffStatus::Pending);
            assert!(poll.token.is_none());
        }
        assert!(parse_handoff_poll(Some(json!({"status":"completed","token":"token"}))).is_err());
        let poll = parse_handoff_poll(Some(json!({"status":"completed","token":"private-session-token","user_id":"100","user":{
            "id":"100","username":"alex","discriminator":"0001","global_name":"Alex","avatar":null,"email":"fixture@example.test","ignored_secret":"do-not-copy"
        }}))).unwrap();
        assert_eq!(poll.status, DesktopHandoffStatus::Completed);
        assert_eq!(poll.user.as_ref().unwrap()["username"], "alex");
        assert!(poll.user.as_ref().unwrap().get("id").is_none());
        assert!(poll.user.as_ref().unwrap().get("ignored_secret").is_none());
        let debug = format!("{poll:?}");
        assert!(!debug.contains("private-session-token"));
        assert!(!debug.contains("fixture@example.test"));
        let secret = HandoffPollSecret(Arc::from("private-poll-proof"));
        assert!(!format!("{secret:?}").contains("private-poll-proof"));
        assert!(
            parse_handoff_poll(Some(
                json!({"status":"completed","token":"t".repeat(4097),"user_id":"100"})
            ))
            .is_err()
        );
    }

    #[derive(Debug)]
    struct HandoffRequest {
        method: String,
        path: String,
        headers: std::collections::BTreeMap<String, String>,
        body: Vec<u8>,
    }
    async fn handoff_server(
        responses: Vec<(u16, Value)>,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<HandoffRequest>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                let mut chunk = [0_u8; 4096];
                let end = loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&chunk[..n]);
                    if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let head = String::from_utf8_lossy(&data[..end]);
                let mut lines = head.split("\r\n");
                let mut first = lines.next().unwrap().split_whitespace();
                let method = first.next().unwrap().to_owned();
                let path = first.next().unwrap().to_owned();
                let headers: std::collections::BTreeMap<String, String> = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let length = headers
                    .get("content-length")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                while data.len() - end < length {
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&chunk[..n]);
                }
                recorded.lock().unwrap().push(HandoffRequest {
                    method,
                    path,
                    headers,
                    body: data[end..end + length].to_vec(),
                });
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 {status} Status\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (base, requests, task)
    }
    fn handoff_test_client(base: &str) -> Client {
        let mut instance =
            Instance::from_endpoints(&format!("{base}/api"), "ws://127.0.0.1/gateway").unwrap();
        instance.web_app = Url::parse("https://browser.example").unwrap();
        Client::new(instance)
            .unwrap()
            .with_token(SessionToken::new("existing-account-token").unwrap())
            .with_auth_locale("fr")
            .unwrap()
    }
    fn handoff_initiate_payload() -> Value {
        json!({"code":"ABC-123","expires_at":"2099-01-01T00:00:00Z","poll_secret":"private_poll_proof","return_method":"deep_link"})
    }

    #[tokio::test]
    async fn handoff_socket_code_path_uses_main_process_headers_and_remembers_get_fallback() {
        let (base, requests, server) = handoff_server(vec![
            (201, handoff_initiate_payload()),
            (405, Value::Null),
            (200, json!({"status":"pending"})),
            (
                200,
                json!({"status":"completed","token":"new-session-token","user_id":"100"}),
            ),
        ])
        .await;
        let client = handoff_test_client(&base);
        let mut session = client.initiate_desktop_handoff().await.unwrap();
        assert!(!session.is_expired());
        assert!(session.expires_at_millis() > unix_millis());
        assert!(!format!("{session:?}").contains("private_poll_proof"));
        assert_eq!(
            client
                .poll_desktop_handoff(&mut session)
                .await
                .unwrap()
                .status,
            DesktopHandoffStatus::Pending
        );
        assert!(session.poll_secret.is_none());
        let result = client.poll_desktop_handoff(&mut session).await.unwrap();
        assert_eq!(result.status, DesktopHandoffStatus::Completed);
        assert_eq!(result.token.unwrap().expose_secret(), "new-session-token");
        server.await.unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].path, "/api/v1/auth/handoff/initiate");
        assert!(requests[0].body.is_empty());
        assert!(!requests[0].headers.contains_key("content-type"));
        assert_eq!(requests[1].path, "/api/v1/auth/handoff/ABC-123/status");
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[1].body).unwrap(),
            json!({"poll_secret":"private_poll_proof"})
        );
        assert_eq!(requests[2].method, "GET");
        assert_eq!(requests[3].method, "GET");
        for request in requests.iter() {
            for header in [
                "authorization",
                "accept-language",
                "x-fluxer-features",
                "x-fluxer-capabilities",
                "x-fluxer-client-installation-id",
                "user-agent",
                "accept-encoding",
            ] {
                assert!(!request.headers.contains_key(header), "{header}");
            }
            assert_eq!(request.headers["accept"], "application/json");
            if request.method == "POST" {
                assert_eq!(request.headers["origin"], "https://browser.example");
            } else {
                assert!(!request.headers.contains_key("origin"));
                assert!(request.body.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn handoff_socket_never_falls_back_for_auth_rate_limit_or_server_errors() {
        for status in [400, 401, 403, 429, 500, 501] {
            let (base, requests, server) = handoff_server(vec![
                (201, handoff_initiate_payload()),
                (
                    status,
                    json!({"token":"must-not-log","poll_secret":"must-not-log"}),
                ),
            ])
            .await;
            let client = handoff_test_client(&base);
            let mut session = client.initiate_desktop_handoff().await.unwrap();
            let error = client.poll_desktop_handoff(&mut session).await.unwrap_err();
            assert!(matches!(error,Error::Api{status:actual,..} if actual==status));
            assert!(!format!("{error:?}").contains("must-not-log"));
            assert!(session.poll_secret.is_some());
            server.await.unwrap();
            assert_eq!(requests.lock().unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn handoff_socket_missing_proof_uses_get_and_oversize_payload_is_rejected() {
        let mut payload = handoff_initiate_payload();
        payload["poll_secret"] = json!("invalid/proof");
        let (base, requests, server) =
            handoff_server(vec![(201, payload), (200, json!({"status":"pending"}))]).await;
        let client = handoff_test_client(&base);
        let mut session = client.initiate_desktop_handoff().await.unwrap();
        assert!(session.poll_secret.is_none());
        client.poll_desktop_handoff(&mut session).await.unwrap();
        server.await.unwrap();
        assert_eq!(requests.lock().unwrap()[1].method, "GET");
        let (base, _, server) =
            handoff_server(vec![(201, json!({"oversize":"x".repeat(64*1024)}))]).await;
        assert!(matches!(
            handoff_test_client(&base).initiate_desktop_handoff().await,
            Err(Error::ResponseTooLarge(65536))
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn handoff_local_expiry_and_cross_instance_binding_do_not_send_requests() {
        let client = handoff_test_client("http://127.0.0.1:1");
        let mut session = DesktopHandoffSession {
            code: "ABC-123".into(),
            expires_at: "2000-01-01T00:00:00Z".into(),
            poll_secret: Some(HandoffPollSecret(Arc::from("proof"))),
            expires_at_ms: 0,
            api: client.instance.api.clone(),
            api_version: 1,
            web_app: client.instance.web_app.clone(),
            active: true,
        };
        assert_eq!(
            client
                .poll_desktop_handoff(&mut session)
                .await
                .unwrap()
                .status,
            DesktopHandoffStatus::Expired
        );
        assert!(session.poll_secret.is_none());
        session.active = true;
        session.api = Url::parse("http://other.example/api").unwrap();
        assert!(matches!(
            client.poll_desktop_handoff(&mut session).await,
            Err(Error::InvalidAuthResponse)
        ));
        let mut invalid_version = client.instance.clone();
        invalid_version.api_version = 1001;
        assert!(matches!(
            Client::new(invalid_version)
                .unwrap()
                .initiate_desktop_handoff()
                .await,
            Err(Error::InvalidEndpoint(_))
        ));
    }
}

/// Normalize the silent prefix and visible content. Native does not yet expose
/// the classic composer’s optional emoticon and URL settings.
pub fn normalize_forum_content(content: &str) -> (String, u32) {
    let trimmed = content.trim_start_matches(char::is_whitespace);
    let (content, flags) = if let Some(rest) = trimmed.strip_prefix("@silent") {
        if rest.is_empty() {
            (rest, 1 << 12)
        } else if let Some(first) = rest.chars().next().filter(|c| c.is_whitespace()) {
            (&rest[first.len_utf8()..], 1 << 12)
        } else {
            (content, 0)
        }
    } else {
        (content, 0)
    };
    let visible = content.chars().any(|c| {
        !c.is_whitespace()
            && !matches!(c as u32,
        0x0000 | 0x00ad | 0x034f | 0x061c | 0x115f | 0x1160 | 0x17b4 | 0x17b5 | 0x180e |
        0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x2069 | 0x2800 | 0x3164 |
        0xfe00..=0xfe0f | 0xfeff | 0xffa0 | 0xe0100..=0xe01ef)
    });
    (
        if visible {
            content.to_owned()
        } else {
            String::new()
        },
        flags,
    )
}

pub fn forum_post_body(name: &str, content: &str, tags: &[String], archive: Option<u64>) -> Value {
    let (content, flags) = normalize_forum_content(content);
    let mut body = json!({"name": name, "message": {"content": content}});
    if flags != 0 {
        body["message"]["flags"] = json!(flags);
    }
    if !tags.is_empty() {
        body["applied_tags"] = json!(tags);
    }
    if let Some(archive) = archive {
        body["auto_archive_duration"] = json!(archive);
    }
    body
}

#[cfg(test)]
mod forum_post_tests {
    use super::*;
    #[test]
    fn forum_post_wire_body_matches_classic_defaults() {
        assert_eq!(
            forum_post_body(
                "Title",
                "  @silent hello :) ",
                &["123".into(), "456".into()],
                Some(1440)
            ),
            json!({"name":"Title", "auto_archive_duration":1440, "applied_tags":["123","456"], "message":{"content":"hello :) ", "flags":4096}})
        );
        assert_eq!(
            forum_post_body("Title", "@silentish hello", &[], None),
            json!({"name":"Title", "message":{"content":"@silentish hello"}})
        );
        assert_eq!(
            normalize_forum_content("@silent \u{200b}"),
            (String::new(), 4096)
        );
    }
}

#[cfg(test)]
mod message_reference_tests {
    use super::*;
    #[test]
    fn reply_and_forward_bodies_match_the_web_client_contract() {
        let mut reference = fluxer_core::MessageReference {
            channel_id: 100.into(),
            message_id: Some(200.into()),
            guild_id: None,
            kind: 0,
        };
        assert_eq!(
            message_body("reply", Some("nonce"), Some(&reference), false),
            json!({"content":"reply","nonce":"nonce","message_reference":{"channel_id":"100","message_id":"200","type":0},"allowed_mentions":{"replied_user":false}})
        );
        reference.kind = 1;
        reference.guild_id = Some(10.into());
        assert_eq!(
            message_body("", None, Some(&reference), false),
            json!({"message_reference":{"channel_id":"100","message_id":"200","guild_id":"10","type":1}})
        );
        assert_eq!(
            message_body("hello", None, None, false),
            json!({"content":"hello"})
        );
    }
}
