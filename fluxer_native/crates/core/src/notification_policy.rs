// SPDX-License-Identifier: AGPL-3.0-or-later
//! Active-account Notification.ts gates. Only a live gateway dispatch may call this.
use crate::{ClientState, Message, PresenceStatus};
use base64::{Engine, engine::general_purpose};
use prost::Message as ProstMessage;
use serde_json::Value;
use std::collections::HashMap;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageSound {
    Message,
    DirectMessage,
    SameChannel,
}
impl MessageSound {
    pub fn key(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::DirectMessage => "direct-message",
            Self::SameChannel => "same-channel-message",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NotificationDecision {
    pub desktop: bool,
    pub sound: Option<MessageSound>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct NotificationContext {
    pub focused: bool,
    pub modal: bool,
    pub streamer: bool,
    pub now_ms: i64,
}
impl ClientState {
    pub fn live_message_notification(
        &self,
        message: &Message,
        context: NotificationContext,
    ) -> NotificationDecision {
        let none = NotificationDecision::default();
        let Some(me) = self.current_user.as_ref() else {
            return none;
        };
        let Some(channel) = self.channels.get(&message.channel_id) else {
            return none;
        };
        let configured = self.user_settings.get("status").and_then(Value::as_str);
        let expired = self
            .user_settings
            .get("status_resets_at")
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|time| time.timestamp_millis() <= context.now_ms);
        let status = if expired {
            self.user_settings
                .get("status_resets_to")
                .and_then(Value::as_str)
                .or(Some("online"))
        } else {
            configured
        };
        let dnd = status.map_or_else(
            || {
                self.presences
                    .get(&me.id)
                    .is_some_and(|p| p.status == PresenceStatus::Dnd)
            },
            |status| status == "dnd",
        );
        if context.streamer
            || message.author.id == me.id
            || self
                .relationships
                .get(&message.author.id)
                .is_some_and(|r| r.is_blocked())
            || dnd
        {
            return none;
        }
        let visible = context.focused && self.selected_channel == Some(channel.id);
        if visible && !context.modal {
            return NotificationDecision {
                desktop: false,
                sound: Some(MessageSound::SameChannel),
            };
        }
        if message.flags & (1 << 12) != 0 {
            return none;
        }
        let settings =
            self.channel_notification_settings(channel.guild_id, channel.id, context.now_ms);
        let mut level = settings.effective_message_notifications;
        let mut member = true;
        if channel.kind.is_thread() {
            let membership = channel.extra.get("member").filter(|v| !v.is_null());
            member = membership.is_some();
            let parent = channel.parent_id.and_then(|id| self.channels.get(&id));
            if membership
                .and_then(|m| m.get("muted"))
                .and_then(Value::as_bool)
                == Some(true)
                || parent.is_some_and(|p| {
                    self.channel_notification_settings(p.guild_id, p.id, context.now_ms)
                        .effective_muted
                })
            {
                return none;
            }
            let flags = membership
                .and_then(|m| m.get("flags"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            level = if !member {
                if message.id == channel.id {
                    parent
                        .filter(|p| p.kind == crate::ChannelType::Forum)
                        .map_or(1, |p| {
                            self.channel_notification_settings(p.guild_id, p.id, context.now_ms)
                                .effective_message_notifications
                        })
                } else {
                    1
                }
            } else if flags & 2 != 0 {
                0
            } else if flags & 4 != 0 {
                1
            } else if flags & 8 != 0 {
                2
            } else {
                parent.map_or(1, |p| {
                    self.channel_notification_settings(p.guild_id, p.id, context.now_ms)
                        .effective_message_notifications
                })
            };
        } else if settings.effective_muted {
            return none;
        }
        if level == 2 {
            return none;
        }
        let guild = self.guild_notification_settings(channel.guild_id);
        let suppress = |key: &str| guild.extra.get(key).and_then(Value::as_bool) == Some(true);
        let roles = channel
            .guild_id
            .and_then(|id| self.members.get(&(id, me.id)))
            .map(|m| &m.roles);
        let mentioned = message.mentions.iter().any(|user| user.id == me.id)
            || (member && message.mention_everyone && !suppress("suppress_everyone"))
            || (!suppress("suppress_roles")
                && roles.is_some_and(|roles| {
                    message.mention_roles.iter().any(|id| roles.contains(id))
                }));
        if level != 0 && !channel.kind.is_private() && !mentioned {
            return none;
        }
        NotificationDecision {
            desktop: !visible,
            sound: Some(if channel.kind.is_private() {
                MessageSound::DirectMessage
            } else {
                MessageSound::Message
            }),
        }
    }
}
#[derive(ProstMessage)]
struct Preferences {
    #[prost(message, optional, tag = "100")]
    sound: Option<Sound>,
}
#[derive(ProstMessage)]
struct Sound {
    #[prost(bool, tag = "1")]
    all_disabled: bool,
    #[prost(double, optional, tag = "2")]
    master: Option<f64>,
    #[prost(map = "string,bool", tag = "3")]
    disabled: HashMap<String, bool>,
    #[prost(map = "string,double", tag = "4")]
    volumes: HashMap<String, f64>,
}
/// Sound.ts defaults and synced override, with source headroom and bounded gain.
pub fn notification_sound_volume(settings: &Value, kind: MessageSound) -> Option<f32> {
    let sound = settings
        .get("synced_preferences")
        .or_else(|| settings.get("syncedPreferences"))
        .and_then(Value::as_str)
        .filter(|s| s.len() < 700_000)
        .and_then(|encoded| {
            [
                &general_purpose::STANDARD,
                &general_purpose::STANDARD_NO_PAD,
                &general_purpose::URL_SAFE,
                &general_purpose::URL_SAFE_NO_PAD,
            ]
            .into_iter()
            .find_map(|engine| engine.decode(encoded).ok())
            .and_then(|bytes| Preferences::decode(bytes.as_slice()).ok())
            .and_then(|prefs| prefs.sound)
        });
    let default_disabled = kind == MessageSound::SameChannel;
    if sound.as_ref().is_some_and(|s| s.all_disabled)
        || sound
            .as_ref()
            .and_then(|s| s.disabled.get(kind.key()))
            .copied()
            .unwrap_or(default_disabled)
    {
        return None;
    }
    let master = sound
        .as_ref()
        .and_then(|s| s.master)
        .filter(|v| v.is_finite())
        .unwrap_or(100.)
        .clamp(0., 200.);
    let gain = sound
        .as_ref()
        .and_then(|s| s.volumes.get(kind.key()))
        .copied()
        .filter(|v| v.is_finite())
        .unwrap_or(100.)
        .clamp(0., 200.);
    Some(((master / 100. * gain / 100. * 0.4 * 0.8).min(0.8)) as f32)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientState, Message, Snowflake};
    #[test]
    fn focused_modal_mutes_mentions_and_identity_are_distinct() {
        let mut state = ClientState::demo();
        state.selected_channel = Some(Snowflake(11));
        let message = Message::text(
            Snowflake(999),
            Snowflake(11),
            state.users[&Snowflake(101)].clone(),
            "hello",
        );
        let mut context = NotificationContext {
            focused: true,
            ..Default::default()
        };
        assert_eq!(
            state.live_message_notification(&message, context).sound,
            Some(MessageSound::SameChannel)
        );
        assert!(!state.live_message_notification(&message, context).desktop);
        context.focused = false;
        assert!(state.live_message_notification(&message, context).desktop);
        state.user_settings = serde_json::json!({"status":"dnd"});
        assert_eq!(
            state.live_message_notification(&message, context),
            NotificationDecision::default()
        );
        state.user_settings = Value::Null;
        let settings = state
            .user_guild_settings
            .entry(Some(Snowflake(1)))
            .or_default();
        settings.message_notifications = 1;
        assert!(!state.live_message_notification(&message, context).desktop);
        let mut mentioned = message.clone();
        mentioned.mentions.push(state.current_user.clone().unwrap());
        assert!(state.live_message_notification(&mentioned, context).desktop);
        state
            .user_guild_settings
            .get_mut(&Some(Snowflake(1)))
            .unwrap()
            .muted = true;
        assert!(!state.live_message_notification(&mentioned, context).desktop);
    }
    #[test]
    fn synced_sound_disable_and_default_same_channel() {
        assert!(notification_sound_volume(&Value::Null, MessageSound::Message).is_some());
        assert!(notification_sound_volume(&Value::Null, MessageSound::SameChannel).is_none());
        let prefs = Preferences {
            sound: Some(Sound {
                all_disabled: true,
                ..Default::default()
            }),
        };
        let encoded = general_purpose::STANDARD.encode(prefs.encode_to_vec());
        assert!(
            notification_sound_volume(
                &serde_json::json!({"synced_preferences":encoded}),
                MessageSound::DirectMessage
            )
            .is_none()
        );
    }
}
