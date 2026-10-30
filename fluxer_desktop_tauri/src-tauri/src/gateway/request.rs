// SPDX-License-Identifier: AGPL-3.0-or-later

//! Renderer request validation, ported from
//! `fluxer_desktop/src/main/NativeGatewayTransportRequestBoundary.ts` and the limits in
//! `packages/desktop_ipc/src/GatewayTransportContract.ts`.

use serde_json::{Map, Value};
use url::Url;

use crate::error::BridgeError;

pub const CONNECTION_ID_MAX_BYTES: usize = 192;
pub const URL_MAX_BYTES: usize = 2048;
pub const FRAME_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const ERROR_MAX_BYTES: usize = 16 * 1024;
pub const CLOSE_REASON_MAX_BYTES: usize = 123;
const CONNECTION_ID_PREFIX: &str = "gateway-renderer-";
const GATEWAY_MODE: &str = "gateway";
const NORMAL_CLOSE_CODE: i64 = 1000;
const APPLICATION_CLOSE_CODES: std::ops::RangeInclusive<i64> = 3000..=4999;
const INVALID_REQUEST_ERROR: &str = "InvalidNativeGatewayTransportRequestError";

pub struct CreateRequest {
    pub connection_id: String,
    /// The WHATWG serialisation of the requested URL.
    pub url: String,
}

pub struct SendTextRequest {
    pub connection_id: String,
    pub payload: String,
}

pub struct CloseRequest {
    pub connection_id: String,
    pub code: u32,
    pub reason: String,
}

fn invalid(reason: &str) -> BridgeError {
    BridgeError::new(
        INVALID_REQUEST_ERROR,
        format!("Native gateway transport request is invalid: {reason}"),
    )
}

/// `readExactPlainRecord`: an object with exactly `keys`.
fn require_record<'a>(
    value: &'a Value,
    keys: &[&str],
    subject: &str,
) -> Result<&'a Map<String, Value>, BridgeError> {
    value
        .as_object()
        .filter(|record| {
            record.len() == keys.len() && keys.iter().all(|key| record.contains_key(*key))
        })
        .ok_or_else(|| {
            invalid(&format!(
                "{subject} must be a plain object with exactly its own keys"
            ))
        })
}

/// A lowercase version-4 UUID, as the contract's connection id pattern spells it.
fn is_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, &byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// `isNativeGatewayTransportConnectionId`: `gateway-renderer-<uuid>:<uuid>`.
fn is_connection_id(value: &str) -> bool {
    value
        .strip_prefix(CONNECTION_ID_PREFIX)
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(document, connection)| is_uuid_v4(document) && is_uuid_v4(connection))
}

/// `requireNativeGatewayConnectionId`.
pub fn require_connection_id(value: Option<&str>) -> Result<String, BridgeError> {
    let value = value.ok_or_else(|| invalid("connection id must be a string"))?;
    if value.len() > CONNECTION_ID_MAX_BYTES || !is_connection_id(value) {
        return Err(invalid("connection id has an invalid format"));
    }
    Ok(value.to_owned())
}

/// `requireNativeGatewayTransportURL`.
fn require_url(value: Option<&Value>) -> Result<String, BridgeError> {
    let value = value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("URL must be a non-empty string"))?;
    if value.len() > URL_MAX_BYTES {
        return Err(invalid("URL is too long"));
    }
    let parsed = Url::parse(value).map_err(|_| invalid("URL must be absolute"))?;
    if !matches!(parsed.scheme(), "ws" | "wss") {
        return Err(invalid("URL protocol must be ws: or wss:"));
    }
    if !parsed.username().is_empty()
        || parsed
            .password()
            .is_some_and(|password| !password.is_empty())
    {
        return Err(invalid("URL credentials are prohibited"));
    }
    // WHATWG `hash` is empty for both no fragment and a bare `#`.
    if parsed
        .fragment()
        .is_some_and(|fragment| !fragment.is_empty())
    {
        return Err(invalid("URL fragment is prohibited"));
    }
    Ok(parsed.to_string())
}

