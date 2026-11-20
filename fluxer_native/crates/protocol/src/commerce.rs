// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::Deserialize;
use serde_json::Value;

/// The classic settings visibility rule from PremiumUtils and runtime discovery.
pub fn premium_features_visible(features: &Value) -> bool {
    match features.get("self_hosted").and_then(Value::as_bool) {
        Some(false) => true,
        Some(true) => features.get("premium_enabled").and_then(Value::as_bool) == Some(true),
        None => false,
    }
}

/// The entitlement portion of GET /premium/state. Billing and pricing data are
/// deliberately excluded from the settings result.
#[derive(Deserialize)]
pub struct PremiumStatus {
    pub actual: PremiumEntitlement,
    pub effective: PremiumPerks,
    pub subscription_provider: Option<SubscriptionProvider>,
}

#[derive(Deserialize)]
pub struct PremiumEntitlement {
    pub is_visionary: bool,
    pub has_active_paid_premium: bool,
    pub premium_since: Option<String>,
    pub premium_until: Option<String>,
    pub premium_will_cancel: bool,
    pub premium_billing_cycle: Option<BillingCycle>,
    pub premium_lifetime_sequence: Option<u64>,
    pub premium_grace_ends_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingCycle {
    Monthly,
    Yearly,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
pub struct PremiumPerks {
    pub is_premium: bool,
    pub premium_perks_disabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionProvider {
    Stripe,
    AppStore,
    GooglePlay,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
pub struct GiftInventoryEntry {
    pub code: String,
    pub duration_type: GiftDurationType,
    pub duration_quantity: i64,
    pub created_at: String,
    pub redeemed_at: Option<String>,
    pub redeemed_by: Option<GiftRecipient>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GiftDurationType {
    Days,
    Weeks,
    Months,
    Years,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
pub struct GiftRecipient {
    pub username: String,
    pub global_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commerce_settings_follow_tiering_and_ignore_stripe() {
        for (features, expected) in [
            (json!({"self_hosted": false}), true),
            (
                json!({"self_hosted": false, "premium_enabled": false}),
                true,
            ),
            (
                json!({"self_hosted": true, "premium_enabled": true, "stripe_enabled": false}),
                true,
            ),
            (json!({"self_hosted": true, "stripe_enabled": true}), false),
            (
                json!({"self_hosted": true, "premium_enabled": false}),
                false,
            ),
            (json!({"premium_enabled": true}), false),
        ] {
            assert_eq!(premium_features_visible(&features), expected);
        }
    }
}
