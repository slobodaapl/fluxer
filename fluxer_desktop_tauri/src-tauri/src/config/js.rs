// SPDX-License-Identifier: AGPL-3.0-or-later

//! The JavaScript value rules DesktopConfig.ts and IpcHandlers.ts validate with: `typeof`
//! checks over untyped JSON, `Boolean(value)`, `String.prototype.trim` and UTF-16 `length`.

use serde_json::{Map, Value};

use crate::js::js_trim;

/// `Boolean(value)` for a JSON value; a missing property is `undefined`.
pub fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|number| number != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// `value.trim().length === 0`.
pub fn is_blank(value: &str) -> bool {
    js_trim(value).is_empty()
}

/// The boolean-valued `keys` of an object, in `keys` order; `None` when there are none, the
/// `Object.keys(settings).length > 0 ? settings : undefined` of the TS sanitisers.
pub fn pick_booleans(value: &Value, keys: &[&str]) -> Option<Map<String, Value>> {
    let object = value.as_object()?;
    let picked: Map<String, Value> = keys
        .iter()
        .filter_map(|key| match object.get(*key) {
            Some(Value::Bool(flag)) => Some(((*key).to_owned(), Value::Bool(*flag))),
            _ => None,
        })
        .collect();
    (!picked.is_empty()).then_some(picked)
}

/// `firstBoolean(fallback, source.a, source.b, ...)`.
pub fn first_boolean(source: Option<&Map<String, Value>>, keys: &[&str], fallback: bool) -> bool {
    keys.iter()
        .find_map(|key| source?.get(*key)?.as_bool())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truthiness_follows_javascript() {
        for falsy in [json!(null), json!(false), json!(0), json!(-0.0), json!("")] {
            assert!(!is_truthy(Some(&falsy)), "{falsy}");
        }
        for truthy in [
            json!(true),
            json!(1),
            json!(-1),
            json!("0"),
            json!([]),
            json!({}),
        ] {
            assert!(is_truthy(Some(&truthy)), "{truthy}");
        }
        assert!(!is_truthy(None));
    }

    #[test]
    fn pick_booleans_keeps_only_boolean_listed_keys_in_list_order() {
        let picked = pick_booleans(
            &json!({"b": true, "a": false, "c": 1, "d": true}),
            &["a", "b", "c"],
        )
        .unwrap();
        assert_eq!(Value::Object(picked).to_string(), r#"{"a":false,"b":true}"#);
        assert_eq!(pick_booleans(&json!({"c": "x"}), &["c"]), None);
        assert_eq!(pick_booleans(&json!([true]), &["0"]), None);
    }

    #[test]
    fn first_boolean_skips_non_booleans() {
        let source = json!({"a": "yes", "b": false});
        let source = source.as_object();
        assert!(!first_boolean(source, &["a", "b"], true));
        assert!(first_boolean(source, &["a"], true));
        assert!(first_boolean(None, &["a"], true));
    }
}
