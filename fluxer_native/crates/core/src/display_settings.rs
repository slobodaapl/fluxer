// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source: preferences.proto/accessibility.proto and Accessibility.ts.
//! Preserve unknown protobuf fields when changing one accessibility preference.
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub enum DisplaySetting {
    FavoriteGif { entry: FavoriteGif, add: bool },
    Compact(bool),
    GifAutoplay(bool),
    AnimateEmoji(bool),
    FontSize(f64),
    DefaultGuildsRestricted(bool),
    BotDefaultGuildsRestricted(bool),
    FriendSource { flag: u64, enabled: bool },
    AddTrustedDomain(String),
    TrustedDomains(Vec<String>),
}
fn varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut n = 0u64;
    for shift in (0..70).step_by(7) {
        let b = *bytes.get(*at)?;
        *at += 1;
        if shift == 63 && b > 1 {
            return None;
        }
        n |= ((b & 127) as u64) << shift;
        if b < 128 {
            return Some(n);
        }
    }
    None
}
fn encode(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push((n as u8) | 128);
        n >>= 7;
    }
    out.push(n as u8);
}
fn fields(bytes: &[u8]) -> Option<Vec<(u64, usize, usize, usize)>> {
    let mut at = 0;
    let mut out = vec![];
    while at < bytes.len() {
        let start = at;
        let tag = varint(bytes, &mut at)?;
        if tag >> 3 == 0 {
            return None;
        }
        let payload = match tag & 7 {
            0 => {
                varint(bytes, &mut at)?;
                at
            }
            1 => {
                at = at.checked_add(8)?;
                at - 8
            }
            2 => {
                let len = usize::try_from(varint(bytes, &mut at)?).ok()?;
                let p = at;
                at = at.checked_add(len)?;
                p
            }
            5 => {
                at = at.checked_add(4)?;
                at - 4
            }
            _ => return None,
        };
        if at > bytes.len() {
            return None;
        }
        out.push((tag, start, payload, at));
    }
    Some(out)
}
fn decoded(settings: &Value) -> Option<Vec<u8>> {
    match settings.get("synced_preferences") {
        None | Some(Value::Null) => Some(vec![]),
        Some(Value::String(s)) => STANDARD
            .decode(s)
            .or_else(|_| URL_SAFE.decode(s))
            .or_else(|_| STANDARD_NO_PAD.decode(s))
            .or_else(|_| URL_SAFE_NO_PAD.decode(s))
            .ok(),
        _ => None,
    }
}
fn accessibility(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![];
    for (tag, _, p, e) in fields(bytes)? {
        if tag == 10 {
            out.extend_from_slice(&bytes[p..e]);
        }
    }
    Some(out)
}
/// URL favorites are field 25 of SyncedPreferences. Decode only the fields used
/// by the picker; prost retains compatibility with other preference fields.
#[derive(Clone, PartialEq, prost::Message)]
struct GifFavoritePreferences {
    #[prost(message, repeated, tag = "1")]
    entries: Vec<FavoriteGif>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FavoriteGif {
    #[prost(string, tag = "1")]
    pub url: String,
    #[prost(string, tag = "2")]
    pub proxy_url: String,
    #[prost(uint32, tag = "3")]
    pub width: u32,
    #[prost(uint32, tag = "4")]
    pub height: u32,
    #[prost(map = "string, message", tag = "5")]
    pub media: std::collections::HashMap<String, FavoriteGifFormat>,
    #[prost(string, tag = "6")]
    pub content_type: String,
    #[prost(string, tag = "7")]
    pub placeholder: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FavoriteGifFormat {
    #[prost(string, tag = "1")]
    pub src: String,
    #[prost(string, tag = "2")]
    pub proxy_src: String,
    #[prost(uint32, tag = "3")]
    pub width: u32,
    #[prost(uint32, tag = "4")]
    pub height: u32,
}

pub fn favorite_gifs(settings: &Value) -> Option<Vec<FavoriteGif>> {
    use prost::Message;
    let bytes = decoded(settings)?;
    let mut payload = Vec::new();
    for (tag, _, start, end) in fields(&bytes)? {
        if tag == (25 << 3 | 2) {
            payload.extend_from_slice(&bytes[start..end]);
        }
    }
    Some(
        GifFavoritePreferences::decode(payload.as_slice())
            .ok()?
            .entries,
    )
}

/// Match classic's removal of attachment signature query fields.
pub fn favorite_gif_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    let raw = url.trim();
    let (head, fragment) = raw
        .split_once('#')
        .map_or((raw, ""), |(a, _)| (a, &raw[a.len()..]));
    let (base, query) = head.split_once('?').unwrap_or((head, ""));
    let kept: Vec<_> = query
        .split('&')
        .filter(|field| {
            !field.is_empty()
                && *field != "="
                && !matches!(
                    field.split('=').next().unwrap_or(""),
                    "ex" | "is" | "hm" | "uc"
                )
        })
        .collect();
    Some(format!(
        "{}{}{}",
        base,
        if kept.is_empty() {
            String::new()
        } else {
            format!("?{}", kept.join("&"))
        },
        fragment
    ))
}

fn favorite_gif_patch(
    settings: &Value,
    mut entry: FavoriteGif,
    add: bool,
) -> Result<Value, String> {
    use prost::Message;
    let target = favorite_gif_url(&entry.url).ok_or("Invalid GIF URL")?;
    entry.url = target.clone();
    let bytes = decoded(settings).ok_or("Cannot read synced preferences; reload before saving")?;
    let mut payload = Vec::new();
    let mut outer = Vec::new();
    for (tag, start, p, end) in fields(&bytes).ok_or("Invalid synced preferences")? {
        if tag == (25 << 3 | 2) {
            payload.extend_from_slice(&bytes[p..end]);
        } else {
            outer.extend_from_slice(&bytes[start..end]);
        }
    }
    let mut updated = Vec::new();
    let mut count = 0;
    let mut found = false;
    for (tag, start, p, end) in fields(&payload).ok_or("Invalid GIF preferences")? {
        if tag == 10 {
            let existing =
                FavoriteGif::decode(&payload[p..end]).map_err(|_| "Invalid GIF favorite")?;
            count += 1;
            if favorite_gif_url(&existing.url).as_deref() == Some(&target) {
                found = true;
                if !add {
                    continue;
                }
            }
        }
        updated.extend_from_slice(&payload[start..end]);
    }
    if add && !found {
        if favorite_gifs_saved_media(settings).ok_or("Invalid GIF preferences")? {
            return Err("GIF favorites are configured as saved media".into());
        }
        if count >= 10000 {
            return Err("Favorite GIF limit reached".into());
        }
        let encoded = entry.encode_to_vec();
        encode(10, &mut updated);
        encode(encoded.len() as u64, &mut updated);
        updated.extend(encoded);
    }
    if updated.len() > 384 * 1024 {
        return Err("Favorite GIF settings are full".into());
    }
    encode((25 << 3) | 2, &mut outer);
    encode(updated.len() as u64, &mut outer);
    outer.extend(updated);
    Ok(json!({"synced_preferences": STANDARD.encode(outer)}))
}

pub fn favorite_gifs_saved_media(settings: &Value) -> Option<bool> {
    let bytes = decoded(settings)?;
    let mut payload = Vec::new();
    for (tag, _, p, e) in fields(&bytes)? {
        if tag == (25 << 3 | 2) {
            payload.extend_from_slice(&bytes[p..e]);
        }
    }
    let mut enabled = false;
    for (tag, start, _, _) in fields(&payload)? {
        if tag == 16 {
            let mut at = start;
            varint(&payload, &mut at)?;
            enabled = varint(&payload, &mut at)? != 0;
        }
    }
    Some(enabled)
}

pub fn message_font_size(settings: &Value) -> f32 {
    let value = (|| {
        let bytes = decoded(settings)?;
        let a = accessibility(&bytes)?;
        let mut result = None;
        for (tag, _, p, e) in fields(&a)? {
            if tag == 97 {
                result = Some(f64::from_le_bytes(a[p..e].try_into().ok()?));
            }
        }
        result
    })();
    value
        .filter(|v| v.is_finite())
        .unwrap_or(16.)
        .clamp(8., 48.) as f32
}
pub fn compact_messages(settings: &Value) -> bool {
    settings
        .get("message_display_compact")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
pub fn display_settings_patch(settings: &Value, setting: DisplaySetting) -> Result<Value, String> {
    Ok(match setting {
        DisplaySetting::FavoriteGif { entry, add } => {
            return favorite_gif_patch(settings, entry, add);
        }
        DisplaySetting::DefaultGuildsRestricted(v) => json!({"default_guilds_restricted":v}),
        DisplaySetting::BotDefaultGuildsRestricted(v) => json!({"bot_default_guilds_restricted":v}),
        DisplaySetting::FriendSource { flag, enabled } => {
            if !matches!(flag, 1 | 2 | 4) {
                return Err("Invalid friend request source".into());
            }
            let flags = settings
                .get("friend_source_flags")
                .and_then(Value::as_u64)
                .ok_or("Friend request settings are still loading")?;
            json!({"friend_source_flags": if enabled { flags | flag } else { flags & !flag }})
        }
        DisplaySetting::Compact(v) => json!({"message_display_compact":v}),
        DisplaySetting::GifAutoplay(v) => json!({"gif_auto_play":v}),
        DisplaySetting::AnimateEmoji(v) => json!({"animate_emoji":v}),
        DisplaySetting::AddTrustedDomain(host) => {
            if host.contains(['*', '/', '?', '#', '@', '\\'])
                || host
                    .chars()
                    .any(|character| character.is_control() || character.is_whitespace())
            {
                return Err("Invalid trusted domain".into());
            }
            let host = url::Host::parse(&host)
                .map_err(|_| "Invalid trusted domain")?
                .to_string();
            let values = settings
                .get("trusted_domains")
                .and_then(Value::as_array)
                .ok_or("Trusted domains are still loading")?;
            let mut domains = values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or("Invalid trusted domains")
                })
                .collect::<Result<Vec<_>, _>>()?;
            if domains
                .iter()
                .any(|domain| domain == "*" || domain == &host)
            {
                return Ok(json!({}));
            }
            domains.push(host);
            return display_settings_patch(settings, DisplaySetting::TrustedDomains(domains));
        }
        DisplaySetting::TrustedDomains(domains) => {
            if domains.len() > 1000
                || domains
                    .iter()
                    .any(|domain| domain.is_empty() || domain.encode_utf16().count() > 253)
                || (domains.iter().any(|domain| domain == "*") && domains.len() != 1)
            {
                return Err("Invalid trusted domains".into());
            }
            json!({"trusted_domains": domains})
        }
        DisplaySetting::FontSize(size) => {
            if !size.is_finite() || !(8. ..=48.).contains(&size) {
                return Err("Text size must be between 8 and 48 pixels".into());
            }
            let bytes =
                decoded(settings).ok_or("Cannot read synced preferences; reload before saving")?;
            let a = accessibility(&bytes).ok_or("Invalid synced preferences")?;
            let mut updated = vec![];
            for (tag, s, _, e) in fields(&a).ok_or("Invalid accessibility preferences")? {
                if tag >> 3 != 12 {
                    updated.extend_from_slice(&a[s..e]);
                }
            }
            encode(97, &mut updated);
            updated.extend_from_slice(&size.to_le_bytes());
            let mut result = vec![];
            for (tag, s, _, e) in fields(&bytes).ok_or("Invalid synced preferences")? {
                if tag >> 3 != 1 {
                    result.extend_from_slice(&bytes[s..e]);
                }
            }
            encode(10, &mut result);
            encode(updated.len() as u64, &mut result);
            result.extend(updated);
            json!({"synced_preferences":STANDARD.encode(result)})
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gif_mutation_preserves_other_fields_and_removes_only_target() {
        use prost::Message;
        let entry = FavoriteGif {
            url: "https://example.org/a.gif?ex=old&x=1".into(),
            ..Default::default()
        };
        let encoded = entry.encode_to_vec();
        let mut payload = vec![16, 0, 24, 1, 0x78, 42];
        encode(10, &mut payload);
        encode(encoded.len() as u64, &mut payload);
        payload.extend(encoded);
        let mut original = vec![10, 2, 16, 1];
        encode(202, &mut original);
        encode(payload.len() as u64, &mut original);
        original.extend(payload);
        let settings = json!({"synced_preferences": STANDARD.encode(original)});
        let other = FavoriteGif {
            url: "https://example.org/b.gif".into(),
            ..Default::default()
        };
        let added = display_settings_patch(
            &settings,
            DisplaySetting::FavoriteGif {
                entry: other.clone(),
                add: true,
            },
        )
        .unwrap();
        assert_eq!(favorite_gifs(&added).unwrap().len(), 2);
        let removed =
            display_settings_patch(&added, DisplaySetting::FavoriteGif { entry, add: false })
                .unwrap();
        assert_eq!(favorite_gifs(&removed).unwrap(), vec![other]);
        let bytes = decoded(&removed).unwrap();
        assert!(bytes.starts_with(&[10, 2, 16, 1]));
        assert!(bytes.windows(6).any(|w| w == [16, 0, 24, 1, 0x78, 42]));
        assert!(favorite_gif_url("file:///tmp/a.gif").is_none());
        assert!(favorite_gif_url("https://user:pass@example.org/a.gif").is_none());
    }
    #[test]
    fn saved_media_mode_refuses_url_addition() {
        let settings = json!({"synced_preferences":STANDARD.encode([0xca, 1, 2, 16, 1])});
        let entry = FavoriteGif {
            url: "https://example.org/a.gif".into(),
            ..Default::default()
        };
        assert!(
            display_settings_patch(&settings, DisplaySetting::FavoriteGif { entry, add: true })
                .is_err()
        );
    }
    #[test]
    fn font_patch_preserves_unknown_fields() {
        let original = vec![10, 4, 16, 1, 112, 1, 0x82, 5, 2, 24, 1];
        let settings = json!({"synced_preferences":STANDARD.encode(&original)});
        let patch = display_settings_patch(&settings, DisplaySetting::FontSize(22.)).unwrap();
        assert_eq!(message_font_size(&patch), 22.);
        let bytes = decoded(&patch).unwrap();
        assert!(bytes.windows(5).any(|w| w == [0x82, 5, 2, 24, 1]));
        assert!(accessibility(&bytes).unwrap().starts_with(&[16, 1, 112, 1]));
    }
    #[test]
    fn rejects_corrupt_preferences_and_invalid_sizes() {
        assert!(
            display_settings_patch(
                &json!({"synced_preferences":"AA=="}),
                DisplaySetting::FontSize(20.)
            )
            .is_err()
        );
        for size in [0., 49., f64::NAN] {
            assert!(display_settings_patch(&Value::Null, DisplaySetting::FontSize(size)).is_err());
        }
        assert_eq!(message_font_size(&Value::Null), 16.);
    }
    #[test]
    fn privacy_patches_preserve_other_sources_and_omit_community_lists() {
        let settings = json!({"friend_source_flags": 0x83, "restricted_guilds": ["123"]});
        assert_eq!(
            display_settings_patch(
                &settings,
                DisplaySetting::FriendSource {
                    flag: 1,
                    enabled: false
                }
            )
            .unwrap(),
            json!({"friend_source_flags": 0x82})
        );
        assert_eq!(
            display_settings_patch(&settings, DisplaySetting::DefaultGuildsRestricted(true))
                .unwrap(),
            json!({"default_guilds_restricted": true})
        );
        assert_eq!(
            display_settings_patch(&settings, DisplaySetting::BotDefaultGuildsRestricted(false))
                .unwrap(),
            json!({"bot_default_guilds_restricted": false})
        );
        assert!(
            display_settings_patch(
                &Value::Null,
                DisplaySetting::FriendSource {
                    flag: 1,
                    enabled: true
                }
            )
            .is_err()
        );
        assert!(
            display_settings_patch(
                &settings,
                DisplaySetting::FriendSource {
                    flag: 8,
                    enabled: true
                }
            )
            .is_err()
        );
    }
    #[test]
    fn trusted_domains_patch_obeys_account_contract() {
        assert_eq!(
            display_settings_patch(
                &Value::Null,
                DisplaySetting::TrustedDomains(vec!["example.org".into()])
            )
            .unwrap(),
            json!({"trusted_domains":["example.org"]})
        );
        assert!(
            display_settings_patch(
                &Value::Null,
                DisplaySetting::TrustedDomains(vec!["*".into(), "example.org".into()])
            )
            .is_err()
        );
        assert!(
            display_settings_patch(
                &Value::Null,
                DisplaySetting::TrustedDomains(vec!["".into()])
            )
            .is_err()
        );
        assert!(
            display_settings_patch(
                &Value::Null,
                DisplaySetting::TrustedDomains(vec!["a".repeat(254)])
            )
            .is_err()
        );
        assert!(
            display_settings_patch(
                &Value::Null,
                DisplaySetting::TrustedDomains(vec!["a".into(); 1001])
            )
            .is_err()
        );
    }
    #[test]
    fn server_boolean_keys_are_exact() {
        assert_eq!(
            display_settings_patch(&Value::Null, DisplaySetting::Compact(true)).unwrap(),
            json!({"message_display_compact":true})
        );
        assert_eq!(
            display_settings_patch(&Value::Null, DisplaySetting::AnimateEmoji(false)).unwrap(),
            json!({"animate_emoji":false})
        );
        assert_eq!(
            display_settings_patch(&Value::Null, DisplaySetting::GifAutoplay(false)).unwrap(),
            json!({"gif_auto_play":false})
        );
    }

    #[test]
    fn trusted_domain_append_uses_current_account_settings() {
        let add = |host: &str| DisplaySetting::AddTrustedDomain(host.into());
        let settings = json!({"trusted_domains":["other.example"],"gif_auto_play":true});
        assert_eq!(
            display_settings_patch(&settings, add("EXAMPLE.org")).unwrap(),
            json!({"trusted_domains":["other.example","example.org"]})
        );
        assert_eq!(
            display_settings_patch(&settings, add("other.example")).unwrap(),
            json!({})
        );
        assert_eq!(
            display_settings_patch(&json!({"trusted_domains":["*"]}), add("example.org")).unwrap(),
            json!({})
        );
        assert!(display_settings_patch(&Value::Null, add("example.org")).is_err());
        for invalid in [
            "*.example.org",
            "example.org/path",
            "attacker@example.org",
            "example.org:80",
        ] {
            assert!(
                display_settings_patch(&settings, add(invalid)).is_err(),
                "{invalid}"
            );
        }
    }
}
