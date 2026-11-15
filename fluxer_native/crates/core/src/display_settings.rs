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
    Compact(bool),
    GifAutoplay(bool),
    FontSize(f64),
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
        DisplaySetting::Compact(v) => json!({"message_display_compact":v}),
        DisplaySetting::GifAutoplay(v) => json!({"gif_auto_play":v}),
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
    fn server_boolean_keys_are_exact() {
        assert_eq!(
            display_settings_patch(&Value::Null, DisplaySetting::Compact(true)).unwrap(),
            json!({"message_display_compact":true})
        );
        assert_eq!(
            display_settings_patch(&Value::Null, DisplaySetting::GifAutoplay(false)).unwrap(),
            json!({"gif_auto_play":false})
        );
    }
}
