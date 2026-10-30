// SPDX-License-Identifier: AGPL-3.0-or-later

//! Request validation of DesktopAppStorage.ts. Payloads arrive as JSON values; a missing
//! argument or field is `null`, which every check treats like the `undefined` the TS saw.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::errors;
use crate::error::BridgeError;

pub(super) const LOOKUP_KEY_MAX_BYTES: usize = 1024;
pub(super) const ENTRY_VALUE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MARKER_VALUE_MAX_BYTES: usize = 64 * 1024;
pub(super) const RECORD_MAX_BYTES: usize = 8 * 1024 * 1024;
pub(super) const BATCH_ENTRIES_MAX: usize = 65_536;
pub(super) const BATCH_RECORDS_MAX: usize = 1024;
pub(super) const BATCH_KEYS_MAX: usize = 65_536;

type Checked<T> = Result<T, BridgeError>;

static NULL: Value = Value::Null;

/// `DesktopAccountRecord`; also the shape the native store reads and writes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRecord {
    pub storage_key: String,
    pub record: Value,
}

/// `DesktopStoreMarker`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct Marker {
    pub key: String,
    pub value: String,
}

pub(super) fn field<'a>(record: &'a Map<String, Value>, name: &str) -> &'a Value {
    record.get(name).unwrap_or(&NULL)
}

/// `JSON.stringify(value)` byte length.
pub(super) fn json_bytes(value: &Value) -> usize {
    value.to_string().len()
}

pub(super) fn record_bytes(key: &str, record: &Value) -> usize {
    key.len() + json_bytes(record)
}

pub(super) fn require_string<'a>(value: &'a Value, context: &str) -> Checked<&'a str> {
    value
        .as_str()
        .ok_or_else(|| errors::request(context, "must be a string"))
}

pub(super) fn require_bounded_string<'a>(
    value: &'a Value,
    context: &str,
    max_bytes: usize,
) -> Checked<&'a str> {
    let text = require_string(value, context)?;
    if text.len() > max_bytes {
        return Err(errors::request(
            context,
            &format!("must be at most {max_bytes} bytes"),
        ));
    }
    Ok(text)
}

pub(super) fn require_lookup_key<'a>(value: &'a Value, context: &str) -> Checked<&'a str> {
    let text = require_bounded_string(value, context, LOOKUP_KEY_MAX_BYTES)?;
    if text.is_empty() || text.contains('\0') {
        return Err(errors::request(
            context,
            "must be a non-empty NUL-free string",
        ));
    }
    Ok(text)
}

/// JSON numbers are always finite; `NaN` and infinities reach the shell as `null`.
pub(super) fn require_finite_number(value: &Value, context: &str) -> Checked<f64> {
    value
        .as_f64()
        .ok_or_else(|| errors::request(context, "must be a finite number"))
}

pub(super) fn require_boolean(value: &Value, context: &str) -> Checked<bool> {
    value
        .as_bool()
        .ok_or_else(|| errors::request(context, "must be a boolean"))
}

pub(super) fn require_array<'a>(
    value: &'a Value,
    context: &str,
    max_length: usize,
) -> Checked<&'a [Value]> {
    let items = value
        .as_array()
        .ok_or_else(|| errors::request(context, "must be an array"))?;
    if items.len() > max_length {
        return Err(errors::request(
            context,
            &format!("must hold at most {max_length} items"),
        ));
    }
    Ok(items)
}

pub(super) fn require_record<'a>(
    value: &'a Value,
    context: &str,
) -> Checked<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| errors::request(context, "must be an object"))
}

/// `requireJSONObject`. Any JSON value serialises, so only the size can be refused.
pub(super) fn require_json_object<'a>(
    value: &'a Value,
    context: &str,
    max_bytes: usize,
) -> Checked<&'a Map<String, Value>> {
    let record = require_record(value, context)?;
    if json_bytes(value) > max_bytes {
        return Err(errors::request(
            context,
            &format!("must be at most {max_bytes} bytes"),
        ));
    }
    Ok(record)
}

