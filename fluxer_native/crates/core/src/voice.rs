use crate::{ClientState, Snowflake};
use std::{cmp::Ordering, collections::BTreeMap};

/// One gateway voice connection, independent of the local media SDK's roster.
/// Multiple connections for one account are retained; a sidebar can group by
/// `user_id`, while a call pane can render each connection individually.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceParticipant {
    pub user_id: Snowflake,
    pub connection_id: Option<String>,
    /// Exact VoiceParticipantIdentity.ts identity. Unknown connections have none.
    pub livekit_identity: Option<String>,
    pub display_name: String,
    pub avatar: Option<String>,
    pub guild_avatar: Option<String>,
    /// Member profile_flags AVATAR_UNSET suppresses both avatar hashes.
    pub avatar_unset: bool,
    /// Same account, not necessarily the connection running in this client.
    pub is_current_user: bool,
    pub connection_count: usize,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub mute: bool,
    pub deaf: bool,
    pub self_video: bool,
    pub self_stream: bool,
    pub suppress: bool,
}

impl ClientState {
    /// Project a channel's authoritative gateway roster, including this account
    /// and channels the local SDK has not joined. Name precedence follows
    /// NicknameUtils.getUntruncatedNickname; unresolved users retain their ID.
    ///
    /// Ordering is deterministic numeric, case-insensitive name order, followed
    /// by user/connection. The desktop's locale-specific ICU collation and
    /// per-mounted-list frozen-name/join snapshots belong to the UI layer and
    /// are not reproduced by this stateless projection.
    pub fn voice_participants(&self, channel_id: Snowflake) -> Vec<VoiceParticipant> {
        let channel = self.channels.get(&channel_id);
        let states: Vec<_> = self
            .voice_states
            .values()
            .filter(|state| state.channel_id == Some(channel_id))
            .collect();
        let mut counts = BTreeMap::new();
        for state in &states {
            *counts.entry(state.user_id).or_insert(0) += 1;
        }
        let mut participants: Vec<_> = states
            .into_iter()
            .map(|state| {
                let guild_id = channel.and_then(|c| c.guild_id).or(state.guild_id);
                let member = guild_id.and_then(|id| self.members.get(&(id, state.user_id)));
                let user = self
                    .current_user
                    .as_ref()
                    .filter(|me| me.id == state.user_id)
                    .or_else(|| self.users.get(&state.user_id))
                    .or_else(|| member.map(|m| &m.user))
                    .or_else(|| {
                        channel.and_then(|c| c.recipients.iter().find(|u| u.id == state.user_id))
                    })
                    .or_else(|| {
                        self.relationships
                            .get(&state.user_id)
                            .and_then(|r| r.user.as_ref())
                    });
                let base_name = user
                    .map(|u| u.display_name())
                    .filter(|name| !name.is_empty());
                let relationship_nick = self
                    .relationships
                    .get(&state.user_id)
                    .and_then(|r| r.nickname.as_deref())
                    .filter(|name| !name.is_empty());
                let context_nick = if guild_id.is_some() {
                    member.and_then(|m| m.nick.as_deref())
                } else {
                    channel
                        .and_then(|c| c.extra.get("nicks"))
                        .and_then(|nicks| nicks.get(state.user_id.to_string()))
                        .and_then(|nick| nick.as_str())
                }
                .filter(|name| !name.is_empty());
                let display_name = context_nick
                    .or(relationship_nick)
                    .or(base_name)
                    .map(str::to_owned)
                    .unwrap_or_else(|| state.user_id.to_string());
                let connection_id = state.connection_id.clone().filter(|id| !id.is_empty());
                let livekit_identity = connection_id
                    .as_ref()
                    .map(|connection| format!("user_{}_{connection}", state.user_id));
                VoiceParticipant {
                    user_id: state.user_id,
                    connection_id,
                    livekit_identity,
                    display_name,
                    avatar: user.and_then(|u| u.avatar.clone()),
                    guild_avatar: member.and_then(|m| m.avatar.clone()),
                    avatar_unset: member
                        .and_then(|m| m.profile_flags)
                        .is_some_and(|flags| flags & 1 != 0),
                    is_current_user: self
                        .current_user
                        .as_ref()
                        .is_some_and(|me| me.id == state.user_id),
                    connection_count: counts[&state.user_id],
                    self_mute: state.self_mute,
                    self_deaf: state.self_deaf,
                    mute: state.mute,
                    deaf: state.deaf,
                    self_video: state.self_video,
                    self_stream: state.self_stream,
                    suppress: state
                        .extra
                        .get("suppress")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                }
            })
            .collect();
        participants.sort_by(|left, right| {
            compare_names(&left.display_name, &right.display_name)
                .then_with(|| left.user_id.cmp(&right.user_id))
                .then_with(|| left.connection_id.cmp(&right.connection_id))
        });
        participants
    }
}

// Numeric comparison without parsing a bounded integer, so long digit runs do
// not overflow. This deliberately does not claim locale-specific ICU ordering.
fn compare_names(left: &str, right: &str) -> Ordering {
    let left = left.to_lowercase();
    let right = right.to_lowercase();
    let (mut left, mut right) = (left.as_str(), right.as_str());
    while !left.is_empty() && !right.is_empty() {
        let l = left.chars().next().expect("nonempty");
        let r = right.chars().next().expect("nonempty");
        if l.is_ascii_digit() && r.is_ascii_digit() {
            let ln = left.bytes().take_while(u8::is_ascii_digit).count();
            let rn = right.bytes().take_while(u8::is_ascii_digit).count();
            let lv = left[..ln].trim_start_matches('0');
            let rv = right[..rn].trim_start_matches('0');
            let ordering = lv.len().cmp(&rv.len()).then_with(|| lv.cmp(rv));
            if ordering != Ordering::Equal {
                return ordering;
            }
            left = &left[ln..];
            right = &right[rn..];
        } else {
            let ordering = l.cmp(&r);
            if ordering != Ordering::Equal {
                return ordering;
            }
            left = &left[l.len_utf8()..];
            right = &right[r.len_utf8()..];
        }
    }
    left.is_empty().cmp(&right.is_empty()).reverse()
}
