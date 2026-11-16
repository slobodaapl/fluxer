// SPDX-License-Identifier: AGPL-3.0-or-later
//! UI identity follows authoritative worker context, not channel IDs or READY
//! count. A repeated READY keeps drafts and overlays; a changed account/instance
//! fences programmatic composer updates until its next state arrives.
use fluxer_core::Snowflake;

#[derive(Default)]
pub(super) struct ConversationScope {
    instance: Option<String>,
    account: Option<Snowflake>,
    awaiting_state: bool,
    synchronized_content: Option<String>,
    pub generation: u64,
}
impl ConversationScope {
    pub fn context(&mut self, instance: &str, account: Option<Snowflake>) -> bool {
        let changed = self.instance.as_deref() != Some(instance)
            || account.is_some_and(|account| self.account != Some(account));
        self.instance = Some(instance.to_owned());
        if account.is_some() {
            self.account = account;
        }
        if changed {
            self.generation = self.generation.wrapping_add(1);
            self.awaiting_state = true;
        }
        changed
    }
    /// Returns (clear old private UI, reload authoritative composer draft).
    pub fn state(&mut self, account: Option<Snowflake>) -> (bool, bool) {
        let changed = self.account != account;
        let reload = changed || self.awaiting_state;
        self.account = account;
        self.awaiting_state = false;
        if changed {
            self.generation = self.generation.wrapping_add(1);
        }
        (changed, reload)
    }
    pub fn synchronize(&mut self, content: &str) {
        self.synchronized_content = Some(content.to_owned());
    }
    pub fn is_user_edit(&mut self, content: &str) -> bool {
        if self.awaiting_state {
            return false;
        }
        self.synchronized_content.take().as_deref() != Some(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ready_repetition_preserves_work_but_equal_ids_on_other_instances_reload() {
        let mut scope = ConversationScope::default();
        assert!(scope.context("https://a.test/api", Some(7.into())));
        assert!(!scope.is_user_edit("old draft"));
        assert_eq!(scope.state(Some(7.into())), (false, true));
        scope.synchronize("account A draft");
        assert!(!scope.is_user_edit("account A draft"));
        assert!(scope.is_user_edit("typing"));
        let generation = scope.generation;
        assert!(!scope.context("https://a.test/api", Some(7.into())));
        assert_eq!(scope.state(Some(7.into())), (false, false));
        assert_eq!(scope.generation, generation);
        assert!(scope.is_user_edit("kept after reconnect"));
        assert!(scope.context("https://b.test/api", Some(7.into())));
        scope.synchronize("");
        assert!(!scope.is_user_edit(""));
        assert_eq!(scope.state(Some(7.into())), (false, true));
        scope.synchronize("account B draft");
        assert!(!scope.is_user_edit("account B draft"));
        assert!(scope.is_user_edit("new edit"));
    }
    #[test]
    fn direct_account_change_and_logout_do_not_depend_on_channel_identity() {
        let mut scope = ConversationScope::default();
        scope.context("https://same.test/api", Some(7.into()));
        scope.state(Some(7.into()));
        assert_eq!(scope.state(Some(8.into())), (true, true));
        scope.synchronize("new user's saved draft");
        assert!(!scope.is_user_edit("new user's saved draft"));
        assert_eq!(scope.state(None), (true, true));
        assert_eq!(scope.state(None), (false, false));
    }
}