pub fn require_create_request(value: &Value) -> Result<CreateRequest, BridgeError> {
    let record = require_record(value, &["connectionId", "url", "mode"], "create request")?;
    if record.get("mode").and_then(Value::as_str) != Some(GATEWAY_MODE) {
        return Err(invalid("transport mode must be gateway"));
    }
    Ok(CreateRequest {
        connection_id: require_connection_id(record.get("connectionId").and_then(Value::as_str))?,
        url: require_url(record.get("url"))?,
    })
}

pub fn require_send_text_request(value: &Value) -> Result<SendTextRequest, BridgeError> {
    let record = require_record(value, &["connectionId", "payload"], "text send request")?;
    let payload = record
        .get("payload")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("text payload must be a string"))?;
    if payload.len() > FRAME_MAX_BYTES {
        return Err(invalid("text payload is too large"));
    }
    Ok(SendTextRequest {
        connection_id: require_connection_id(record.get("connectionId").and_then(Value::as_str))?,
        payload: payload.to_owned(),
    })
}

/// The binary payload of `requireNativeGatewaySendBinaryRequest`; the bridge sends it as the
/// raw request body, so only its size is left to check.
pub fn require_binary_payload(payload: &[u8]) -> Result<(), BridgeError> {
    if payload.len() > FRAME_MAX_BYTES {
        return Err(invalid("binary payload is too large"));
    }
    Ok(())
}

pub fn binary_payload_missing() -> BridgeError {
    invalid("binary payload must be a byte buffer")
}

pub fn require_close_request(value: &Value) -> Result<CloseRequest, BridgeError> {
    let record = require_record(value, &["connectionId", "code", "reason"], "close request")?;
    let code = record
        .get("code")
        .and_then(integer)
        .ok_or_else(|| invalid("close code must be an integer"))?;
    if code != NORMAL_CLOSE_CODE && !APPLICATION_CLOSE_CODES.contains(&code) {
        return Err(invalid("close code is not permitted"));
    }
    let reason = record
        .get("reason")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("close reason must be a string"))?;
    if reason.len() > CLOSE_REASON_MAX_BYTES {
        return Err(invalid("close reason is too long"));
    }
    Ok(CloseRequest {
        connection_id: require_connection_id(record.get("connectionId").and_then(Value::as_str))?,
        code: u32::try_from(code).expect("permitted close codes fit in u32"),
        reason: reason.to_owned(),
    })
}

