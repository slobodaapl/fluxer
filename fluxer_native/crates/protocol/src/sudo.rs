//! Wire contract for a sudo challenge. This does not perform verification or
//! authorize a sensitive request; callers must preserve the original request
//! and account scope across the challenge before adding a retry flow.
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SudoMethods {
    pub totp: bool,
    pub webauthn: bool,
    pub backup_codes: bool,
    #[serde(default)]
    pub email_code: bool,
    #[serde(default)]
    pub password: bool,
    pub sso_reauth: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SudoChallenge {
    pub has_mfa: bool,
    pub methods: SudoMethods,
}

impl SudoChallenge {
    /// Only a 403 with the exact API error code is a sudo challenge. Malformed
    /// or unrelated errors must remain errors, never successful verification.
    pub fn from_response(status: u16, body: &Value) -> Option<Self> {
        if status != 403 || body.get("code")?.as_str()? != "SUDO_MODE_REQUIRED" {
            return None;
        }
        serde_json::from_value(body.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_classic_challenge_and_rejects_other_errors() {
        let body = json!({"code":"SUDO_MODE_REQUIRED", "message":"Sudo mode required",
            "has_mfa":true, "methods":{"totp":true,"webauthn":false,
            "backup_codes":true,"email_code":false,"password":false,"sso_reauth":false}});
        let challenge = SudoChallenge::from_response(403, &body).unwrap();
        assert!(challenge.has_mfa && challenge.methods.totp && challenge.methods.backup_codes);
        assert!(SudoChallenge::from_response(400, &body).is_none());
        let mut other = body.clone();
        other["code"] = json!("OTHER");
        assert!(SudoChallenge::from_response(403, &other).is_none());
        other["code"] = json!("SUDO_MODE_REQUIRED");
        other["methods"]["totp"] = json!("true");
        assert!(SudoChallenge::from_response(403, &other).is_none());
    }

    #[test]
    fn older_optional_methods_default_to_unavailable() {
        let body = json!({"code":"SUDO_MODE_REQUIRED", "has_mfa":false,
            "methods":{"totp":false,"webauthn":false,"backup_codes":false,"sso_reauth":true}});
        let methods = SudoChallenge::from_response(403, &body).unwrap().methods;
        assert!(!methods.password && !methods.email_code && methods.sso_reauth);
    }
}
