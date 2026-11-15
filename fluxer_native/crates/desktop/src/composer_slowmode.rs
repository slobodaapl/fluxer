// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use fluxer_core::Permissions;
#[derive(Default)]
pub(super) struct Slowmode {
    sent: BTreeMap<Snowflake, std::time::Instant>,
    explicit: BTreeMap<Snowflake, std::time::Instant>,
    shown: u64,
    pub server_bypass: BTreeMap<Snowflake, bool>,
}
impl Slowmode {
    pub fn sent(&mut self, channel: Snowflake) {
        self.sent.insert(channel, std::time::Instant::now());
    }
    pub fn retry(&mut self, channel: Snowflake, duration: Duration) {
        if duration.is_zero() {
            self.explicit.remove(&channel);
            self.sent.remove(&channel);
        } else {
            self.explicit.insert(
                channel,
                std::time::Instant::now() + duration.min(Duration::from_secs(21600)),
            );
        }
    }
    fn remaining(&self, channel: Snowflake, seconds: u32, now: std::time::Instant) -> u64 {
        let local = self
            .sent
            .get(&channel)
            .map(|sent| {
                (*sent + Duration::from_secs(seconds as u64)).saturating_duration_since(now)
            })
            .unwrap_or_default();
        let explicit = self
            .explicit
            .get(&channel)
            .map(|until| until.saturating_duration_since(now))
            .unwrap_or_default();
        let duration = local.max(explicit);
        duration.as_secs() + u64::from(duration.subsec_nanos() > 0)
    }
}
impl Fluxer {
    pub(super) fn slowmode_remaining(&self) -> u64 {
        if self.editing.is_some() {
            return 0;
        }
        let Some(channel) = self
            .state
            .selected_channel
            .and_then(|id| self.state.channels.get(&id))
        else {
            return 0;
        };
        if channel.guild_id.is_none()
            || channel.rate_limit_per_user == 0
            || channel.rate_limit_per_user > 21600
        {
            return 0;
        }
        if self.slowmode.server_bypass.get(&channel.id) == Some(&true) {
            return 0;
        }
        if self.state.current_user.as_ref().is_some_and(|user| {
            self.state
                .permissions_for(channel.id, user.id)
                .contains(Permissions::BYPASS_SLOWMODE)
        }) {
            return 0;
        }
        self.slowmode.remaining(
            channel.id,
            channel.rate_limit_per_user,
            std::time::Instant::now(),
        )
    }
    pub(super) fn tick_slowmode(&mut self, cx: &mut Context<Self>) {
        let remaining = self.slowmode_remaining();
        if remaining != self.slowmode.shown {
            self.slowmode.shown = remaining;
            cx.notify();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::{Duration, Slowmode, Snowflake};
    #[test]
    fn cooldown_ceil_and_authoritative_retry() {
        let now = std::time::Instant::now();
        let id = Snowflake(1);
        let mut state = Slowmode::default();
        state.sent.insert(id, now);
        state.explicit.insert(id, now + Duration::from_secs(8));
        assert_eq!(state.remaining(id, 5, now + Duration::from_millis(250)), 8);
        assert_eq!(state.remaining(id, 10, now + Duration::from_secs(1)), 9);
        assert_eq!(state.remaining(id, 5, now + Duration::from_secs(9)), 0);
        assert_eq!(state.remaining(Snowflake(2), 5, now), 0);
        state.retry(id, Duration::ZERO);
        assert_eq!(state.remaining(id, 5, now), 0);
    }
}
