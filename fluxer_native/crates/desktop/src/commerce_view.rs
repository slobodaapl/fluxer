// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::controls::quiet_button;
use fluxer_client::{CommerceData, CommerceView};
use fluxer_protocol::{
    BillingCycle, GiftDurationType, GiftInventoryEntry, PremiumStatus, SubscriptionProvider,
};

#[derive(Default)]
pub(super) struct CommerceSettings {
    pub visible: bool,
    request: Option<u64>,
    result: Option<Result<CommerceData, String>>,
    copied: Option<usize>,
}

impl CommerceSettings {
    pub fn clear(&mut self) {
        self.request = None;
        self.result = None;
        self.copied = None;
    }

    pub fn finish(&mut self, request_id: u64, result: Result<CommerceData, String>) {
        if self.request == Some(request_id) {
            self.request = None;
            self.result = Some(result);
        }
    }
}

impl Fluxer {
    pub(super) fn commerce_visible(&self) -> bool {
        self.commerce.visible
            && !self.demo
            && self
                .state
                .current_user
                .as_ref()
                .is_some_and(|user| !user.bot)
    }

    pub(super) fn load_commerce(&mut self, view: CommerceView, cx: &mut Context<Self>) {
        self.commerce.clear();
        if !self.commerce_visible() {
            return;
        }
        let request_id = fluxer_client::next_pending_message_id();
        let result = self
            .controller
            .as_ref()
            .ok_or_else(|| "The native client worker is unavailable".to_string())
            .and_then(|controller| controller.send(Command::FetchCommerce { request_id, view }));
        match result {
            Ok(()) => self.commerce.request = Some(request_id),
            Err(error) => self.commerce.result = Some(Err(error)),
        }
        cx.notify();
    }

    pub(super) fn commerce_settings(
        &self,
        view: CommerceView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = match view {
            CommerceView::Plutonium => "Plutonium",
            CommerceView::Gifts => "Gifts & codes",
        };
        let mut body = div()
            .id("commerce-settings-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_size(px(22.)).child(title));
        if !self.commerce_visible() {
            return body
                .child(muted("This section is unavailable for this account."))
                .into_any_element();
        }
        body = body.child(match (&self.commerce.result, self.commerce.request) {
            (_, Some(_)) => muted("Loading…").into_any_element(),
            (Some(Err(error)), _) => muted(error.clone()).into_any_element(),
            (Some(Ok(CommerceData::Plutonium(status))), _) if view == CommerceView::Plutonium => {
                premium_card(status)
            }
            (Some(Ok(CommerceData::Gifts(gifts))), _) if view == CommerceView::Gifts => {
                self.gift_rows(gifts, cx)
            }
            _ => muted("Select Refresh to load this section.").into_any_element(),
        });
        if self.commerce.request.is_none() {
            body = body.child(
                quiet_button("refresh-commerce", "Refresh")
                    .on_click(cx.listener(move |this, _, _, cx| this.load_commerce(view, cx))),
            );
        }
        body.into_any_element()
    }

    fn gift_rows(&self, gifts: &[GiftInventoryEntry], cx: &mut Context<Self>) -> AnyElement {
        if gifts.is_empty() {
            return muted("Your gift inventory is empty.").into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(gifts.iter().enumerate().map(|(index, gift)| {
                let mut row = card().child(gift_duration(gift));
                if let Some(date) = &gift.redeemed_at {
                    let recipient = gift.redeemed_by.as_ref().map(|user| {
                        user.global_name
                            .as_deref()
                            .filter(|name| !name.is_empty())
                            .unwrap_or(&user.username)
                    });
                    row = row.child(muted(match recipient {
                        Some(name) => format!("Redeemed by {name} · {}", date_label(date)),
                        None => format!("Redeemed · {}", date_label(date)),
                    }));
                } else {
                    row = row
                        .child(muted(format!(
                            "Available · Created {}",
                            date_label(&gift.created_at)
                        )))
                        .child(
                            quiet_button(
                                SharedString::from(format!("copy-gift-{index}")),
                                if self.commerce.copied == Some(index) {
                                    "Copied"
                                } else {
                                    "Copy code"
                                },
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if !this.commerce_visible() {
                                        return;
                                    }
                                    if let Some(Ok(CommerceData::Gifts(gifts))) =
                                        &this.commerce.result
                                        && let Some(gift) = gifts.get(index)
                                        && gift.redeemed_at.is_none()
                                    {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            gift.code.clone(),
                                        ));
                                        this.commerce.copied = Some(index);
                                        cx.notify();
                                    }
                                },
                            )),
                        );
                }
                row
            }))
            .into_any_element()
    }
}

fn card() -> Div {
    div()
        .p_4()
        .rounded_md()
        .bg(rgb(0x25222e))
        .flex()
        .flex_col()
        .gap_2()
}

fn date_label(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%b %-d, %Y")
                .to_string()
        })
        .unwrap_or_else(|_| value.to_owned())
}

fn premium_card(status: &PremiumStatus) -> AnyElement {
    let actual = &status.actual;
    let effective = &status.effective;
    let title = if actual.is_visionary {
        "Visionary lifetime access"
    } else if actual.has_active_paid_premium {
        "Active Plutonium"
    } else {
        "No active Plutonium entitlement"
    };
    let perks = if effective.premium_perks_disabled {
        "Perks temporarily disabled"
    } else if effective.is_premium {
        "Perks active"
    } else {
        "Perks inactive"
    };
    let billing = match actual.premium_billing_cycle {
        Some(BillingCycle::Monthly) => Some("Monthly"),
        Some(BillingCycle::Yearly) => Some("Yearly"),
        _ => None,
    };
    let provider = match status.subscription_provider {
        Some(SubscriptionProvider::Stripe) => Some("Stripe"),
        Some(SubscriptionProvider::AppStore) => Some("App Store"),
        Some(SubscriptionProvider::GooglePlay) => Some("Google Play"),
        _ => None,
    };
    let details = [billing, provider]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    card()
        .child(title)
        .child(muted(perks))
        .when(!details.is_empty(), |row| row.child(muted(details)))
        .when_some(actual.premium_lifetime_sequence, |row, sequence| {
            row.child(muted(format!("Visionary #{sequence}")))
        })
        .when_some(actual.premium_since.as_deref(), |row, date| {
            row.child(muted(format!("Since {}", date_label(date))))
        })
        .when_some(actual.premium_until.as_deref(), |row, date| {
            row.child(muted(format!("Entitlement end date {}", date_label(date))))
        })
        .when_some(actual.premium_grace_ends_at.as_deref(), |row, date| {
            row.child(muted(format!("Grace period ends {}", date_label(date))))
        })
        .when(actual.premium_will_cancel, |row| {
            row.child(muted(
                "Subscription ends at the close of the billing period",
            ))
        })
        .into_any_element()
}

fn gift_duration(gift: &GiftInventoryEntry) -> String {
    let duration = match gift.duration_type {
        GiftDurationType::Days => "day",
        GiftDurationType::Weeks => "week",
        GiftDurationType::Months => "month",
        GiftDurationType::Years => "year",
        GiftDurationType::Unknown => return "Plutonium gift".into(),
    };
    let plural = if gift.duration_quantity == 1 { "" } else { "s" };
    format!(
        "Plutonium gift · {} {duration}{plural}",
        gift.duration_quantity
    )
}
