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
        if !self.notification_content_allowed(channel) {
            return none;
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
        let mentioned = self.message_mentions_current_user(message, !member);
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
    pub fn message_mentions_current_user(&self, message: &Message, ignore_everyone: bool) -> bool {
        let Some(me) = self.current_user.as_ref() else {
            return false;
        };
        let Some(channel) = self.channels.get(&message.channel_id) else {
            return false;
        };
        let settings = self.guild_notification_settings(channel.guild_id);
        let suppressed = |key: &str| settings.extra.get(key).and_then(Value::as_bool) == Some(true);
        message.mentions.iter().any(|user| user.id == me.id)
            || (!ignore_everyone && message.mention_everyone && !suppressed("suppress_everyone"))
            || (!suppressed("suppress_roles")
                && channel
                    .guild_id
                    .and_then(|guild| self.members.get(&(guild, me.id)))
                    .is_some_and(|member| {
                        message
                            .mention_roles
                            .iter()
                            .any(|id| member.roles.contains(id))
                    }))
    }
    pub fn notification_content_allowed(&self, channel: &crate::Channel) -> bool {
        let channel = if channel.kind.is_thread() {
            channel
                .parent_id
                .and_then(|id| self.channels.get(&id))
                .unwrap_or(channel)
        } else {
            channel
        };
        let parent = channel
            .parent_id
            .and_then(|id| self.channels.get(&id))
            .filter(|p| p.kind == crate::ChannelType::Category);
        let guild = channel.guild_id.and_then(|id| self.guilds.get(&id));
        let mature = channel
            .extra
            .get("nsfw_override")
            .and_then(Value::as_bool)
            .map(|v| (v, 2))
            .or_else(|| {
                parent
                    .and_then(|p| p.extra.get("nsfw_override"))
                    .and_then(Value::as_bool)
                    .map(|v| (v, 1))
            })
            .unwrap_or((
                guild
                    .and_then(|g| g.extra.get("nsfw"))
                    .and_then(Value::as_bool)
                    .unwrap_or(channel.nsfw),
                0,
            ));
        let warning = channel
            .extra
            .get("content_warning_level")
            .and_then(Value::as_u64)
            .filter(|v| *v != 0)
            .map(|v| (v, 2))
            .or_else(|| {
                parent
                    .and_then(|p| p.extra.get("content_warning_level"))
                    .and_then(Value::as_u64)
                    .filter(|v| *v != 0)
                    .map(|v| (v, 1))
            })
            .or_else(|| {
                guild
                    .and_then(|g| g.extra.get("content_warning_level"))
                    .and_then(Value::as_u64)
                    .filter(|v| *v != 0)
                    .map(|v| (v, 0))
            })
            .unwrap_or((0, 0));
        if !mature.0 && warning.0 != 1 {
            return true;
        }
        if mature.0
            && self
                .current_user
                .as_ref()
                .and_then(|u| u.extra.get("nsfw_allowed"))
                .and_then(Value::as_bool)
                != Some(true)
        {
            return false;
        }
        let Some(agreements) =
            decode_preferences(&self.user_settings).and_then(|prefs| prefs.agreements)
        else {
            return false;
        };
        let scope = if mature.0 && warning.0 == 1 {
            mature.1.max(warning.1)
        } else if mature.0 {
            mature.1
        } else {
            warning.1
        };
        match scope {
            2 => agreements.channels.contains(&channel.id.to_string()),
            1 => parent.is_some_and(|p| agreements.categories.contains(&p.id.to_string())),
            _ => guild.is_some_and(|g| agreements.guilds.contains(&g.id.to_string())),
        }
    }
}
#[derive(ProstMessage)]
struct Preferences {
    #[prost(message, optional, tag = "41")]
    mentions: Option<MentionPreferences>,
    #[prost(message, optional, tag = "62")]
    agreements: Option<Agreements>,
    #[prost(message, optional, tag = "100")]
    sound: Option<Sound>,
}
#[derive(ProstMessage)]
struct MentionPreferences {
    #[prost(bool, optional, tag = "1")]
    everyone: Option<bool>,
    #[prost(bool, optional, tag = "2")]
    roles: Option<bool>,
    #[prost(bool, optional, tag = "3")]
    guilds: Option<bool>,
}
pub fn recent_mention_filters(settings: &Value) -> (bool, bool, bool) {
    let prefs = decode_preferences(settings)
        .and_then(|p| p.mentions)
        .unwrap_or_default();
    (
        prefs.everyone.unwrap_or(true),
        prefs.roles.unwrap_or(true),
        prefs.guilds.unwrap_or(true),
    )
}
#[derive(ProstMessage)]
struct Agreements {
    #[prost(string, repeated, tag = "1")]
    channels: Vec<String>,
    #[prost(string, repeated, tag = "2")]
    guilds: Vec<String>,
    #[prost(string, repeated, tag = "3")]
    categories: Vec<String>,
}
fn decode_preferences(settings: &Value) -> Option<Preferences> {
    let encoded = settings
        .get("synced_preferences")
        .or_else(|| settings.get("syncedPreferences"))?
        .as_str()?;
    if encoded.len() > 700_000 {
        return None;
    }
    [
        &general_purpose::STANDARD,
        &general_purpose::STANDARD_NO_PAD,
        &general_purpose::URL_SAFE,
        &general_purpose::URL_SAFE_NO_PAD,
    ]
    .into_iter()
    .find_map(|engine| engine.decode(encoded).ok())
    .and_then(|bytes| Preferences::decode(bytes.as_slice()).ok())
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
    let sound = decode_preferences(settings).and_then(|prefs| prefs.sound);
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
    let percent = sound
        .as_ref()
        .and_then(|s| s.volumes.get(kind.key()))
        .copied()
        .or_else(|| sound.as_ref().and_then(|s| s.master))
        .filter(|v| v.is_finite())
        .unwrap_or(100.)
        .clamp(0., 200.);
    Some(((percent / 100. * 0.4).min(0.8) * 0.8) as f32)
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
            agreements: None,
            mentions: None,
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
    #[test]
    fn mature_content_requires_scoped_consent_and_allowed_account() {
        let mut state = ClientState::demo();
        let id = Snowflake(11);
        state
            .channels
            .get_mut(&id)
            .unwrap()
            .extra
            .insert("nsfw_override".into(), serde_json::json!(true));
        assert!(!state.notification_content_allowed(&state.channels[&id]));
        state
            .current_user
            .as_mut()
            .unwrap()
            .extra
            .insert("nsfw_allowed".into(), serde_json::json!(true));
        let prefs = Preferences {
            agreements: Some(Agreements {
                channels: vec![id.to_string()],
                ..Default::default()
            }),
            ..Default::default()
        };
        state.user_settings = serde_json::json!({"synced_preferences":general_purpose::STANDARD.encode(prefs.encode_to_vec())});
        assert!(state.notification_content_allowed(&state.channels[&id]));
        state
            .channels
            .get_mut(&id)
            .unwrap()
            .extra
            .insert("content_warning_level".into(), serde_json::json!(1));
        assert!(state.notification_content_allowed(&state.channels[&id]));
    }
    #[test]
    fn sound_override_replaces_master_with_source_headroom() {
        let prefs = Preferences {
            sound: Some(Sound {
                master: Some(200.),
                volumes: HashMap::from([("message".into(), 50.)]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let settings = serde_json::json!({"synced_preferences":general_purpose::STANDARD.encode(prefs.encode_to_vec())});
        assert!(
            (notification_sound_volume(&settings, MessageSound::Message).unwrap() - 0.16).abs()
                < 0.0001
        );
    }
    #[test]
    fn recent_mention_filters_use_synced_optional_defaults() {
        assert_eq!(recent_mention_filters(&Value::Null), (true, true, true));
        let prefs = Preferences {
            mentions: Some(MentionPreferences {
                everyone: Some(false),
                roles: None,
                guilds: Some(false),
            }),
            ..Default::default()
        };
        let settings = serde_json::json!({"synced_preferences":general_purpose::STANDARD.encode(prefs.encode_to_vec())});
        assert_eq!(recent_mention_filters(&settings), (false, true, false));
    }
}
