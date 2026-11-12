// SPDX-License-Identifier: AGPL-3.0-or-later
//! Upload policy from LimitResolverAdapter/LimitDiffer/LimitRuleRuntime and
//! PrivacyPreferences.ts. This reads the server snapshot without updating it.
use crate::{Snowflake, User};
use base64::{Engine, engine::general_purpose};
use prost::Message;
use serde_json::Value;

const MIB: u64 = 1024 * 1024;
const FLUXER_EPOCH: u64 = 1_420_070_400_000;
const PREUPLOAD_DEFAULT_ON_AFTER: u64 = 1_780_589_400_000;
const PREFERENCES_MAX_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttachmentPolicy {
    pub max_file_size: u64,
    pub max_attachments: usize,
    pub preupload: bool,
}

impl AttachmentPolicy {
    pub fn resolve(
        discovery: &Value,
        user: &User,
        guild_features: &[String],
        user_settings: &Value,
    ) -> Self {
        // User.ts uses stock limits only when no runtime snapshot exists.
        let mut result = Self {
            max_file_size: 500 * MIB,
            max_attachments: 10,
            preupload: preupload_override(user_settings)
                .unwrap_or_else(|| default_preupload(user.id)),
        };
        let Some(snapshot) = discovery.get("limits").filter(|value| !value.is_null()) else {
            return result;
        };
        // resolveLimits starts with DEFAULT_RESTRICTED_LIMITS. Full snapshots
        // contain rule.limits; v2 expands each rule.overrides over those defaults.
        result.max_file_size = 25 * MIB;
        let wire_v2 = snapshot.get("version").and_then(Value::as_f64) == Some(2.)
            && snapshot.get("defaultsHash").is_some();
        let traits: Vec<_> = user
            .extra
            .get("traits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_lowercase)
            .collect();
        let mut rules: Vec<_> = snapshot
            .get("rules")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        // Stable order is significant for equally specific unfiltered rules.
        rules.sort_by_key(|rule| {
            filter_values(rule, "traits").len() + filter_values(rule, "guildFeatures").len()
        });
        for rule in rules {
            let required_traits = filter_values(rule, "traits");
            let required_features = filter_values(rule, "guildFeatures");
            if !required_traits
                .iter()
                .all(|required| traits.contains(&required.to_lowercase()))
                || !required_features
                    .iter()
                    .all(|required| guild_features.iter().any(|feature| feature == required))
            {
                continue;
            }
            let filtered = !required_traits.is_empty() || !required_features.is_empty();
            let limits = rule.get(if wire_v2 { "overrides" } else { "limits" });
            for (key, restricted) in [
                ("max_attachment_file_size", 25 * MIB),
                ("max_attachments_per_message", 10),
            ] {
                let value = match limits.and_then(|limits| limits.get(key)) {
                    Some(value) => valid_limit(value),
                    None if wire_v2 => Some(restricted),
                    None => None,
                };
                let Some(value) = value else { continue };
                let current = match key {
                    "max_attachment_file_size" => result.max_file_size,
                    _ => result.max_attachments as u64,
                };
                let value = if filtered { current.max(value) } else { value };
                match key {
                    "max_attachment_file_size" => result.max_file_size = value,
                    _ => result.max_attachments = usize::try_from(value).unwrap_or(usize::MAX),
                }
            }
        }
        result
    }
}

fn filter_values<'a>(rule: &'a Value, key: &str) -> Vec<&'a str> {
    rule.get("filters")
        .and_then(|filters| filters.get(key))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn valid_limit(value: &Value) -> Option<u64> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.)
        .map(|value| value.floor() as u64)
}

pub fn default_preupload(user: Snowflake) -> bool {
    (user.0 >> 22).saturating_add(FLUXER_EPOCH) >= PREUPLOAD_DEFAULT_ON_AFTER
}

#[derive(Message)]
struct SyncedPreferences {
    #[prost(message, optional, tag = "80")]
    privacy: Option<PrivacyPreferences>,
}
#[derive(Message)]
struct PrivacyPreferences {
    #[prost(bool, optional, tag = "3")]
    preupload_message_attachments: Option<bool>,
}