/// `Number.isInteger`: JSON numbers such as `1000.0` count.
fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.fract() == 0.0)
            // Saturates, so an out-of-range integer is refused as not permitted, like the TS.
            .map(|number| number as i64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "gateway-renderer-0f8fad5b-d9cb-469f-a165-70867728950e:7c9e6679-7425-40de-944b-e07fc1f90ae7";

    fn refusal<T>(result: Result<T, BridgeError>) -> String {
        match result {
            Ok(_) => panic!("the request was accepted"),
            Err(error) => error.message().to_owned(),
        }
    }

    #[test]
    fn create_requests_need_exact_keys_gateway_mode_and_a_plain_ws_url() {
        let request = require_create_request(
            &json!({"connectionId": ID, "url": "WSS://Gateway.Example/?v=9", "mode": "gateway"}),
        )
        .unwrap();
        assert_eq!(request.connection_id, ID);
        assert_eq!(request.url, "wss://gateway.example/?v=9");

        for (request, reason) in [
            (
                json!({"connectionId": ID, "url": "wss://a"}),
                "create request must be a plain object with exactly its own keys",
            ),
            (
                json!({"connectionId": ID, "url": "wss://a", "mode": "gateway", "x": 1}),
                "create request must be a plain object with exactly its own keys",
            ),
            (
                json!({"connectionId": ID, "url": "wss://a", "mode": "voice"}),
                "transport mode must be gateway",
            ),
            (
                json!({"connectionId": ID, "url": "", "mode": "gateway"}),
                "URL must be a non-empty string",
            ),
            (
                json!({"connectionId": ID, "url": format!("wss://a/{}", "x".repeat(2048)), "mode": "gateway"}),
                "URL is too long",
            ),
            (
                json!({"connectionId": ID, "url": "/relative", "mode": "gateway"}),
                "URL must be absolute",
            ),
            (
                json!({"connectionId": ID, "url": "https://a", "mode": "gateway"}),
                "URL protocol must be ws: or wss:",
            ),
            (
                json!({"connectionId": ID, "url": "wss://u:p@a", "mode": "gateway"}),
                "URL credentials are prohibited",
            ),
            (
                json!({"connectionId": ID, "url": "wss://a/#x", "mode": "gateway"}),
                "URL fragment is prohibited",
            ),
        ] {
            assert_eq!(
                refusal(require_create_request(&request)),
                format!("Native gateway transport request is invalid: {reason}")
            );
        }
        assert_eq!(
            require_create_request(
                &json!({"connectionId": ID, "url": "wss://a/#", "mode": "gateway"})
            )
            .unwrap()
            .url,
            "wss://a/#"
        );
    }

    #[test]
    fn connection_ids_follow_the_contract_pattern() {
        assert!(require_connection_id(Some(ID)).is_ok());
        for id in [
            "",
            "gateway-renderer-0f8fad5b-d9cb-469f-a165-70867728950e",
            &ID.to_uppercase(),
            &ID.replacen("-469f-", "-369f-", 1),
            &ID.replacen("-a165-", "-c165-", 1),
            &format!("{ID}x"),
        ] {
            assert_eq!(
                refusal(require_connection_id(Some(id))),
                "Native gateway transport request is invalid: connection id has an invalid format"
            );
        }
        assert!(refusal(require_connection_id(None)).ends_with("connection id must be a string"));
    }

    #[test]
    fn text_payloads_are_bounded_strings() {
        assert_eq!(
            require_send_text_request(&json!({"connectionId": ID, "payload": "{}"}))
                .unwrap()
                .payload,
            "{}"
        );
        assert!(
            refusal(require_send_text_request(
                &json!({"connectionId": ID, "payload": 1})
            ))
            .ends_with("text payload must be a string")
        );
        let large = "x".repeat(FRAME_MAX_BYTES + 1);
        assert!(
            refusal(require_send_text_request(
                &json!({"connectionId": ID, "payload": large})
            ))
            .ends_with("text payload is too large")
        );
        assert!(
            refusal(require_binary_payload(
                vec![0; FRAME_MAX_BYTES + 1].as_slice()
            ))
            .ends_with("binary payload is too large")
        );
        assert!(require_binary_payload(&[1, 2, 3]).is_ok());
    }

    #[test]
    fn close_requests_allow_normal_and_application_codes() {
        for code in [json!(1000), json!(3000), json!(4999), json!(4000.0)] {
            assert!(
                require_close_request(&json!({"connectionId": ID, "code": code, "reason": "bye"}))
                    .is_ok()
            );
        }
        for (code, reason) in [
            (json!(1001), "close code is not permitted"),
            (json!(5000), "close code is not permitted"),
            (json!(1000.5), "close code must be an integer"),
            (json!("1000"), "close code must be an integer"),
        ] {
            assert!(
                refusal(require_close_request(
                    &json!({"connectionId": ID, "code": code, "reason": ""})
                ))
                .ends_with(reason)
            );
        }
        let long = "é".repeat(62);
        assert!(
            refusal(require_close_request(
                &json!({"connectionId": ID, "code": 1000, "reason": long})
            ))
            .ends_with("close reason is too long")
        );
    }
}
