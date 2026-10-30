// SPDX-License-Identifier: AGPL-3.0-or-later

//! Instance discovery document parsing, origin validation and client-runtime
//! projection, ported from `Discovery.ts` and `ClientRuntimeDiscovery.ts` in
//! `packages/instance_bootstrap`.
//!
//! Every sub-parser is total: invalid input yields `None` or an empty value,
//! never an error. Only the top-level shape, `endpoints.api`,
//! `endpoints.gateway`, the generation and the projection can fail.

use std::fmt;

use serde_json::{Map, Value};
use url::Url;

use super::endpoint::{EndpointKind, normalize_instance_endpoint};
use crate::js::is_js_whitespace;

pub const INSTANCE_DISCOVERY_PATH: &str = "/.well-known/fluxer";
const CONVENTIONAL_ENDPOINT_PATH: &str = "/api";
const CURRENT_INSTANCE_CODENAME: &str = "tungsten";
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
const ACCOUNT_DELETION_GRACE_PERIOD_HOURS_MIN: i64 = 1;
const ACCOUNT_DELETION_GRACE_PERIOD_HOURS_MAX: i64 = 8760;
const LIMIT_RULES_MAX: usize = 100;
const AGE_POLICY_GEOS_MAX: usize = 512;
const DOMAIN_MIGRATION_ROLLOUT_BASIS_POINTS_MAX: i64 = 10_000;
const DOMAIN_MIGRATION_ROLLOUT_SALT_MAX_LENGTH: usize = 64;
const DEFAULT_PREMIUM_PRODUCT_NAME: &str = "Plutonium";

const REGISTRATION_MODES: &[&str] = &["open", "approval", "closed"];
const PUSH_DELIVERY_MODES: &[&str] = &["direct", "external_relay", "hybrid"];
const AGE_POLICY_ACTIONS: &[&str] = &["restrict", "block"];
const ACCOUNT_IDENTITY_MODES: &[&str] = &["email", "username"];
const TAG_STYLES: &[&str] = &["none", "random"];
const INSTANT_ACCOUNT_AVAILABILITY: &[&str] = &[
    "available",
    "disabled",
    "captcha_off",
    "registration_closed",
    "approval_required",
];
const INTERACTION_TIER_MODES: &[&str] = &["legacy", "enforced"];
const GUILD_JOIN_GATES: &[&str] = &["off", "visits_capable", "always"];
const EMAIL_DELIVERY_STATES: &[&str] =
    &["on", "off_username_mode", "off_disabled", "off_incomplete"];
const VERIFICATION_BASES: &[&str] = &["confirmed_email", "local_credential"];
const ACCOUNT_IDENTITY_LOCK_REASONS: &[&str] = &[
    "setup_configured",
    "existing_instance",
    "admin_exists",
    "accounts_exist",
];

/// A key that may be absent (`None`), present but unusable or null
/// (`Some(None)`), or present and valid (`Some(Some(_))`). The client runtime
/// treats absent as a missing requirement and present-null as a legitimate value.
pub type Maybe<T> = Option<Option<T>>;

#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveryError {
    InvalidDocument(String),
    RequiresNewerClient(String),
    OriginMismatch { served_by: String, declared: String },
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDocument(reason) => {
                write!(f, "Instance discovery document is unusable: {reason}")
            }
            Self::RequiresNewerClient(generation) => write!(
                f,
                "Instance speaks generation \"{generation}\", which this client does not understand"
            ),
            Self::OriginMismatch {
                served_by,
                declared,
            } => write!(
                f,
                "Instance discovery served by {served_by} declared an API at {declared}"
            ),
        }
    }
}

impl std::error::Error for DiscoveryError {}

