// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{SavedSession, StoreError};
use fluxer_core::Snowflake;
use fluxer_protocol::{Instance, SessionToken, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

const VERSION: u32 = 1;
pub(crate) const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

// Deliberately no Debug: serialization is secret-bearing and is only passed to
// the encrypted credential backend (or the injectable in-memory test store).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    account_key: String,
    instance: PinnedInstance,
    token: SecretText,
    user_id: Snowflake,
    user: Option<Value>,
    locale: String,
    selected_guild: Option<Snowflake>,
    selected_channel: Option<Snowflake>,
    valid: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct SecretText(String);
impl Drop for SecretText {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinnedInstance {
    api: String,
    gateway: String,
    web_app: String,
    api_version: u64,
    features: Value,
    document: Value,
}

pub(crate) fn encode(session: &SavedSession) -> Result<Zeroizing<Vec<u8>>, StoreError> {
    validate_session(session)?;
    let instance = validate_instance(PinnedInstance {
        api: normalize_url(&session.instance.api, false)?.to_string(),
        gateway: normalize_url(&session.instance.gateway, true)?.to_string(),
        web_app: normalize_url(&session.instance.web_app, false)?.to_string(),
        api_version: session.instance.api_version,
        features: session.instance.features.clone(),
        document: session.instance.document.clone(),
    })?;
    let record = Record {
        version: VERSION,
        account_key: session.account_key(),
        instance: PinnedInstance {
            api: instance.api.to_string(),
            gateway: instance.gateway.to_string(),
            web_app: instance.web_app.to_string(),
            api_version: instance.api_version,
            features: instance.features,
            document: instance.document,
        },
        token: SecretText(session.token.expose_secret().to_owned()),
        user_id: session.user_id,
        user: session.user.clone(),
        locale: session.locale.clone(),
        selected_guild: session.selected_guild,
        selected_channel: session.selected_channel,
        valid: session.valid,
    };
    let bytes = Zeroizing::new(serde_json::to_vec(&record).map_err(|_| StoreError::InvalidRecord)?);
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(StoreError::InvalidRecord);
    }
    Ok(bytes)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<SavedSession, StoreError> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(StoreError::InvalidRecord);
    }
    let record: Record = serde_json::from_slice(bytes).map_err(|_| StoreError::InvalidRecord)?;
    if record.version != VERSION || record.token.0.is_empty() || record.token.0.len() > 4096 {
        return Err(StoreError::InvalidRecord);
    }
    let instance = validate_instance(record.instance)?;
    let session = SavedSession {
        instance,
        token: SessionToken::new(record.token.0.clone()).map_err(|_| StoreError::InvalidRecord)?,
        user_id: record.user_id,
        user: record.user,
        locale: record.locale,
        selected_guild: record.selected_guild,
        selected_channel: record.selected_channel,
        valid: record.valid,
    };
    validate_session(&session)?;
    if record.account_key != session.account_key() {
        return Err(StoreError::InvalidRecord);
    }
    Ok(session)
}

fn validate_session(session: &SavedSession) -> Result<(), StoreError> {
    if session.account_key().len() > 1024
        || session.user_id.get() == 0
        || session.token.expose_secret().len() > 4096
        || session.locale.is_empty()
        || session.locale.len() > 128
        || !session
            .locale
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || session.selected_guild.is_some_and(|id| id.get() == 0)
        || session.selected_channel.is_some_and(|id| id.get() == 0)
    {
        return Err(StoreError::InvalidRecord);
    }
    if let Some(user) = &session.user {
        if !user.is_object() {
            return Err(StoreError::InvalidRecord);
        }
        if let Some(id) = user.get("id") {
            let id: Snowflake =
                serde_json::from_value(id.clone()).map_err(|_| StoreError::InvalidRecord)?;
            if id != session.user_id {
                return Err(StoreError::InvalidRecord);
            }
        }
    }
    Ok(())
}

fn normalize_url(url: &Url, socket: bool) -> Result<Url, StoreError> {
    if url.as_str().len() > 8192
        || url.host_str().is_none()
        || url.port() == Some(0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || if socket {
            !matches!(url.scheme(), "ws" | "wss")
        } else {
            !matches!(url.scheme(), "http" | "https")
        }
    {
        return Err(StoreError::InvalidRecord);
    }
    let mut normalized = url.clone();
    normalized.set_path(url.path().trim_end_matches('/'));
    Ok(normalized)
}
fn parse_url(value: &str, socket: bool) -> Result<Url, StoreError> {
    normalize_url(
        &Url::parse(value).map_err(|_| StoreError::InvalidRecord)?,
        socket,
    )
}

fn validate_instance(pin: PinnedInstance) -> Result<Instance, StoreError> {
    if pin.api_version == 0 || pin.api_version > MAX_SAFE_INTEGER {
        return Err(StoreError::InvalidRecord);
    }
    let api = parse_url(&pin.api, false)?;
    let gateway = parse_url(&pin.gateway, true)?;
    let web_app = parse_url(&pin.web_app, false)?;
    if pin.document.is_null() {
        // Explicit test/development endpoints are also pinned. There is no
        // discovery, referral, endpoint guessing or token-bearing I/O here.
        return Ok(Instance {
            api,
            gateway,
            web_app,
            api_version: pin.api_version,
            features: pin.features,
            document: pin.document,
        });
    }
    let projected =
        Instance::from_document(&api, pin.document).map_err(|_| StoreError::InvalidRecord)?;
    if projected.api != api
        || projected.gateway != gateway
        || projected.web_app != web_app
        || projected.api_version != pin.api_version
        || projected.features != pin.features
    {
        return Err(StoreError::InvalidRecord);
    }
    Ok(projected)
}
