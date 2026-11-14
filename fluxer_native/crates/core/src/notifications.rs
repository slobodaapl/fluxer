use crate::{ClientState, Snowflake};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MuteConfig {
    pub selected_time_window: Option<u64>,
    pub end_time: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelNotificationOverride {
    pub channel_id: Snowflake,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default = "inherit")]
    pub message_notifications: i8,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub mute_config: Option<MuteConfig>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
fn unset() -> i8 {
    -1
}
fn inherit() -> i8 {
    3
}
impl ChannelNotificationOverride {
    pub fn new(channel_id: Snowflake) -> Self {
        Self {
            channel_id,
            collapsed: false,
            message_notifications: 3,
            muted: false,
            mute_config: None,
            extra: BTreeMap::new(),
        }
    }
    pub fn is_muted(&self, now_ms: i64) -> bool {
        active(self.muted, self.mute_config.as_ref(), now_ms)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserGuildSettings {
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub mute_config: Option<MuteConfig>,
    #[serde(default = "unset")]
    pub message_notifications: i8,
    #[serde(default, deserialize_with = "overrides")]
    pub channel_overrides: BTreeMap<Snowflake, ChannelNotificationOverride>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
impl Default for UserGuildSettings {
    fn default() -> Self {
        Self {
            muted: false,
            mute_config: None,
            message_notifications: -1,
            channel_overrides: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}
impl UserGuildSettings {
    pub fn is_muted(&self, now_ms: i64) -> bool {
        active(self.muted, self.mute_config.as_ref(), now_ms)
    }
}
impl MuteConfig {
    pub fn for_seconds(seconds: u64, now_ms: i64) -> Self {
        let end = now_ms.saturating_add((seconds.saturating_mul(1000)).min(i64::MAX as u64) as i64);
        Self {
            selected_time_window: Some(seconds.saturating_mul(1000)),
            end_time: chrono::DateTime::from_timestamp_millis(end)
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        }
    }
}
fn overrides<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<Snowflake, ChannelNotificationOverride>, D::Error> {
    let value = Value::deserialize(d)?;
    let rows: Vec<Value> = match value {
        Value::Null => vec![],
        Value::Array(v) => v,
        Value::Object(v) => v.into_values().collect(),
        _ => return Err(serde::de::Error::custom("invalid channel overrides")),
    };
    let mut out = BTreeMap::new();
    for row in rows {
        let entry: ChannelNotificationOverride =
            serde_json::from_value(row).map_err(serde::de::Error::custom)?;
        out.insert(entry.channel_id, entry);
    }
    Ok(out)
}
fn active(muted: bool, config: Option<&MuteConfig>, now_ms: i64) -> bool {
    muted
        && config
            .and_then(|c| c.end_time.as_deref())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .is_none_or(|end| end.timestamp_millis() > now_ms)
}
#[derive(Clone, Debug)]
pub struct ChannelNotificationSettings {
    pub explicit_muted: bool,
    pub effective_muted: bool,
    pub message_notifications: i8,
    pub effective_message_notifications: i8,
}
impl ClientState {
    /// ReadStateEntry.computeMentionCountAfterAck scans buffered messages by timestamp.
    pub fn manual_ack_mention_count(&self, channel_id: Snowflake, previous: Snowflake) -> u32 {
        let Some(me) = self.current_user.as_ref() else {
            return 0;
        };
        let Some(channel) = self.channels.get(&channel_id) else {
            return 0;
        };
        let settings = self.guild_notification_settings(channel.guild_id);
        let suppressed = |key: &str| {
            settings
                .extra
                .get(key)
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        let roles = channel
            .guild_id
            .and_then(|guild| self.members.get(&(guild, me.id)))
            .map(|member| &member.roles);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let muted = settings.is_muted(now)
            || settings
                .channel_overrides
                .get(&channel_id)
                .is_some_and(|c| c.is_muted(now));
        let non_member_thread =
            channel.kind.is_thread() && channel.extra.get("member").is_none_or(Value::is_null);
        self.messages
            .get(&channel_id)
            .into_iter()
            .flat_map(|messages| messages.values())
            .filter(|message| {
                (message.id.0 >> 22) > (previous.0 >> 22)
                    && !self
                        .relationships
                        .get(&message.author.id)
                        .is_some_and(|r| r.is_blocked())
                    && (message.mentions.iter().any(|user| user.id == me.id)
                        || (!suppressed("suppress_everyone")
                            && message.mention_everyone
                            && !non_member_thread)
                        || (!suppressed("suppress_roles")
                            && roles.is_some_and(|roles| {
                                message
                                    .mention_roles
                                    .iter()
                                    .any(|role| roles.contains(role))
                            }))
                        || (channel.kind.is_private() && !muted))
            })
            .count()
            .min(u32::MAX as usize) as u32
    }
    pub fn guild_notification_settings(&self, guild_id: Option<Snowflake>) -> UserGuildSettings {
        self.user_guild_settings
            .get(&guild_id)
            .cloned()
            .unwrap_or_default()
    }
    pub fn channel_notification_settings(
        &self,
        guild_id: Option<Snowflake>,
        channel_id: Snowflake,
        now_ms: i64,
    ) -> ChannelNotificationSettings {
        let default_settings = UserGuildSettings::default();
        let settings = self
            .user_guild_settings
            .get(&guild_id)
            .unwrap_or(&default_settings);
        let direct = settings.channel_overrides.get(&channel_id);
        let parent = guild_id
            .and_then(|_| self.channels.get(&channel_id)?.parent_id)
            .filter(|id| {
                self.channels
                    .get(id)
                    .is_some_and(|c| c.kind == crate::ChannelType::Category)
            })
            .and_then(|id| settings.channel_overrides.get(&id));
        let explicit_muted = direct.is_some_and(|c| c.is_muted(now_ms));
        ChannelNotificationSettings {
            explicit_muted,
            effective_muted: explicit_muted
                || parent.is_some_and(|c| c.is_muted(now_ms))
                || active(settings.muted, settings.mute_config.as_ref(), now_ms),
            message_notifications: direct.map_or(3, |c| c.message_notifications),
            effective_message_notifications: direct
                .map(|c| c.message_notifications)
                .filter(|n| (0..=2).contains(n))
                .or_else(|| {
                    parent
                        .map(|c| c.message_notifications)
                        .filter(|n| (0..=2).contains(n))
                })
                .or_else(|| {
                    (0..=2)
                        .contains(&settings.message_notifications)
                        .then_some(settings.message_notifications)
                })
                .or_else(|| {
                    guild_id
                        .and_then(|id| self.guilds.get(&id))
                        .and_then(|g| g.extra.get("default_message_notifications"))
                        .and_then(Value::as_i64)
                        .and_then(|v| i8::try_from(v).ok())
                })
                .unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn gateway_merges_preserves_and_clears_overrides_and_resolves_expiry() {
        let mut state = ClientState::default();
        state.apply_gateway("READY", json!({"user":{"id":"1","username":"a"},"user_guild_settings":[{"guild_id":"10","muted":true,"mute_config":{"selected_time_window":900000,"end_time":"2026-01-01T00:15:00Z"},"suppress_roles":true,"channel_overrides":[{"channel_id":"11","muted":true,"message_notifications":2}]}]})).unwrap();
        let guild = Some(Snowflake(10));
        assert!(state.guild_notification_settings(guild).is_muted(0));
        assert!(!state.guild_notification_settings(guild).is_muted(i64::MAX));
        assert!(
            state
                .channel_notification_settings(guild, Snowflake(11), 0)
                .explicit_muted
        );
        state
            .apply_gateway(
                "USER_GUILD_SETTINGS_UPDATE",
                json!({"guild_id":"10","message_notifications":1}),
            )
            .unwrap();
        let settings = state.guild_notification_settings(guild);
        assert_eq!(settings.message_notifications, 1);
        assert_eq!(settings.extra["suppress_roles"], true);
        assert_eq!(settings.channel_overrides.len(), 1);
        state
            .apply_gateway(
                "USER_GUILD_SETTINGS_UPDATE",
                json!({"guild_id":"10","channel_overrides":null}),
            )
            .unwrap();
        assert!(
            state
                .guild_notification_settings(guild)
                .channel_overrides
                .is_empty()
        );
        assert_eq!(
            MuteConfig::for_seconds(900, 0).selected_time_window,
            Some(900_000)
        );
    }
    #[test]
    fn manual_mentions_follow_block_suppression_roles_and_private_mute() {
        let mut state = ClientState::demo();
        let channel = Snowflake(11);
        let me = state.current_user.as_ref().unwrap().id;
        let guild = state.channels[&channel].guild_id.unwrap();
        state.members.insert(
            (guild, me),
            crate::Member {
                user: state.current_user.clone().unwrap(),
                roles: vec![Snowflake(90)],
                ..Default::default()
            },
        );
        state.messages.remove(&channel);
        let previous = Snowflake(10 << 22);
        let author = crate::User {
            id: Snowflake(900),
            username: "author".into(),
            ..Default::default()
        };
        let mut role = crate::Message::text(Snowflake(11 << 22), channel, author.clone(), "role");
        role.mention_roles.push(Snowflake(90));
        let mut everyone =
            crate::Message::text(Snowflake(12 << 22), channel, author.clone(), "all");
        everyone.mention_everyone = true;
        let mut direct = crate::Message::text(Snowflake(13 << 22), channel, author, "direct");
        direct.mentions.push(state.current_user.clone().unwrap());
        for m in [role, everyone, direct] {
            state.insert_message(m);
        }
        assert_eq!(state.manual_ack_mention_count(channel, previous), 3);
        state.merge_user_guild_settings(json!({"guild_id":guild.to_string(),"suppress_roles":true,"suppress_everyone":true})).unwrap();
        assert_eq!(state.manual_ack_mention_count(channel, previous), 1);
        state.relationships.insert(
            Snowflake(900),
            crate::Relationship {
                id: Snowflake(900),
                kind: 2,
                ..Default::default()
            },
        );
        assert_eq!(state.manual_ack_mention_count(channel, previous), 0);
    }
}