fn invalid(reason: &str) -> DiscoveryError {
    DiscoveryError::InvalidDocument(reason.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceGeneration {
    Tungsten,
    Legacy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryEndpoints {
    pub api: String,
    pub gateway: String,
    pub api_client: Option<String>,
    pub api_public: Option<String>,
    pub media: Option<String>,
    pub upload_relay: Option<String>,
    pub static_cdn: Option<String>,
    pub marketing: Option<String>,
    pub docs: Option<String>,
    pub admin: Option<String>,
    pub invite: Option<String>,
    pub gift: Option<String>,
    pub webapp: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstanceFeatures {
    pub voice_enabled: Option<bool>,
    pub stripe_enabled: Option<bool>,
    pub self_hosted: Option<bool>,
    pub presigned_attachment_uploads: Option<bool>,
    pub emails_enabled: Option<bool>,
    pub premium_enabled: Option<bool>,
    pub stripe_serviceable: Option<bool>,
    pub phone_verification_enabled: Option<bool>,
    pub discovery_enabled: Option<bool>,
    pub desktop_modules_enabled: Option<bool>,
    pub identifier_first: Option<bool>,
    pub instant_accounts: Option<bool>,
    pub email_codes: Option<bool>,
    pub date_of_birth_step: Option<bool>,
    pub account_identity: Option<String>,
    pub tag_style: Option<String>,
    pub instant_account_availability: Option<String>,
    pub interaction_tier: Option<String>,
    pub guild_join_gate: Option<String>,
    pub email_delivery_state: Option<String>,
    pub verification_basis: Option<String>,
    pub account_deletion_grace_period_hours: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceGif {
    pub provider: String,
    pub display_name: String,
    pub attribution_required: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceSso {
    pub enabled: bool,
    pub enforced: bool,
    pub display_name: Option<String>,
    pub button_label: Option<String>,
    pub redirect_uri: String,
    pub available: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceRegistration {
    pub mode: String,
    pub admin_registration_urls_enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceCommunity {
    pub single_community: bool,
    pub single_community_guild_id: Option<String>,
    pub direct_messages_disabled: bool,
    pub friends_disabled: bool,
    pub guild_create_access: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceServices {
    pub gif_enabled: bool,
    pub youtube_enabled: bool,
    pub bluesky_enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceNotice {
    pub id: String,
    pub message: String,
    pub url: Option<String>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitFilter {
    pub traits: Option<Vec<String>>,
    pub guild_features: Option<Vec<String>>,
}

/// One limit rule; `values` are `overrides` in a wire-format config and `limits` in a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitRule {
    pub id: String,
    pub filters: Option<LimitFilter>,
    pub values: Vec<(String, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LimitConfig {
    Wire {
        trait_definitions: Vec<String>,
        rules: Vec<LimitRule>,
        defaults_hash: String,
    },
    Snapshot {
        version: Option<i64>,
        trait_definitions: Vec<String>,
        rules: Vec<LimitRule>,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstancePush {
    pub public_vapid_key: Maybe<String>,
    pub delivery_mode: Option<String>,
    pub event_resolution_path: Option<String>,
    pub external_relay_url: Maybe<String>,
    pub relay_signing_key_hashes: Maybe<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstanceBranding {
    pub product_name: Option<String>,
    pub premium_product_name: Option<String>,
    pub icon_url: Maybe<String>,
    pub symbol_url: Maybe<String>,
    pub logo_url: Maybe<String>,
    pub wordmark_url: Maybe<String>,
    pub favicon_url: Maybe<String>,
    pub favicon_dark_url: Maybe<String>,
    pub status_page_url: Maybe<String>,
    pub status_page_incident_history_url: Maybe<String>,
    pub premium_info_url: Maybe<String>,
    pub sign_in_backdrop_url: Maybe<String>,
    pub theme_color: Maybe<String>,
    pub custom_css: Maybe<String>,
    pub upload_limit_message: Maybe<String>,
    pub trusted_domains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstanceSetup {
    pub configured: Option<bool>,
    pub account_identity_locked: Option<bool>,
    pub setup_token_required: Option<bool>,
    pub admin_url: Maybe<String>,
    pub account_identity_lock_reason: Maybe<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstanceLegal {
    pub terms_url: Maybe<String>,
    pub privacy_url: Maybe<String>,
    pub guidelines_url: Maybe<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstanceAppRegistration {
    pub collect_date_of_birth: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceAppPublic {
    pub branding: InstanceBranding,
    pub setup: InstanceSetup,
    pub legal: InstanceLegal,
    pub registration: InstanceAppRegistration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceAgePolicyGeo {
    pub country_code: String,
    pub region_code: Option<String>,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceAgePolicy {
    pub geos: Vec<InstanceAgePolicyGeo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceDomainMigration {
    pub enabled: bool,
    pub anonymous_rollout_basis_points: i64,
    pub rollout_salt: String,
    pub standalone_forwarding: bool,
}

/// A parsed discovery document. `raw` is the untouched JSON for callers that forward it verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceDiscovery {
    pub raw: Value,
    pub generation: InstanceGeneration,
    pub api_code_version: Option<i64>,
    pub endpoints: DiscoveryEndpoints,
    pub features: InstanceFeatures,
    pub gif: Option<InstanceGif>,
    pub sso: Option<InstanceSso>,
    pub registration: Option<InstanceRegistration>,
    pub community: Option<InstanceCommunity>,
    pub services: Option<InstanceServices>,
    pub limits: Option<LimitConfig>,
    pub push: InstancePush,
    pub app_public: Option<InstanceAppPublic>,
    pub age_policy: Option<InstanceAgePolicy>,
    pub domain_migration: Option<InstanceDomainMigration>,
    pub notice: Option<InstanceNotice>,
}

// ---------------------------------------------------------------------------
// Candidate URLs
// ---------------------------------------------------------------------------

/// Candidate discovery URLs for a user-entered instance endpoint, most specific first.
pub fn discovery_candidate_urls(input: &str) -> Vec<String> {
    let Some(base) = normalize_instance_endpoint(input, EndpointKind::Service) else {
        return Vec::new();
    };
    let Ok(url) = Url::parse(&base) else {
        return Vec::new();
    };
    let origin = format!("{}://{}", url.scheme(), host_with_port(&url));
    let endpoint_path = (url.path() != "/").then(|| url.path().to_owned());
    let mut candidates: Vec<String> = Vec::new();
    let mut append = |candidate: String| {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    };
    append(format!("{origin}{INSTANCE_DISCOVERY_PATH}"));
    if let Some(path) = endpoint_path {
        append(format!("{origin}{path}{INSTANCE_DISCOVERY_PATH}"));
    }
    append(format!(
        "{origin}{CONVENTIONAL_ENDPOINT_PATH}{INSTANCE_DISCOVERY_PATH}"
    ));
    candidates
}

/// `URL.host`: hostname plus a non-default port.
fn host_with_port(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Origin check
// ---------------------------------------------------------------------------

/// `new URL(value).origin`, `None` when the value does not parse.
fn origin_of_endpoint(value: &str) -> Option<String> {
    Url::parse(value)
        .ok()
        .map(|url| url.origin().ascii_serialization())
}

pub fn require_discovery_declares_its_own_origin(
    served_url: &str,
    parsed: &InstanceDiscovery,
) -> Result<(), DiscoveryError> {
    let Some(served_by) = origin_of_endpoint(served_url) else {
        return Err(DiscoveryError::OriginMismatch {
            served_by: served_url.to_owned(),
            declared: parsed.endpoints.api.clone(),
        });
    };
    for declared in [
        Some(&parsed.endpoints.api),
        parsed.endpoints.api_client.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if origin_of_endpoint(declared).as_deref() != Some(served_by.as_str()) {
            return Err(DiscoveryError::OriginMismatch {
                served_by,
                declared: declared.clone(),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Primitive readers
// ---------------------------------------------------------------------------

fn as_record(value: Option<&Value>) -> Option<&Map<String, Value>> {
    match value {
        Some(Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn read_boolean(source: &Map<String, Value>, key: &str) -> Option<bool> {
    source.get(key).and_then(Value::as_bool)
}

fn read_non_empty_string(source: &Map<String, Value>, key: &str) -> Option<String> {
    source
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// JS `Number.isSafeInteger` over a JSON number, bounded to `[minimum, maximum]`.
fn read_integer(source: &Map<String, Value>, key: &str, minimum: i64, maximum: i64) -> Option<i64> {
    let number = source.get(key)?.as_number()?;
    let value = if let Some(integer) = number.as_i64() {
        integer
    } else if number.is_u64() {
        return None;
    } else {
        let float = number.as_f64()?;
        if !float.is_finite() || float.fract() != 0.0 || float.abs() > MAX_SAFE_INTEGER as f64 {
            return None;
        }
        float as i64
    };
    (value.unsigned_abs() <= MAX_SAFE_INTEGER.unsigned_abs()
        && (minimum..=maximum).contains(&value))
    .then_some(value)
}

fn read_string_array(source: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    source
        .get(key)?
        .as_array()?
        .iter()
        .map(|entry| entry.as_str().map(str::to_owned))
        .collect()
}

fn read_literal(source: &Map<String, Value>, key: &str, allowed: &[&str]) -> Option<String> {
    let value = source.get(key)?.as_str()?;
    allowed.contains(&value).then(|| value.to_owned())
}

fn read_absolute_http_url(source: &Map<String, Value>, key: &str) -> Option<String> {
    let raw = read_non_empty_string(source, key)?;
    let value = raw.trim_matches(is_js_whitespace);
    let url = Url::parse(value).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| value.to_owned())
}

/// `Object.hasOwn(source, key) ? reader(source, key) : absent`.
fn maybe<T>(
    source: &Map<String, Value>,
    key: &str,
    reader: impl FnOnce(&Map<String, Value>, &str) -> Option<T>,
) -> Maybe<T> {
    source.contains_key(key).then(|| reader(source, key))
}

/// `readDateString`: a non-empty string that `Date.parse` accepts.
fn read_date_string(source: &Map<String, Value>, key: &str) -> Option<String> {
    read_non_empty_string(source, key).filter(|value| is_parsable_date(value))
}

/// Accepts the ECMAScript date-time string format (with V8's leniencies for
/// range checks and separators). V8's legacy fallback grammar (`Jan 1 2021`,
/// `2021/01/01`, ...) is not reproduced.
fn is_parsable_date(value: &str) -> bool {
    fn digits(bytes: &[u8], at: &mut usize, count: usize) -> Option<u32> {
        let slice = bytes.get(*at..*at + count)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *at += count;
        Some(
            slice
                .iter()
                .fold(0, |acc, digit| acc * 10 + u32::from(digit - b'0')),
        )
    }
    fn eat(bytes: &[u8], at: &mut usize, expected: u8) -> bool {
        let matched = bytes.get(*at) == Some(&expected);
        if matched {
            *at += 1;
        }
        matched
    }
    fn parse(bytes: &[u8]) -> Option<()> {
        let mut at = 0;
        match bytes.first()? {
            b'+' | b'-' => {
                at += 1;
                digits(bytes, &mut at, 6)?;
            }
            _ => {
                digits(bytes, &mut at, 4)?;
            }
        }
        if eat(bytes, &mut at, b'-') {
            if !(1..=12).contains(&digits(bytes, &mut at, 2)?) {
                return None;
            }
            if eat(bytes, &mut at, b'-') && !(1..=31).contains(&digits(bytes, &mut at, 2)?) {
                return None;
            }
        }
        if at == bytes.len() {
            return Some(());
        }
        if !matches!(bytes[at], b'T' | b't' | b' ') {
            return None;
        }
        at += 1;
        let hour = digits(bytes, &mut at, 2)?;
        if !eat(bytes, &mut at, b':') {
            return None;
        }
        let minute = digits(bytes, &mut at, 2)?;
        let (mut second, mut fraction) = (0, 0);
        if eat(bytes, &mut at, b':') {
            second = digits(bytes, &mut at, 2)?;
            if eat(bytes, &mut at, b'.') {
                let start = at;
                while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                    at += 1;
                }
                if at == start {
                    return None;
                }
                fraction = bytes[start..at]
                    .iter()
                    .map(|digit| u32::from(digit - b'0'))
                    .sum();
            }
        }
        if hour > 24
            || minute > 59
            || second > 59
            || (hour == 24 && (minute, second, fraction) != (0, 0, 0))
        {
            return None;
        }
        match bytes.get(at) {
            None => return Some(()),
            Some(b'Z' | b'z') => at += 1,
            Some(b'+' | b'-') => {
                at += 1;
                let offset_hour = digits(bytes, &mut at, 2)?;
                eat(bytes, &mut at, b':');
                let offset_minute = digits(bytes, &mut at, 2)?;
                if offset_hour > 23 || offset_minute > 59 {
                    return None;
                }
            }
            Some(_) => return None,
        }
        (at == bytes.len()).then_some(())
    }
    parse(value.trim_matches(is_js_whitespace).as_bytes()).is_some()
}

// ---------------------------------------------------------------------------
// Document parsing
// ---------------------------------------------------------------------------

pub fn parse_instance_discovery_document(
    value: &Value,
) -> Result<InstanceDiscovery, DiscoveryError> {
    let Some(source) = as_record(Some(value)) else {
        return Err(invalid("the response body is not a JSON object"));
    };
    let api_code_version = read_integer(source, "api_code_version", 1, MAX_SAFE_INTEGER);
    Ok(InstanceDiscovery {
        raw: value.clone(),
        generation: resolve_generation(source, api_code_version)?,
        api_code_version,
        endpoints: parse_endpoints(source.get("endpoints"))?,
        features: parse_features(source.get("features")),
        gif: parse_gif(source.get("gif")),
        sso: parse_sso(source.get("sso")),
        registration: parse_registration(source.get("registration")),
        community: parse_community(source.get("community")),
        services: parse_services(source.get("services")),
        limits: parse_limits(source.get("limits")),
        push: parse_push(source.get("push")),
        app_public: parse_app_public(source.get("app_public")),
        age_policy: parse_age_policy(source.get("age_policy")),
        domain_migration: parse_instance_domain_migration(source.get("domain_migration")),
        notice: parse_notice(source.get("notice")),
    })
}

fn resolve_generation(
    source: &Map<String, Value>,
    api_code_version: Option<i64>,
) -> Result<InstanceGeneration, DiscoveryError> {
    if let Some(reported) = source
        .get("codename")
        .and_then(Value::as_str)
        .filter(|reported| !reported.is_empty())
    {
        return if reported == CURRENT_INSTANCE_CODENAME {
            Ok(InstanceGeneration::Tungsten)
        } else {
            Err(DiscoveryError::RequiresNewerClient(reported.to_owned()))
        };
    }
    if source
        .get("api_code_version")
        .is_some_and(|value| !value.is_null())
        && api_code_version.is_none()
    {
        return Err(invalid("api_code_version is not a positive integer"));
    }
    Ok(InstanceGeneration::Legacy)
}

fn endpoint_field(source: &Map<String, Value>, key: &str, kind: EndpointKind) -> Option<String> {
    source
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| normalize_instance_endpoint(value, kind))
}

fn parse_endpoints(value: Option<&Value>) -> Result<DiscoveryEndpoints, DiscoveryError> {
    let Some(source) = as_record(value) else {
        return Err(invalid("endpoints is missing or not an object"));
    };
    let service = |key| endpoint_field(source, key, EndpointKind::Service);
    let Some(api) = service("api") else {
        return Err(invalid(
            "endpoints.api is missing or not an absolute http(s) endpoint",
        ));
    };
    let Some(gateway) = endpoint_field(source, "gateway", EndpointKind::Gateway) else {
        return Err(invalid(
            "endpoints.gateway is missing or not a ws(s) endpoint",
        ));
    };
    Ok(DiscoveryEndpoints {
        api,
        gateway,
        api_client: service("api_client"),
        api_public: service("api_public"),
        media: service("media"),
        upload_relay: service("upload_relay"),
        static_cdn: service("static_cdn"),
        marketing: service("marketing"),
        docs: service("docs"),
        admin: service("admin"),
        invite: service("invite"),
        gift: service("gift"),
        webapp: endpoint_field(source, "webapp", EndpointKind::WebApp),
    })
}

fn parse_features(value: Option<&Value>) -> InstanceFeatures {
    let Some(source) = as_record(value) else {
        return InstanceFeatures::default();
    };
    InstanceFeatures {
        voice_enabled: read_boolean(source, "voice_enabled"),
        stripe_enabled: read_boolean(source, "stripe_enabled"),
        self_hosted: read_boolean(source, "self_hosted"),
        presigned_attachment_uploads: read_boolean(source, "presigned_attachment_uploads"),
        emails_enabled: read_boolean(source, "emails_enabled"),
        premium_enabled: read_boolean(source, "premium_enabled"),
        stripe_serviceable: read_boolean(source, "stripe_serviceable"),
        phone_verification_enabled: read_boolean(source, "phone_verification_enabled"),
        discovery_enabled: read_boolean(source, "discovery_enabled"),
        desktop_modules_enabled: read_boolean(source, "desktop_modules_enabled"),
        identifier_first: read_boolean(source, "identifier_first"),
        instant_accounts: read_boolean(source, "instant_accounts"),
        email_codes: read_boolean(source, "email_codes"),
        date_of_birth_step: read_boolean(source, "date_of_birth_step"),
        account_identity: read_literal(source, "account_identity", ACCOUNT_IDENTITY_MODES),
        tag_style: read_literal(source, "tag_style", TAG_STYLES),
        instant_account_availability: read_literal(
            source,
            "instant_account_availability",
            INSTANT_ACCOUNT_AVAILABILITY,
        ),
        interaction_tier: read_literal(source, "interaction_tier", INTERACTION_TIER_MODES),
        guild_join_gate: read_literal(source, "guild_join_gate", GUILD_JOIN_GATES),
        email_delivery_state: read_literal(source, "email_delivery_state", EMAIL_DELIVERY_STATES),
        verification_basis: read_literal(source, "verification_basis", VERIFICATION_BASES),
        account_deletion_grace_period_hours: read_integer(
            source,
            "account_deletion_grace_period_hours",
            ACCOUNT_DELETION_GRACE_PERIOD_HOURS_MIN,
            ACCOUNT_DELETION_GRACE_PERIOD_HOURS_MAX,
        ),
    }
}

fn parse_gif(value: Option<&Value>) -> Option<InstanceGif> {
    let source = as_record(value)?;
    Some(InstanceGif {
        provider: read_non_empty_string(source, "provider")?,
        display_name: read_non_empty_string(source, "display_name")?,
        attribution_required: read_boolean(source, "attribution_required")?,
    })
}

fn parse_sso(value: Option<&Value>) -> Option<InstanceSso> {
    let source = as_record(value)?;
    Some(InstanceSso {
        enabled: read_boolean(source, "enabled")?,
        enforced: read_boolean(source, "enforced")?,
        redirect_uri: read_absolute_http_url(source, "redirect_uri")?,
        display_name: read_non_empty_string(source, "display_name"),
        button_label: read_non_empty_string(source, "button_label"),
        available: read_boolean(source, "available"),
    })
}

fn parse_registration(value: Option<&Value>) -> Option<InstanceRegistration> {
    let source = as_record(value)?;
    Some(InstanceRegistration {
        mode: read_literal(source, "mode", REGISTRATION_MODES)?,
        admin_registration_urls_enabled: read_boolean(source, "admin_registration_urls_enabled")?,
    })
}

fn parse_community(value: Option<&Value>) -> Option<InstanceCommunity> {
    let source = as_record(value)?;
    let single_community = read_boolean(source, "single_community")?;
    let direct_messages_disabled = read_boolean(source, "direct_messages_disabled")?;
    Some(InstanceCommunity {
        single_community,
        single_community_guild_id: read_non_empty_string(source, "single_community_guild_id"),
        direct_messages_disabled,
        friends_disabled: read_boolean(source, "friends_disabled")
            .unwrap_or(direct_messages_disabled),
        guild_create_access: read_boolean(source, "guild_create_access").unwrap_or(true),
    })
}

fn parse_notice(value: Option<&Value>) -> Option<InstanceNotice> {
    let source = as_record(value)?;
    Some(InstanceNotice {
        id: read_non_empty_string(source, "id")?,
        message: read_non_empty_string(source, "message")?,
        url: read_non_empty_string(source, "url"),
        starts_at: read_date_string(source, "starts_at"),
        ends_at: read_date_string(source, "ends_at"),
    })
}

fn parse_services(value: Option<&Value>) -> Option<InstanceServices> {
    let source = as_record(value)?;
    Some(InstanceServices {
        gif_enabled: read_boolean(source, "gif_enabled")?,
        youtube_enabled: read_boolean(source, "youtube_enabled")?,
        bluesky_enabled: read_boolean(source, "bluesky_enabled")?,
    })
}

fn parse_limits(value: Option<&Value>) -> Option<LimitConfig> {
    let source = as_record(value)?;
    let trait_definitions = read_string_array(source, "traitDefinitions")?;
    let entries = source.get("rules")?.as_array()?;
    if entries.len() > LIMIT_RULES_MAX {
        return None;
    }
    let mut wire_rules = Vec::new();
    let mut snapshot_rules = Vec::new();
    for entry in entries {
        let rule = as_record(Some(entry))?;
        let id = read_non_empty_string(rule, "id")?;
        let filters = parse_limit_filter(rule.get("filters"));
        if rule.contains_key("filters") && filters.is_none() {
            return None;
        }
        if let Some(values) = read_limit_values(rule.get("overrides")) {
            wire_rules.push(LimitRule {
                id,
                filters,
                values,
            });
            continue;
        }
        let values = read_limit_values(rule.get("limits"))?;
        snapshot_rules.push(LimitRule {
            id,
            filters,
            values,
        });
    }
    if !wire_rules.is_empty() && !snapshot_rules.is_empty() {
        return None;
    }
    let version = read_integer(source, "version", 1, MAX_SAFE_INTEGER);
    if !wire_rules.is_empty() || (snapshot_rules.is_empty() && version == Some(2)) {
        return Some(LimitConfig::Wire {
            trait_definitions,
            rules: wire_rules,
            defaults_hash: read_non_empty_string(source, "defaultsHash")?,
        });
    }
    Some(LimitConfig::Snapshot {
        version,
        trait_definitions,
        rules: snapshot_rules,
    })
}

fn parse_limit_filter(value: Option<&Value>) -> Option<LimitFilter> {
    let source = as_record(value)?;
    Some(LimitFilter {
        traits: read_string_array(source, "traits"),
        guild_features: read_string_array(source, "guildFeatures"),
    })
}

fn read_limit_values(value: Option<&Value>) -> Option<Vec<(String, f64)>> {
    as_record(value)?
        .iter()
        .map(|(key, entry)| Some((key.clone(), entry.as_number()?.as_f64()?)))
        .collect()
}

fn parse_push(value: Option<&Value>) -> InstancePush {
    let Some(source) = as_record(value) else {
        return InstancePush::default();
    };
    InstancePush {
        public_vapid_key: maybe(source, "public_vapid_key", read_non_empty_string),
        delivery_mode: read_literal(source, "delivery_mode", PUSH_DELIVERY_MODES),
        event_resolution_path: read_non_empty_string(source, "event_resolution_path"),
        external_relay_url: maybe(source, "external_relay_url", read_absolute_http_url),
        relay_signing_key_hashes: maybe(source, "relay_signing_key_hashes", read_string_array),
    }
}

fn parse_app_public(value: Option<&Value>) -> Option<InstanceAppPublic> {
    let source = as_record(value)?;
    Some(InstanceAppPublic {
        branding: parse_branding(source.get("branding")),
        setup: parse_setup(source.get("setup")),
        legal: parse_legal(source.get("legal")),
        registration: parse_app_registration(source.get("registration")),
    })
}

fn parse_branding(value: Option<&Value>) -> InstanceBranding {
    let Some(source) = as_record(value) else {
        return InstanceBranding::default();
    };
    let url = |key| maybe(source, key, read_absolute_http_url);
    let text = |key| maybe(source, key, read_non_empty_string);
    InstanceBranding {
        product_name: read_non_empty_string(source, "product_name"),
        premium_product_name: read_non_empty_string(source, "premium_product_name"),
        icon_url: url("icon_url"),
        symbol_url: url("symbol_url"),
        logo_url: url("logo_url"),
        wordmark_url: url("wordmark_url"),
        favicon_url: url("favicon_url"),
        favicon_dark_url: url("favicon_dark_url"),
        status_page_url: url("status_page_url"),
        status_page_incident_history_url: url("status_page_incident_history_url"),
        premium_info_url: url("premium_info_url"),
        sign_in_backdrop_url: url("sign_in_backdrop_url"),
        theme_color: text("theme_color"),
        custom_css: text("custom_css"),
        upload_limit_message: text("upload_limit_message"),
        trusted_domains: read_string_array(source, "trusted_domains"),
    }
}

fn parse_setup(value: Option<&Value>) -> InstanceSetup {
    let Some(source) = as_record(value) else {
        return InstanceSetup::default();
    };
    InstanceSetup {
        configured: read_boolean(source, "configured"),
        account_identity_locked: read_boolean(source, "account_identity_locked"),
        setup_token_required: read_boolean(source, "setup_token_required"),
        admin_url: maybe(source, "admin_url", read_absolute_http_url),
        account_identity_lock_reason: maybe(
            source,
            "account_identity_lock_reason",
            |source, key| read_literal(source, key, ACCOUNT_IDENTITY_LOCK_REASONS),
        ),
    }
}

fn parse_legal(value: Option<&Value>) -> InstanceLegal {
    let Some(source) = as_record(value) else {
        return InstanceLegal::default();
    };
    let url = |key| maybe(source, key, read_absolute_http_url);
    InstanceLegal {
        terms_url: url("terms_url"),
        privacy_url: url("privacy_url"),
        guidelines_url: url("guidelines_url"),
    }
}

fn parse_app_registration(value: Option<&Value>) -> InstanceAppRegistration {
    InstanceAppRegistration {
        collect_date_of_birth: as_record(value)
            .and_then(|source| read_boolean(source, "collect_date_of_birth")),
    }
}

fn parse_age_policy(value: Option<&Value>) -> Option<InstanceAgePolicy> {
    let source = as_record(value)?;
    let entries = source.get("geos")?.as_array()?;
    if entries.len() > AGE_POLICY_GEOS_MAX {
        return None;
    }
    let geos = entries
        .iter()
        .map(|entry| {
            let geo = as_record(Some(entry))?;
            Some(InstanceAgePolicyGeo {
                country_code: read_non_empty_string(geo, "country_code")?,
                action: read_literal(geo, "action", AGE_POLICY_ACTIONS)?,
                region_code: read_non_empty_string(geo, "region_code"),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(InstanceAgePolicy { geos })
}

pub fn parse_instance_domain_migration(value: Option<&Value>) -> Option<InstanceDomainMigration> {
    let source = as_record(value)?;
    Some(InstanceDomainMigration {
        enabled: read_boolean(source, "enabled")?,
        anonymous_rollout_basis_points: read_integer(
            source,
            "anonymous_rollout_basis_points",
            0,
            DOMAIN_MIGRATION_ROLLOUT_BASIS_POINTS_MAX,
        )?,
        rollout_salt: read_domain_migration_rollout_salt(source)?,
        standalone_forwarding: read_boolean(source, "standalone_forwarding")?,
    })
}

/// Trimmed, 1..=64 UTF-16 units, printable ASCII only (so units equal characters).
fn read_domain_migration_rollout_salt(source: &Map<String, Value>) -> Option<String> {
    let value = source
        .get("rollout_salt")?
        .as_str()?
        .trim_matches(is_js_whitespace);
    (!value.is_empty()
        && value.encode_utf16().count() <= DOMAIN_MIGRATION_ROLLOUT_SALT_MAX_LENGTH
        && value.chars().all(|c| ('\x20'..='\x7e').contains(&c)))
    .then(|| value.to_owned())
}

// ---------------------------------------------------------------------------
// Client runtime projection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ClientRuntimeEndpoints {
    pub discovery_api: String,
    /// The API endpoint the client talks to (`apiEndpoint` in TypeScript).
    pub api_client: String,
    pub api_public: String,
    pub gateway: String,
    pub media: String,
    pub upload_relay: Option<String>,
    pub static_cdn: String,
    pub marketing: String,
    pub docs: Option<String>,
    pub admin: String,
    pub invite: String,
    pub gift: String,
    pub webapp: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClientRuntimeFeatures {
    pub voice_enabled: bool,
    pub stripe_enabled: bool,
    pub self_hosted: bool,
    pub presigned_attachment_uploads: bool,
    pub emails_enabled: bool,
    pub premium_enabled: bool,
    pub stripe_serviceable: bool,
    /// Always `false`: the client runtime does not offer phone verification.
    pub phone_verification_enabled: bool,
}

/// The fields of the TypeScript `ClientRuntimeDiscovery` the desktop shell consumes.
/// Presence of the remaining required sections is validated but not carried.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientRuntimeDiscovery {
    pub generation: InstanceGeneration,
    pub api_code_version: i64,
    pub endpoints: ClientRuntimeEndpoints,
    pub features: ClientRuntimeFeatures,
    /// `push.public_vapid_key`; `None` when the server declared it null.
    pub push_public_vapid_key: Option<String>,
    /// `app_public.branding.premium_product_name`, defaulted like the client runtime does.
    pub premium_product_name: String,
}

fn required<'a, T>(value: &'a Option<T>, path: &str) -> Result<&'a T, DiscoveryError> {
    value.as_ref().ok_or_else(|| {
        DiscoveryError::InvalidDocument(format!("{path} is required by the client runtime"))
    })
}

pub fn project_client_runtime_discovery(
    parsed: &InstanceDiscovery,
) -> Result<ClientRuntimeDiscovery, DiscoveryError> {
    let api_code_version = *required(&parsed.api_code_version, "api_code_version")?;
    let endpoints = project_endpoints(&parsed.endpoints)?;
    let features = project_features(&parsed.features)?;
    required(&parsed.gif, "gif")?;
    required(&parsed.registration, "registration")?;
    required(&parsed.community, "community")?;
    required(&parsed.services, "services")?;
    required(&parsed.limits, "limits")?;
    let push_public_vapid_key =
        required(&parsed.push.public_vapid_key, "push.public_vapid_key")?.clone();
    let premium_product_name = project_app_public(&parsed.app_public)?;
    Ok(ClientRuntimeDiscovery {
        generation: parsed.generation,
        api_code_version,
        endpoints,
        features,
        push_public_vapid_key,
        premium_product_name,
    })
}

fn project_endpoints(
    endpoints: &DiscoveryEndpoints,
) -> Result<ClientRuntimeEndpoints, DiscoveryError> {
    Ok(ClientRuntimeEndpoints {
        discovery_api: endpoints.api.clone(),
        api_client: required(&endpoints.api_client, "endpoints.api_client")?.clone(),
        api_public: required(&endpoints.api_public, "endpoints.api_public")?.clone(),
        gateway: endpoints.gateway.clone(),
        media: required(&endpoints.media, "endpoints.media")?.clone(),
        upload_relay: endpoints.upload_relay.clone(),
        static_cdn: required(&endpoints.static_cdn, "endpoints.static_cdn")?.clone(),
        marketing: required(&endpoints.marketing, "endpoints.marketing")?.clone(),
        docs: endpoints.docs.clone(),
        admin: required(&endpoints.admin, "endpoints.admin")?.clone(),
        invite: required(&endpoints.invite, "endpoints.invite")?.clone(),
        gift: required(&endpoints.gift, "endpoints.gift")?.clone(),
        webapp: required(&endpoints.webapp, "endpoints.webapp")?.clone(),
    })
}

fn project_features(features: &InstanceFeatures) -> Result<ClientRuntimeFeatures, DiscoveryError> {
    let voice_enabled = *required(&features.voice_enabled, "features.voice_enabled")?;
    let stripe_enabled = *required(&features.stripe_enabled, "features.stripe_enabled")?;
    let self_hosted = *required(&features.self_hosted, "features.self_hosted")?;
    let presigned_attachment_uploads = *required(
        &features.presigned_attachment_uploads,
        "features.presigned_attachment_uploads",
    )?;
    let emails_enabled = *required(&features.emails_enabled, "features.emails_enabled")?;
    Ok(ClientRuntimeFeatures {
        voice_enabled,
        stripe_enabled,
        self_hosted,
        presigned_attachment_uploads,
        emails_enabled,
        premium_enabled: features.premium_enabled.unwrap_or(!self_hosted),
        stripe_serviceable: features.stripe_serviceable.unwrap_or(false),
        phone_verification_enabled: false,
    })
}

/// Validates the `app_public` requirements and returns the effective premium product name.
fn project_app_public(app_public: &Option<InstanceAppPublic>) -> Result<String, DiscoveryError> {
    let value = required(app_public, "app_public")?;
    let branding = &value.branding;
    required(&branding.product_name, "app_public.branding.product_name")?;
    required(&branding.icon_url, "app_public.branding.icon_url")?;
    required(&branding.symbol_url, "app_public.branding.symbol_url")?;
    required(&branding.logo_url, "app_public.branding.logo_url")?;
    required(&branding.wordmark_url, "app_public.branding.wordmark_url")?;
    required(&branding.favicon_url, "app_public.branding.favicon_url")?;
    required(&branding.theme_color, "app_public.branding.theme_color")?;
    required(&value.setup.configured, "app_public.setup.configured")?;
    required(&value.setup.admin_url, "app_public.setup.admin_url")?;
    required(&value.legal.terms_url, "app_public.legal.terms_url")?;
    required(&value.legal.privacy_url, "app_public.legal.privacy_url")?;
    required(
        &value.registration.collect_date_of_birth,
        "app_public.registration.collect_date_of_birth",
    )?;
    Ok(branding
        .premium_product_name
        .clone()
        .unwrap_or_else(|| DEFAULT_PREMIUM_PRODUCT_NAME.to_owned()))
}

#[cfg(test)]
mod tests;
