// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a notification click leads, the port of NotificationTargets.ts: the account and URL of
//! the last 1000 notifications shown. Showing an id again makes it the newest; the oldest goes
//! first once the store is full.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::js::utf16_length;

/// `MAX_NOTIFICATION_TARGETS`.
const CAPACITY: usize = 1000;
const ACCOUNT_KEY_MAX_LENGTH: usize = 2048;
const URL_MAX_LENGTH: usize = 8192;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotificationTarget {
    pub account_key: Option<String>,
    pub url: Option<String>,
}

/// Insertion-ordered, like the `Map` the TS used: `order` maps an insertion sequence number to
/// the id holding it.
#[derive(Default)]
pub struct NotificationTargets {
    entries: HashMap<String, (u64, NotificationTarget)>,
    order: BTreeMap<u64, String>,
    next_sequence: u64,
}

impl NotificationTargets {
    pub fn remember(&mut self, id: &str, target: NotificationTarget) {
        self.forget(id);
        self.next_sequence += 1;
        self.order.insert(self.next_sequence, id.to_owned());
        self.entries
            .insert(id.to_owned(), (self.next_sequence, target));
        while self.entries.len() > CAPACITY {
            let Some((_, oldest)) = self.order.pop_first() else {
                return;
            };
            self.entries.remove(&oldest);
        }
    }

    pub fn take(&mut self, id: &str) -> Option<NotificationTarget> {
        let (sequence, target) = self.entries.remove(id)?;
        self.order.remove(&sequence);
        Some(target)
    }

    pub fn forget(&mut self, id: &str) {
        self.take(id);
    }
}

/// A string of 1 to `max` UTF-16 code units.
fn read_bounded(value: Option<&Value>, max: usize) -> Option<String> {
    let value = value?.as_str()?;
    (1..=max)
        .contains(&utf16_length(value))
        .then(|| value.to_owned())
}

/// `readNotificationAccountKey`.
pub fn read_account_key(value: Option<&Value>) -> Option<String> {
    read_bounded(value, ACCOUNT_KEY_MAX_LENGTH)
}

/// `readNotificationUrl`.
pub fn read_url(value: Option<&Value>) -> Option<String> {
    read_bounded(value, URL_MAX_LENGTH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn target(url: &str) -> NotificationTarget {
        NotificationTarget {
            account_key: None,
            url: Some(url.to_owned()),
        }
    }

    #[test]
    fn take_returns_the_target_once() {
        let mut targets = NotificationTargets::default();
        targets.remember("a", target("/channels/1"));
        assert_eq!(targets.take("a"), Some(target("/channels/1")));
        assert_eq!(targets.take("a"), None);
        targets.remember("b", target("/channels/2"));
        targets.forget("b");
        assert_eq!(targets.take("b"), None);
    }

    #[test]
    fn the_oldest_target_is_evicted_past_a_thousand() {
        let mut targets = NotificationTargets::default();
        for index in 0..=1000 {
            targets.remember(&index.to_string(), target(&format!("/{index}")));
        }
        assert_eq!(targets.take("0"), None);
        assert_eq!(targets.take("1"), Some(target("/1")));
        assert_eq!(targets.take("1000"), Some(target("/1000")));
    }

    #[test]
    fn remembering_an_id_again_makes_it_the_newest() {
        let mut targets = NotificationTargets::default();
        for index in 0..1000 {
            targets.remember(&index.to_string(), target(&format!("/{index}")));
        }
        targets.remember("0", target("/again"));
        targets.remember("new", target("/new"));
        assert_eq!(targets.take("1"), None);
        assert_eq!(targets.take("0"), Some(target("/again")));
        assert_eq!(targets.take("2"), Some(target("/2")));
    }

    #[test]
    fn account_keys_and_urls_are_bounded_javascript_strings() {
        assert_eq!(read_account_key(Some(&json!("k"))), Some("k".to_owned()));
        assert_eq!(read_account_key(Some(&json!(""))), None);
        assert_eq!(read_account_key(Some(&json!(7))), None);
        assert_eq!(read_account_key(None), None);
        // Length counts UTF-16 code units: each emoji is two.
        let at_limit = "😀".repeat(1024);
        assert_eq!(read_account_key(Some(&json!(at_limit))), Some(at_limit));
        assert_eq!(read_account_key(Some(&json!("😀".repeat(1025)))), None);
        assert!(read_url(Some(&json!("u".repeat(8192)))).is_some());
        assert_eq!(read_url(Some(&json!("u".repeat(8193)))), None);
    }
}
