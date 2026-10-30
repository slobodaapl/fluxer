// SPDX-License-Identifier: AGPL-3.0-or-later

//! Request payload readers, ported from fluxer_desktop/src/common/PlainRecord.ts.
//!
//! The renderer's payload reaches the shell as JSON, so a "plain record" is a JSON object;
//! accessor properties and symbol keys, which the TS reader refused, cannot survive the
//! bridge's serialization.

use serde_json::{Map, Value};

use crate::js::js_trim;

pub const REQUEST_ID_MAX_LENGTH: usize = 128;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// `readExactPlainRecord`: an object whose keys are exactly `expected_keys`.
pub fn read_exact_record<'a>(
    value: &'a Value,
    expected_keys: &[&str],
) -> Option<&'a Map<String, Value>> {
    let record = value.as_object()?;
    if record.len() != expected_keys.len() {
        return None;
    }
    expected_keys
        .iter()
        .all(|key| record.contains_key(*key))
        .then_some(record)
}

/// `readBoundedString`: a string, trimmed, of 1..=`maximum_length` UTF-16 code units.
pub fn read_bounded_string(value: &Value, maximum_length: usize) -> Option<String> {
    let trimmed = js_trim(value.as_str()?);
    let length = trimmed.encode_utf16().count();
    (length != 0 && length <= maximum_length).then(|| trimmed.to_owned())
}

/// `readRevision` in DesktopRuntimeTransactionRegistry.ts: a safe integer, at least 0 or 1.
pub fn read_revision(value: &Value, allow_zero: bool) -> Option<u64> {
    let minimum = u64::from(!allow_zero);
    let revision = if let Some(unsigned) = value.as_u64() {
        unsigned
    } else if value.is_i64() {
        return None;
    } else {
        let float = value.as_f64()?;
        if float.fract() != 0.0 || float < 0.0 || float > MAX_SAFE_INTEGER as f64 {
            return None;
        }
        float as u64
    };
    (minimum..=MAX_SAFE_INTEGER)
        .contains(&revision)
        .then_some(revision)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn exact_records_need_exactly_the_expected_keys() {
        let keys = ["input", "requestId"];
        assert!(read_exact_record(&json!({"input": 1, "requestId": 2}), &keys).is_some());
        assert!(read_exact_record(&json!({"input": 1}), &keys).is_none());
        assert!(read_exact_record(&json!({"input": 1, "requestId": 2, "x": 3}), &keys).is_none());
        assert!(read_exact_record(&json!({"input": 1, "other": 2}), &keys).is_none());
        assert!(read_exact_record(&json!([1, 2]), &keys).is_none());
        assert!(read_exact_record(&Value::Null, &keys).is_none());
    }

    #[test]
    fn bounded_strings_are_trimmed_and_measured_in_utf16_units() {
        assert_eq!(
            read_bounded_string(&json!("  id \u{FEFF}"), 2).as_deref(),
            Some("id")
        );
        assert_eq!(read_bounded_string(&json!(" \t "), 8), None);
        assert_eq!(read_bounded_string(&json!(5), 8), None);
        // One astral character is two UTF-16 units.
        assert_eq!(read_bounded_string(&json!("\u{1F600}"), 1), None);
        assert!(read_bounded_string(&json!("\u{1F600}"), 2).is_some());
    }

    #[test]
    fn revisions_are_safe_integers_above_the_floor() {
        assert_eq!(read_revision(&json!(0), true), Some(0));
        assert_eq!(read_revision(&json!(0), false), None);
        assert_eq!(read_revision(&json!(3.0), false), Some(3));
        assert_eq!(read_revision(&json!(1.5), true), None);
        assert_eq!(read_revision(&json!(-1), true), None);
        assert_eq!(
            read_revision(&json!(MAX_SAFE_INTEGER), true),
            Some(MAX_SAFE_INTEGER)
        );
        assert_eq!(read_revision(&json!(MAX_SAFE_INTEGER + 1), true), None);
        assert_eq!(read_revision(&json!("1"), true), None);
    }
}