pub(super) fn require_marker(value: &Value, context: &str) -> Checked<Option<Marker>> {
    if value.is_null() {
        return Ok(None);
    }
    let record = require_record(value, context)?;
    Ok(Some(Marker {
        key: require_lookup_key(field(record, "key"), &format!("{context}.key"))?.to_owned(),
        value: require_bounded_string(
            field(record, "value"),
            &format!("{context}.value"),
            MARKER_VALUE_MAX_BYTES,
        )?
        .to_owned(),
    }))
}

pub(super) fn require_account_record(value: &Value, context: &str) -> Checked<AccountRecord> {
    let record = require_record(value, context)?;
    let storage_key = require_lookup_key(
        field(record, "storageKey"),
        &format!("{context}.storageKey"),
    )?;
    let body = field(record, "record");
    require_json_object(body, &format!("{context}.record"), RECORD_MAX_BYTES)?;
    Ok(AccountRecord {
        storage_key: storage_key.to_owned(),
        record: body.clone(),
    })
}

/// `requireKnownInstanceRecord`: returns the instance key; the whole value is the record.
pub(super) fn require_known_instance_record<'a>(
    value: &'a Value,
    context: &str,
) -> Checked<&'a str> {
    let record = require_json_object(value, context, RECORD_MAX_BYTES)?;
    let instance_key = require_lookup_key(
        field(record, "instanceKey"),
        &format!("{context}.instanceKey"),
    )?;
    require_bounded_string(
        field(record, "domain"),
        &format!("{context}.domain"),
        LOOKUP_KEY_MAX_BYTES,
    )?;
    require_bounded_string(
        field(record, "displayName"),
        &format!("{context}.displayName"),
        LOOKUP_KEY_MAX_BYTES,
    )?;
    require_finite_number(field(record, "lastUsed"), &format!("{context}.lastUsed"))?;
    Ok(instance_key)
}

