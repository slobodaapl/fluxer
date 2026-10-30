// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `instance_product_names` list of DesktopConfig.ts, validated with the rules of
//! packages/desktop_ipc/src/InstanceProductNameContract.ts.

use serde::Serialize;
use serde_json::Value;

use crate::js::utf16_length;
use crate::local_app::index_html::{contains_control_character, read_instance_product_name};

const INSTANCE_PRODUCT_NAMES_MAX_ENTRIES: usize = 32;
const INSTANCE_PRODUCT_NAME_KEY_MAX_LENGTH: usize = 2048;

/// One persisted entry; the most recently reported instance comes first.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct InstanceProductName {
    pub instance_key: String,
    pub product_name: String,
}

/// `readInstanceProductNameKey`: unlike the name, the key is not trimmed.
fn read_key(value: Option<&Value>) -> Option<&str> {
    let key = value?.as_str()?;
    let length = utf16_length(key);
    (length > 0
        && length <= INSTANCE_PRODUCT_NAME_KEY_MAX_LENGTH
        && !contains_control_character(key))
    .then_some(key)
}

fn read(instance_key: Option<&Value>, product_name: Option<&Value>) -> Option<InstanceProductName> {
    let instance_key = read_key(instance_key)?;
    let product_name = read_instance_product_name(product_name.and_then(Value::as_str))?;
    Some(InstanceProductName {
        instance_key: instance_key.to_owned(),
        product_name: product_name.to_owned(),
    })
}

/// `readInstanceProductNameReport` over the renderer's `{instanceKey, productName}`.
pub fn read_report(report: &Value) -> Option<InstanceProductName> {
    let report = report.as_object()?;
    read(report.get("instanceKey"), report.get("productName"))
}

/// `sanitizeInstanceProductNames`: valid entries, first per key, at most 32.
pub fn sanitize(value: Option<&Value>) -> Option<Vec<InstanceProductName>> {
    let mut entries: Vec<InstanceProductName> = Vec::new();
    for item in value?.as_array()? {
        let Some(item) = item.as_object() else {
            continue;
        };
        let Some(entry) = read(item.get("instance_key"), item.get("product_name")) else {
            continue;
        };
        if entries
            .iter()
            .any(|seen| seen.instance_key == entry.instance_key)
        {
            continue;
        }
        entries.push(entry);
        if entries.len() == INSTANCE_PRODUCT_NAMES_MAX_ENTRIES {
            break;
        }
    }
    (!entries.is_empty()).then_some(entries)
}

/// `rememberInstanceProductName`: moves `accepted` to the front. `None` when it already is.
pub fn remember(
    current: Vec<InstanceProductName>,
    accepted: InstanceProductName,
) -> Option<Vec<InstanceProductName>> {
    if current.first() == Some(&accepted) {
        return None;
    }
    let mut next = current;
    next.retain(|entry| entry.instance_key != accepted.instance_key);
    next.insert(0, accepted);
    next.truncate(INSTANCE_PRODUCT_NAMES_MAX_ENTRIES);
    Some(next)
}

pub fn to_value(entries: &[InstanceProductName]) -> Value {
    serde_json::to_value(entries).expect("instance product names serialise")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(key: &str, name: &str) -> InstanceProductName {
        InstanceProductName {
            instance_key: key.to_owned(),
            product_name: name.to_owned(),
        }
    }

    #[test]
    fn reports_follow_the_contract() {
        assert_eq!(
            read_report(&json!({"instanceKey": "k", "productName": "  Acme  "})),
            Some(entry("k", "Acme"))
        );
        for report in [
            json!(null),
            json!([]),
            json!({"instanceKey": "", "productName": "Acme"}),
            json!({"instanceKey": "k\u{7}", "productName": "Acme"}),
            json!({"instanceKey": "k".repeat(2049), "productName": "Acme"}),
            json!({"instanceKey": "k", "productName": "   "}),
            json!({"instanceKey": "k", "productName": "x".repeat(81)}),
            json!({"instanceKey": 1, "productName": "Acme"}),
        ] {
            assert_eq!(read_report(&report), None, "{report}");
        }
    }

    #[test]
    fn sanitize_drops_invalid_and_duplicate_keys_and_caps_the_list() {
        let value = json!([
            {"instance_key": "a", "product_name": "A"},
            {"instance_key": "a", "product_name": "A2"},
            {"instance_key": "b", "product_name": ""},
            "junk",
            {"instance_key": "c", "product_name": "C"},
        ]);
        assert_eq!(
            sanitize(Some(&value)),
            Some(vec![entry("a", "A"), entry("c", "C")])
        );
        let many: Vec<Value> = (0..40)
            .map(|index| json!({"instance_key": index.to_string(), "product_name": "N"}))
            .collect();
        assert_eq!(sanitize(Some(&Value::Array(many))).unwrap().len(), 32);
        assert_eq!(sanitize(Some(&json!([]))), None);
    }

    #[test]
    fn remember_moves_the_report_to_the_front() {
        let current = vec![entry("a", "A"), entry("b", "B")];
        assert_eq!(remember(current.clone(), entry("a", "A")), None);
        assert_eq!(
            remember(current.clone(), entry("b", "B2")),
            Some(vec![entry("b", "B2"), entry("a", "A")])
        );
        assert_eq!(
            to_value(&[entry("a", "A")]),
            json!([{"instance_key": "a", "product_name": "A"}])
        );
    }
}
