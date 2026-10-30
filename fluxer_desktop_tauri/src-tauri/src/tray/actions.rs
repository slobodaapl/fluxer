// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tray actions for the renderer (`tray-action`) and the queue that holds them while its bridge
//! is not ready: `queueTrayAction` and `flushPendingTrayActions` of DesktopTray.ts. A queued
//! status change replaces any earlier one, and past [`MAX_PENDING`] the oldest action is
//! dropped.
//!
//! Electron's union also had `check-for-updates` (never sent by its main process) and
//! `switch-account` (account submenu only); this shell sends neither. `open-account-switcher`
//! comes from the launch task of that name, not from the tray menu, which has no account
//! submenu.

use std::collections::VecDeque;

use serde::Serialize;

use super::state::PresenceStatus;

/// `MAX_PENDING_TRAY_ACTIONS`.
pub const MAX_PENDING: usize = 16;

/// `TrayActionPayload`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum TrayAction {
    SetStatus { status: PresenceStatus },
    ToggleMute,
    ToggleDeafen,
    DisconnectVoice,
    OpenAccountSwitcher,
}

#[derive(Debug, Default)]
pub struct PendingActions(VecDeque<TrayAction>);

impl PendingActions {
    /// Queues `action`; the action dropped to stay within [`MAX_PENDING`], if any.
    pub fn push(&mut self, action: TrayAction) -> Option<TrayAction> {
        if matches!(action, TrayAction::SetStatus { .. }) {
            self.0
                .retain(|queued| !matches!(queued, TrayAction::SetStatus { .. }));
        }
        self.0.push_back(action);
        (self.0.len() > MAX_PENDING)
            .then(|| self.0.pop_front())
            .flatten()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn take(&mut self) -> VecDeque<TrayAction> {
        std::mem::take(&mut self.0)
    }

    /// Puts undelivered actions back ahead of any queued since they were taken.
    pub fn restore(&mut self, mut undelivered: VecDeque<TrayAction>) {
        undelivered.append(&mut self.0);
        self.0 = undelivered;
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// Delivers `actions` in order until one fails; that one and the rest are returned.
pub fn deliver(
    mut actions: VecDeque<TrayAction>,
    mut send: impl FnMut(&TrayAction) -> bool,
) -> VecDeque<TrayAction> {
    while let Some(action) = actions.front() {
        if !send(action) {
            break;
        }
        actions.pop_front();
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status(status: PresenceStatus) -> TrayAction {
        TrayAction::SetStatus { status }
    }

    #[test]
    fn actions_serialise_as_the_renderer_reads_them() {
        assert_eq!(
            serde_json::to_value(status(PresenceStatus::Dnd)).unwrap(),
            json!({"action": "set-status", "status": "dnd"})
        );
        assert_eq!(
            serde_json::to_value(TrayAction::DisconnectVoice).unwrap(),
            json!({"action": "disconnect-voice"})
        );
        assert_eq!(
            serde_json::to_value(TrayAction::ToggleDeafen).unwrap(),
            json!({"action": "toggle-deafen"})
        );
        assert_eq!(
            serde_json::to_value(TrayAction::OpenAccountSwitcher).unwrap(),
            json!({"action": "open-account-switcher"})
        );
    }

    #[test]
    fn a_new_status_replaces_the_queued_one_and_keeps_the_order_of_the_rest() {
        let mut pending = PendingActions::default();
        pending.push(status(PresenceStatus::Idle));
        pending.push(TrayAction::ToggleMute);
        pending.push(status(PresenceStatus::Dnd));
        pending.push(TrayAction::ToggleMute);
        assert_eq!(
            Vec::from(pending.take()),
            [
                TrayAction::ToggleMute,
                status(PresenceStatus::Dnd),
                TrayAction::ToggleMute
            ]
        );
    }

    #[test]
    fn the_queue_drops_its_oldest_action_past_the_cap() {
        let mut pending = PendingActions::default();
        pending.push(TrayAction::ToggleDeafen);
        for _ in 1..MAX_PENDING {
            assert_eq!(pending.push(TrayAction::ToggleMute), None);
        }
        assert_eq!(
            pending.push(TrayAction::DisconnectVoice),
            Some(TrayAction::ToggleDeafen)
        );
        let queued = pending.take();
        assert_eq!(queued.len(), MAX_PENDING);
        assert_eq!(queued.back(), Some(&TrayAction::DisconnectVoice));
    }

    #[test]
    fn delivery_stops_at_the_first_failure_and_requeues_in_order() {
        let mut pending = PendingActions::default();
        for action in [TrayAction::ToggleMute, TrayAction::ToggleDeafen, TrayAction::DisconnectVoice] {
            pending.push(action);
        }
        let mut sent = Vec::new();
        let undelivered = deliver(pending.take(), |action| {
            if *action == TrayAction::ToggleDeafen {
                return false;
            }
            sent.push(action.clone());
            true
        });
        assert_eq!(sent, [TrayAction::ToggleMute]);
        // An action queued meanwhile goes after the requeued ones.
        pending.push(status(PresenceStatus::Online));
        pending.restore(undelivered);
        assert_eq!(
            Vec::from(pending.take()),
            [
                TrayAction::ToggleDeafen,
                TrayAction::DisconnectVoice,
                status(PresenceStatus::Online)
            ]
        );
    }
}