/// `readKnownInstanceRecord`: stored records the renderer can use.
pub(super) fn is_known_instance_record(value: &Value) -> bool {
    let Some(record) = value.as_object() else {
        return false;
    };
    field(record, "instanceKey").is_string()
        && field(record, "domain").is_string()
        && field(record, "displayName").is_string()
        && field(record, "lastUsed").is_number()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn refusal(result: Checked<impl std::fmt::Debug>) -> String {
        let error = result.unwrap_err();
        assert_eq!(error.name(), "DesktopAppStoreRequestError");
        error.message().to_owned()
    }

    #[test]
    fn lookup_keys_are_bounded_non_empty_and_nul_free() {
        let longest = "k".repeat(LOOKUP_KEY_MAX_BYTES);
        assert_eq!(
            require_lookup_key(&json!(longest), "storage key").unwrap(),
            longest
        );
        assert_eq!(
            refusal(require_lookup_key(
                &json!(format!("{longest}k")),
                "storage key"
            )),
            "Desktop app store request is invalid: storage key must be at most 1024 bytes"
        );
        // 342 three-byte characters are 1026 bytes but 342 UTF-16 units.
        assert!(require_lookup_key(&json!("€".repeat(342)), "storage key").is_err());
        assert_eq!(
            refusal(require_lookup_key(&json!(""), "storage key")),
            "Desktop app store request is invalid: storage key must be a non-empty NUL-free string"
        );
        assert!(require_lookup_key(&json!("a\0b"), "storage key").is_err());
        assert_eq!(
            refusal(require_lookup_key(&Value::Null, "storage key")),
            "Desktop app store request is invalid: storage key must be a string"
        );
        assert!(require_lookup_key(&json!(7), "storage key").is_err());
    }

    #[test]
    fn values_and_markers_have_their_own_limits() {
        let entry = "v".repeat(ENTRY_VALUE_MAX_BYTES);
        assert!(
            require_bounded_string(&json!(entry), "storage value", ENTRY_VALUE_MAX_BYTES).is_ok()
        );
        assert_eq!(
            refusal(require_bounded_string(
                &json!(format!("{entry}v")),
                "storage value",
                ENTRY_VALUE_MAX_BYTES
            )),
            "Desktop app store request is invalid: storage value must be at most 8388608 bytes"
        );

        let marker =
            json!({"key": "legacy_import.v1", "value": "m".repeat(MARKER_VALUE_MAX_BYTES)});
        assert!(
            require_marker(&marker, "storage import request.marker")
                .unwrap()
                .is_some()
        );
        let oversized =
            json!({"key": "legacy_import.v1", "value": "m".repeat(MARKER_VALUE_MAX_BYTES + 1)});
        assert_eq!(
            refusal(require_marker(&oversized, "storage import request.marker")),
            "Desktop app store request is invalid: storage import request.marker.value must be at most 65536 bytes"
        );
        assert_eq!(require_marker(&Value::Null, "m").unwrap(), None);
        assert_eq!(
            refusal(require_marker(&json!([]), "m")),
            "Desktop app store request is invalid: m must be an object"
        );
    }

    #[test]
    fn account_records_need_a_key_and_an_object_record_within_the_record_limit() {
        let account = json!({"storageKey": "acct", "record": {"userId": "1"}});
        assert_eq!(
            require_account_record(&account, "account").unwrap(),
            AccountRecord {
                storage_key: "acct".to_owned(),
                record: json!({"userId": "1"}),
            }
        );
        assert_eq!(
            refusal(require_account_record(
                &json!({"storageKey": "acct", "record": []}),
                "account"
            )),
            "Desktop app store request is invalid: account.record must be an object"
        );
        assert_eq!(
            refusal(require_account_record(&json!({"record": {}}), "account")),
            "Desktop app store request is invalid: account.storageKey must be a string"
        );
        // {"p":"…"} adds 8 bytes of JSON around the payload.
        let fits = json!({"storageKey": "acct", "record": {"p": "x".repeat(RECORD_MAX_BYTES - 8)}});
        assert!(require_account_record(&fits, "account").is_ok());
        let too_big =
            json!({"storageKey": "acct", "record": {"p": "x".repeat(RECORD_MAX_BYTES - 7)}});
        assert_eq!(
            refusal(require_account_record(&too_big, "account")),
            "Desktop app store request is invalid: account.record must be at most 8388608 bytes"
        );
    }

    #[test]
    fn arrays_are_bounded() {
        let keys = Value::Array(vec![json!("k"); BATCH_RECORDS_MAX + 1]);
        assert_eq!(
            refusal(require_array(
                &keys,
                "account import request.records",
                BATCH_RECORDS_MAX
            )),
            "Desktop app store request is invalid: account import request.records must hold at most 1024 items"
        );
        assert_eq!(
            refusal(require_array(
                &json!({}),
                "storage writes",
                BATCH_ENTRIES_MAX
            )),
            "Desktop app store request is invalid: storage writes must be an array"
        );
    }

    #[test]
    fn known_instances_require_typed_fields() {
        let instance =
            json!({"instanceKey": "i", "domain": "d", "displayName": "n", "lastUsed": 1});
        assert_eq!(
            require_known_instance_record(&instance, "known instance").unwrap(),
            "i"
        );
        assert!(is_known_instance_record(&instance));
        let missing = json!({"instanceKey": "i", "domain": "d", "displayName": "n"});
        assert_eq!(
            refusal(require_known_instance_record(&missing, "known instance")),
            "Desktop app store request is invalid: known instance.lastUsed must be a finite number"
        );
        assert!(!is_known_instance_record(&missing));
        let long_domain = json!({"instanceKey": "i", "domain": "d".repeat(1025), "displayName": "n", "lastUsed": 1});
        assert!(require_known_instance_record(&long_domain, "known instance").is_err());
    }
}