/// SyncedPreferencesCodec.ts stores plain standard/URL-safe base64 protobuf.
/// A malformed value follows its lenient decoder's empty-preferences behavior.
pub fn preupload_override(settings: &Value) -> Option<bool> {
    let encoded = settings
        .get("synced_preferences")
        .filter(|value| !value.is_null())
        .or_else(|| settings.get("syncedPreferences"))?
        .as_str()?;
    if encoded.len() > PREFERENCES_MAX_BYTES.div_ceil(3) * 4 {
        return None;
    }
    let decoded = [
        &general_purpose::STANDARD,
        &general_purpose::STANDARD_NO_PAD,
        &general_purpose::URL_SAFE,
        &general_purpose::URL_SAFE_NO_PAD,
    ]
    .into_iter()
    .find_map(|engine| engine.decode(encoded).ok())?;
    if decoded.len() > PREFERENCES_MAX_BYTES {
        return None;
    }
    SyncedPreferences::decode(decoded.as_slice())
        .ok()?
        .privacy?
        .preupload_message_attachments
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(created_ms: u64) -> User {
        User {
            id: Snowflake((created_ms - FLUXER_EPOCH) << 22),
            ..User::default()
        }
    }
    #[test]
    fn preference_default_has_the_exact_account_creation_boundary_and_explicit_false() {
        assert!(!default_preupload(user(PREUPLOAD_DEFAULT_ON_AFTER - 1).id));
        assert!(default_preupload(user(PREUPLOAD_DEFAULT_ON_AFTER).id));
        assert!(!default_preupload(Snowflake(0)));
        // Wire: privacy(80, length-delimited), then optional bool field3=false.
        let settings = json!({"synced_preferences":"ggUCGAA="});
        assert_eq!(preupload_override(&settings), Some(false));
        assert!(
            !AttachmentPolicy::resolve(
                &Value::Null,
                &user(PREUPLOAD_DEFAULT_ON_AFTER + 1),
                &[],
                &settings,
            )
            .preupload
        );
        assert_eq!(
            preupload_override(&json!({"synced_preferences":"ggUCGAE="})),
            Some(true)
        );
        assert_eq!(
            preupload_override(&json!({"synced_preferences":"not valid base64!"})),
            None
        );
    }
    #[test]
    fn codec_reads_privacy_among_unknown_fields_and_rejects_truncation() {
        // Future preferences fields must not obscure the optional privacy flag.
        let bytes = [8, 1, 0x82, 5, 2, 24, 1, 0xa2, 6, 1, 0];
        let encoded = general_purpose::STANDARD.encode(bytes);
        assert_eq!(
            preupload_override(&json!({"synced_preferences":encoded})),
            Some(true)
        );
        assert_eq!(
            preupload_override(&json!({"synced_preferences":"ggUCGAA"})),
            Some(false)
        );
        assert_eq!(
            preupload_override(&json!({"synced_preferences":"ggUFGAE="})),
            None
        );
        assert_eq!(
            preupload_override(&json!({"synced_preferences":null})),
            None
        );
    }
    #[test]
    fn full_snapshot_uses_specificity_traits_guild_features_and_filtered_maxima() {
        let mut account = user(PREUPLOAD_DEFAULT_ON_AFTER);
        account.extra.insert("traits".into(), json!(["PREMIUM"]));
        let document = json!({"limits":{"rules":[
            {"id":"premium","filters":{"traits":["premium"]},"limits":{"max_attachment_file_size":104857600,"max_attachments_per_message":20}},
            {"id":"default","limits":{"max_attachment_file_size":8388608,"max_attachments_per_message":4}},
            {"id":"restricted-guild","filters":{"guildFeatures":["SMALL"]},"limits":{"max_attachment_file_size":1}},
            {"id":"combined","filters":{"traits":["premium"],"guildFeatures":["BOOSTED"]},"limits":{"max_attachments_per_message":30.9}},
            {"id":"other","filters":{"traits":["staff"]},"limits":{"max_attachments_per_message":50}}
        ]}});
        let policy = AttachmentPolicy::resolve(
            &document,
            &account,
            &["SMALL".into(), "BOOSTED".into()],
            &Value::Null,
        );
        assert_eq!(
            (policy.max_file_size, policy.max_attachments),
            (100 * MIB, 30)
        );
        let unboosted = AttachmentPolicy::resolve(&document, &account, &[], &Value::Null);
        assert_eq!(unboosted.max_attachments, 20);
    }
    #[test]
    fn v2_expands_each_rule_defaults_and_missing_snapshot_uses_stock_fallback() {
        let account = user(PREUPLOAD_DEFAULT_ON_AFTER);
        let no_snapshot = AttachmentPolicy::resolve(&Value::Null, &account, &[], &Value::Null);
        assert_eq!(
            (no_snapshot.max_file_size, no_snapshot.max_attachments),
            (500 * MIB, 10)
        );
        let v2 = json!({"limits":{"version":2,"defaultsHash":"source-snapshot","rules":[
            {"id":"first","overrides":{"max_attachment_file_size":1024,"max_attachments_per_message":2}},
            {"id":"second","overrides":{"max_attachments_per_message":3}}
        ]}});
        let policy = AttachmentPolicy::resolve(&v2, &account, &[], &Value::Null);
        assert_eq!(
            (policy.max_file_size, policy.max_attachments),
            (25 * MIB, 3)
        );
        let zero = json!({"limits":{"rules":[{"id":"off","limits":{"max_attachments_per_message":0,"max_attachment_file_size":0}}]}});
        let policy = AttachmentPolicy::resolve(&zero, &account, &[], &Value::Null);
        assert_eq!((policy.max_file_size, policy.max_attachments), (0, 0));
    }
}
