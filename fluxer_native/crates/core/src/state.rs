use crate::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("invalid {event} dispatch: {source}")]
    InvalidDispatch {
        event: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("READY account differs from the bound account; clear state before switching accounts")]
    AccountMismatch,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    Ignored,
}

/// Indexed rows received from the gateway's channel-specific lazy member list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemberList {
    pub list_id: String,
    pub identity: String,
    pub member_count: u64,
    pub online_count: u64,
    pub groups: Vec<Value>,
    pub rows: BTreeMap<u32, Value>,
}

/// Gateway private-call projection used to identify the account being rung.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrivateCall {
    pub message_id: Option<Snowflake>,
    pub ringing: BTreeSet<Snowflake>,
}

/// One state belongs to one authenticated account. A caller switching accounts
/// must replace it, avoiding accidental cross-account messages and drafts.
#[derive(Clone, Debug, Default)]
pub struct ClientState {
    pub current_user: Option<User>,
    pub users: BTreeMap<Snowflake, User>,
    pub guilds: BTreeMap<Snowflake, Guild>,
    /// ThreadGuilds activates a guild only when its full gateway projection
    /// includes the `threads` field, even when that array is empty.
    pub thread_guilds: BTreeSet<Snowflake>,
    pub channels: BTreeMap<Snowflake, Channel>,
    pub messages: BTreeMap<Snowflake, BTreeMap<Snowflake, Message>>,
    pub members: BTreeMap<(Snowflake, Snowflake), Member>,
    pub member_lists: BTreeMap<(Snowflake, Snowflake), MemberList>,
    pub read_states: BTreeMap<Snowflake, ReadState>,
    pub user_guild_settings: BTreeMap<Option<Snowflake>, UserGuildSettings>,
    pub relationships: BTreeMap<Snowflake, Relationship>,
    pub presences: BTreeMap<Snowflake, Presence>,
    /// Keyed by (user, connection) to retain simultaneous voice connections.
    pub voice_states: BTreeMap<(Snowflake, String), VoiceState>,
    pub private_calls: BTreeMap<Snowflake, PrivateCall>,
    /// Expiration time in Unix milliseconds, grouped by channel and user.
    pub typing: BTreeMap<Snowflake, BTreeMap<Snowflake, u64>>,
    pub selected_channel: Option<Snowflake>,
    pub drafts: BTreeMap<Snowflake, String>,
    pub settings: ClientSettings,
    /// Preserve the authoritative settings payload, including encoded synced
    /// privacy preferences used by attachment preupload policy.
    pub user_settings: Value,
    pub voice: VoiceControls,
    pub session_id: Option<String>,
    local_message_counter: u64,
}
impl ClientState {
    pub fn merge_user_guild_settings(&mut self, data: Value) -> Result<(), StateError> {
        let guild_id = match data.get("guild_id").and_then(Value::as_str) {
            Some("@me") | None => None,
            Some(id) => id.parse().ok(),
        };
        let existing = self.guild_notification_settings(guild_id);
        let mut merged = serde_json::to_value(existing).expect("settings serialize");
        if let (Some(target), Some(updates)) = (merged.as_object_mut(), data.as_object()) {
            for (key, value) in updates {
                target.insert(key.clone(), value.clone());
            }
        }
        let settings = decode("USER_GUILD_SETTINGS_UPDATE", merged)?;
        self.user_guild_settings.insert(guild_id, settings);
        Ok(())
    }
    /// REST/search/pin payloads use the same compact user references as gateway
    /// dispatches. Hydrate from the bound account cache before schema decoding.
    /// Decoding alone does not insert messages or advance read cursors.
    pub fn decode_api<T: DeserializeOwned>(&self, mut data: Value) -> Result<T, StateError> {
        hydrate_wire_users(
            &mut data,
            &self.users,
            self.current_user.as_ref().map(|user| user.id),
        );
        decode("REST", data)
    }
    /// Update a search/pin preview with gateway data without inserting it into
    /// the live timeline or changing unread counts in the account state.
    pub fn reduce_detached_message(
        &self,
        message: &Message,
        event: &str,
        mut data: Value,
    ) -> Result<Option<Message>, StateError> {
        hydrate_wire_users(
            &mut data,
            &self.users,
            self.current_user.as_ref().map(|user| user.id),
        );
        let mut detached = Self {
            current_user: self.current_user.clone(),
            ..Default::default()
        };
        detached.load_history(message.channel_id, vec![message.clone()]);
        detached.apply_gateway(event, data)?;
        Ok(detached
            .messages
            .get_mut(&message.channel_id)
            .and_then(|messages| messages.remove(&message.id)))
    }
    fn cache_user(&mut self, user: User) {
        if let Some(me) = self.current_user.as_ref().filter(|me| me.id == user.id) {
            self.users.insert(me.id, me.clone());
        } else {
            self.users.insert(user.id, user);
        }
    }
    fn hydrate_member_if_missing(&mut self, guild: Snowflake, member: Member) {
        if !self.members.contains_key(&(guild, member.user.id)) {
            self.cache_user(member.user.clone());
            self.members.insert((guild, member.user.id), member);
        }
    }
    pub fn channel_messages(
        &self,
        channel: Snowflake,
    ) -> impl DoubleEndedIterator<Item = &Message> {
        self.messages
            .get(&channel)
            .into_iter()
            .flat_map(|m| m.values())
    }
    pub fn guild_channels(&self, guild: Snowflake) -> Vec<&Channel> {
        let mut channels: Vec<_> = self
            .channels
            .values()
            .filter(|c| c.guild_id == Some(guild))
            .collect();
        channels.sort_by_key(|c| (c.position, c.id));
        channels
    }
    pub fn public_thread_type(&self, parent: Snowflake) -> Option<ChannelType> {
        let channel = self.channels.get(&parent)?;
        let guild = channel.guild_id?;
        let user = self.current_user.as_ref()?;
        let required = Permissions::VIEW_CHANNEL | Permissions::CREATE_PUBLIC_THREADS;
        if !self.thread_guilds.contains(&guild)
            || self.thread_creation_timed_out(parent, guild, user.id)
            || !self.permissions_for(parent, user.id).contains(required)
        {
            return None;
        }
        channel.kind.standalone_thread_type()
    }

    fn thread_creation_timed_out(
        &self,
        parent: Snowflake,
        guild: Snowflake,
        user: Snowflake,
    ) -> bool {
        if self.guilds.get(&guild).is_some_and(|g| g.owner_id == user)
            || self
                .permissions_for(parent, user)
                .contains(Permissions::ADMINISTRATOR)
        {
            return false;
        }
        self.members
            .get(&(guild, user))
            .and_then(|m| m.communication_disabled_until.as_deref())
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|until| until > chrono::Utc::now())
    }

    pub fn private_thread_allowed(&self, parent: Snowflake) -> bool {
        let Some(channel) = self.channels.get(&parent) else {
            return false;
        };
        let (Some(guild), Some(user)) = (channel.guild_id, self.current_user.as_ref()) else {
            return false;
        };
        channel.kind == ChannelType::Text
            && self.thread_guilds.contains(&guild)
            && !self.thread_creation_timed_out(parent, guild, user.id)
            && self
                .permissions_for(parent, user.id)
                .contains(Permissions::VIEW_CHANNEL | Permissions::CREATE_PRIVATE_THREADS)
    }

    pub fn message_thread_type(&self, parent: Snowflake) -> Option<ChannelType> {
        let kind = self.public_thread_type(parent)?;
        let user = self.current_user.as_ref()?;
        self.permissions_for(parent, user.id)
            .contains(Permissions::READ_MESSAGE_HISTORY)
            .then_some(kind)
    }

    pub fn select_channel(&mut self, id: Snowflake) -> bool {
        if !self.channels.contains_key(&id) {
            return false;
        }
        self.selected_channel = Some(id);
        self.mark_read(id);
        true
    }
    pub fn set_draft(&mut self, channel: Snowflake, draft: String) {
        if draft.is_empty() {
            self.drafts.remove(&channel);
        } else {
            self.drafts.insert(channel, draft);
        }
    }
    pub fn mark_read(&mut self, channel: Snowflake) {
        let last = self.channels.get(&channel).and_then(|c| c.last_message_id);
        let state = self.read_states.entry(channel).or_insert(ReadState {
            id: channel,
            ..ReadState::default()
        });
        state.last_message_id = state.last_message_id.max(last);
        state.mention_count = 0;
    }
    pub fn is_unread(&self, channel: Snowflake) -> bool {
        self.channels
            .get(&channel)
            .and_then(|c| c.last_message_id)
            .is_some_and(|last| {
                self.read_states
                    .get(&channel)
                    .and_then(|r| r.last_message_id)
                    .is_none_or(|read| last > read)
            })
    }
    pub fn has_unread_pins(&self, channel: Snowflake) -> bool {
        let latest = self
            .channels
            .get(&channel)
            .and_then(|channel| channel.last_pin_timestamp.as_deref());
        let acknowledged = self
            .read_states
            .get(&channel)
            .and_then(|state| state.last_pin_timestamp.as_deref());
        pin_timestamp(latest) > pin_timestamp(acknowledged)
    }
    /// ReadStates.applyPinAck uses equality (not monotonic comparison); invalid
    /// or absent acknowledgement timestamps fall back to the latest known pin.
    pub fn apply_pin_ack(&mut self, channel: Snowflake, timestamp: Option<&str>) -> bool {
        let latest = self
            .channels
            .get(&channel)
            .and_then(|channel| channel.last_pin_timestamp.clone());
        let candidate = if pin_timestamp(timestamp) != 0 {
            timestamp.map(str::to_owned)
        } else {
            latest
        };
        let read = self.read_states.entry(channel).or_insert(ReadState {
            id: channel,
            ..Default::default()
        });
        if pin_timestamp(read.last_pin_timestamp.as_deref()) == pin_timestamp(candidate.as_deref())
        {
            return false;
        }
        read.last_pin_timestamp = candidate;
        true
    }
    pub fn guild_permissions_for(&self, guild_id: Snowflake, user_id: Snowflake) -> Permissions {
        let Some(guild) = self
            .guilds
            .get(&guild_id)
            .filter(|guild| !guild.unavailable)
        else {
            return Permissions::NONE;
        };
        if guild.owner_id == user_id {
            return Permissions::ALL;
        }
        self.members
            .get(&(guild_id, user_id))
            .map_or(Permissions::NONE, |member| {
                resolve_permissions(guild, member, &[])
            })
    }

    pub fn permissions_for(&self, channel_id: Snowflake, user_id: Snowflake) -> Permissions {
        let Some(channel) = self.channels.get(&channel_id) else {
            return Permissions::NONE;
        };
        let Some(guild_id) = channel.guild_id else {
            let is_self = self.current_user.as_ref().is_some_and(|u| u.id == user_id);
            return if is_self && channel.kind.is_private() {
                Permissions::DEFAULT
            } else {
                Permissions::NONE
            };
        };
        let Some(guild) = self.guilds.get(&guild_id) else {
            return Permissions::NONE;
        };
        if guild.owner_id == user_id {
            return Permissions::ALL;
        }
        let Some(member) = self.members.get(&(guild_id, user_id)) else {
            return Permissions::NONE;
        };
        // Thread permissions inherit their parent. Private membership and write
        // lifecycle checks belong to the thread feature, not this base resolver.
        let overwrites = if channel.kind.is_thread() {
            channel
                .parent_id
                .and_then(|id| self.channels.get(&id))
                .map(|c| &c.permission_overwrites)
                .unwrap_or(&channel.permission_overwrites)
        } else {
            &channel.permission_overwrites
        };
        let permissions = resolve_permissions(guild, member, overwrites);
        if channel.kind.is_thread() && !permissions.contains(Permissions::ADMINISTRATOR) {
            let permissions = permissions.without(Permissions::SEND_MESSAGES);
            if permissions.contains(Permissions::SEND_MESSAGES_IN_THREADS) {
                permissions | Permissions::SEND_MESSAGES
            } else {
                permissions
            }
        } else {
            permissions
        }
    }
    /// MessageActionUtils/MessagePinService: PIN_MESSAGES is independent of
    /// SEND_MESSAGES; guild-wide SEND_MESSAGE can still disable pin operations.
    pub fn can_pin_messages(&self, channel_id: Snowflake) -> bool {
        self.channels
            .get(&channel_id)
            .zip(self.current_user.as_ref())
            .is_some_and(|(channel, user)| {
                let disabled = channel
                    .guild_id
                    .and_then(|id| self.guilds.get(&id))
                    .and_then(|guild| guild.extra.get("disabled_operations"))
                    .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
                    .unwrap_or(0);
                let system_dm = channel.kind == ChannelType::Dm
                    && channel
                        .recipients
                        .iter()
                        .any(|user| user.id == Snowflake(0));
                !system_dm
                    && disabled & (1 << 4) == 0
                    && (channel.kind.is_private()
                        || self
                            .permissions_for(channel_id, user.id)
                            .contains(Permissions::PIN_MESSAGES))
            })
    }
    pub fn insert_message(&mut self, message: Message) {
        self.cache_user(message.author.clone());
        for user in &message.mentions {
            self.cache_user(user.clone());
        }
        let channel_id = message.channel_id;
        let id = message.id;
        let already_seen = self
            .messages
            .get(&channel_id)
            .is_some_and(|m| m.contains_key(&id));
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            channel.last_message_id = channel.last_message_id.max(Some(id));
        }
        if !already_seen
            && self.read_state_mentions(&message, chrono::Utc::now().timestamp_millis())
        {
            let read = self.read_states.entry(channel_id).or_insert(ReadState {
                id: channel_id,
                ..ReadState::default()
            });
            if read.last_message_id.is_none_or(|read| id > read) {
                read.mention_count = read.mention_count.saturating_add(1);
            }
        }
        self.messages
            .entry(channel_id)
            .or_default()
            .insert(id, message);
    }
    /// Merge REST history without counting historical mentions as notifications.
    pub fn load_history(&mut self, channel_id: Snowflake, messages: Vec<Message>) {
        for message in messages.into_iter().filter(|m| m.channel_id == channel_id) {
            self.cache_user(message.author.clone());
            self.messages
                .entry(channel_id)
                .or_default()
                .insert(message.id, message);
        }
    }
    /// Local/demo edit. Live clients should call this after REST success or use
    /// gateway confirmation. Timestamp is supplied by the server on dispatch.
    pub fn edit_message(&mut self, channel: Snowflake, id: Snowflake, content: String) -> bool {
        let Some(message) = self.messages.get_mut(&channel).and_then(|m| m.get_mut(&id)) else {
            return false;
        };
        message.content = content;
        true
    }
    pub fn delete_message(&mut self, channel: Snowflake, id: Snowflake) -> bool {
        self.messages
            .get_mut(&channel)
            .and_then(|m| m.remove(&id))
            .is_some()
    }
    pub fn toggle_reaction(&mut self, channel: Snowflake, id: Snowflake, emoji: Emoji) -> bool {
        let Some(message) = self.messages.get_mut(&channel).and_then(|m| m.get_mut(&id)) else {
            return false;
        };
        if let Some(index) = message
            .reactions
            .iter()
            .position(|r| r.emoji.same_as(&emoji))
        {
            let reaction = &mut message.reactions[index];
            if reaction.me {
                reaction.me = false;
                reaction.count = reaction.count.saturating_sub(1);
            } else {
                reaction.me = true;
                reaction.count = reaction.count.saturating_add(1);
            }
            if reaction.count == 0 {
                message.reactions.remove(index);
            }
        } else {
            message.reactions.push(Reaction {
                emoji,
                count: 1,
                me: true,
            });
        }
        true
    }
    /// Deterministic IDs for the explicitly offline demo, never for REST sends.
    pub fn next_local_message_id(&mut self) -> Snowflake {
        let highest = self
            .messages
            .values()
            .filter_map(|m| m.last_key_value().map(|(id, _)| id.0))
            .max()
            .unwrap_or(0);
        self.local_message_counter = self.local_message_counter.max(highest).saturating_add(1);
        self.local_message_counter.into()
    }
    pub fn expire_typing(&mut self, now_ms: u64) {
        self.typing.retain(|_, users| {
            users.retain(|_, expiry| *expiry > now_ms);
            !users.is_empty()
        });
    }
    fn insert_guild(&mut self, mut guild: Guild) {
        for mut channel in std::mem::take(&mut guild.channels)
            .into_iter()
            .chain(std::mem::take(&mut guild.threads))
        {
            channel.guild_id = Some(guild.id);
            self.channels.insert(channel.id, channel);
        }
        for member in std::mem::take(&mut guild.members) {
            self.cache_user(member.user.clone());
            self.members.insert((guild.id, member.user.id), member);
        }
        for mut state in std::mem::take(&mut guild.voice_states) {
            state.guild_id = Some(guild.id);
            self.insert_voice_state(state);
        }
        for presence in std::mem::take(&mut guild.presences) {
            self.presences.insert(presence.user.id, presence);
        }
        self.guilds.insert(guild.id, guild);
    }
    fn insert_voice_state(&mut self, voice: VoiceState) {
        let connection = voice
            .connection_id
            .clone()
            .or_else(|| voice.session_id.clone())
            .unwrap_or_default();
        let key = (voice.user_id, connection);
        if voice.channel_id.is_some() {
            self.voice_states.insert(key, voice);
        } else if key.1.is_empty() {
            self.voice_states
                .retain(|(user, _), _| *user != voice.user_id);
        } else {
            self.voice_states.remove(&key);
        }
    }
    fn remove_channel(&mut self, id: Snowflake) {
        self.member_lists.retain(|(_, channel), _| *channel != id);
        self.channels.remove(&id);
        self.messages.remove(&id);
        self.drafts.remove(&id);
        self.read_states.remove(&id);
        self.typing.remove(&id);
        self.voice_states.retain(|_, v| v.channel_id != Some(id));
        if self.selected_channel == Some(id) {
            self.selected_channel = None;
        }
        if self.voice.channel_id == Some(id) {
            self.voice.disconnect();
        }
    }
    /// Apply a JSON gateway dispatch. The gateway adapter must deduplicate by
    /// dispatch sequence before calling this method. Unknown events are ignored.
    /// Malformed payloads are validated before mutating their target objects.
    pub fn apply_gateway(
        &mut self,
        event: &str,
        mut data: Value,
    ) -> Result<ApplyOutcome, StateError> {
        // Gateway READY deliberately deduplicates member users into `users`.
        // See session_ready_collect.erl:198-226 and GuildMember.ts:40-44.
        // Resolve real cached users before decoding strict REST-shaped models;
        // unresolved references remain errors rather than fabricated profiles.
        if event == "READY" {
            let self_user: User = decode(event, data.get("user").cloned().unwrap_or(Value::Null))?;
            let mut users = BTreeMap::new();
            if let Some(values) = data.get("users").and_then(Value::as_array) {
                for value in values {
                    let user: User = decode(event, value.clone())?;
                    users.insert(user.id, user);
                }
            }
            users.insert(self_user.id, self_user.clone());
            hydrate_wire_users(&mut data, &users, Some(self_user.id));
        } else {
            hydrate_wire_users(
                &mut data,
                &self.users,
                self.current_user.as_ref().map(|u| u.id),
            );
        }
        match event {
            "USER_GUILD_SETTINGS_UPDATE" => {
                self.merge_user_guild_settings(data)?;
            }
            "READY" => {
                #[derive(Deserialize)]
                struct Ready {
                    user: User,
                    #[serde(default)]
                    guilds: Vec<Guild>,
                    #[serde(default)]
                    private_channels: Vec<Channel>,
                    #[serde(default)]
                    users: Vec<User>,
                    #[serde(default)]
                    read_states: Vec<ReadState>,
                    #[serde(default)]
                    user_guild_settings: Vec<Value>,
                    #[serde(default)]
                    relationships: Vec<Relationship>,
                    #[serde(default)]
                    presences: Vec<Presence>,
                    #[serde(default)]
                    session_id: Option<String>,
                    #[serde(default)]
                    user_settings: Value,
                }
                if let Some(guilds) = data.get_mut("guilds").and_then(Value::as_array_mut) {
                    for guild in guilds {
                        *guild = normalize_guild(std::mem::take(guild));
                    }
                }
                let thread_guilds = data
                    .get("guilds")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|guild| {
                        guild.get("unavailable").and_then(Value::as_bool) != Some(true)
                            && guild.get("threads").is_some()
                    })
                    .filter_map(|guild| guild.get("id")?.as_str()?.parse::<Snowflake>().ok())
                    .collect();
                let ready: Ready = decode(event, data)?;
                if self
                    .current_user
                    .as_ref()
                    .is_some_and(|u| u.id != ready.user.id)
                {
                    return Err(StateError::AccountMismatch);
                }
                let mut next = Self {
                    current_user: Some(ready.user.clone()),
                    session_id: ready.session_id,
                    settings: self.settings.clone(),
                    drafts: self.drafts.clone(),
                    user_settings: ready.user_settings,
                    thread_guilds,
                    ..Self::default()
                };
                next.users.insert(ready.user.id, ready.user);
                for user in ready.users {
                    if next.current_user.as_ref().is_none_or(|me| me.id != user.id) {
                        next.users.insert(user.id, user);
                    }
                }
                for guild in ready.guilds {
                    next.insert_guild(guild);
                }
                for channel in ready.private_channels {
                    next.channels.insert(channel.id, channel);
                }
                for settings in ready.user_guild_settings {
                    next.merge_user_guild_settings(settings)?;
                }
                for read in ready.read_states {
                    next.read_states.insert(read.id, read);
                }
                for mut relationship in ready.relationships {
                    if relationship.user.is_none() {
                        relationship.user = next.users.get(&relationship.id).cloned();
                    }
                    next.relationships.insert(relationship.id, relationship);
                }
                for presence in ready.presences {
                    next.presences.insert(presence.user.id, presence);
                }
                next.selected_channel = self
                    .selected_channel
                    .filter(|id| next.channels.contains_key(id));
                *self = next;
            }
            "RESUMED" => {}
            "MESSAGE_CREATE" => self.insert_message(decode(event, data)?),
            "MESSAGE_UPDATE" => {
                let reference: MessageId = decode(event, data.clone())?;
                let Some(message) = self
                    .messages
                    .get(&reference.channel_id)
                    .and_then(|m| m.get(&reference.id))
                else {
                    return Ok(ApplyOutcome::Ignored);
                };
                let updated: Message = patch(event, message, data)?;
                self.messages
                    .get_mut(&reference.channel_id)
                    .unwrap()
                    .insert(reference.id, updated);
            }
            "MESSAGE_DELETE" => {
                let m: MessageId = decode(event, data)?;
                self.delete_message(m.channel_id, m.id);
            }
            "MESSAGE_DELETE_BULK" => {
                #[derive(Deserialize)]
                struct Bulk {
                    channel_id: Snowflake,
                    ids: Vec<Snowflake>,
                }
                let bulk: Bulk = decode(event, data)?;
                for id in bulk.ids {
                    self.delete_message(bulk.channel_id, id);
                }
            }
            "MESSAGE_REACTION_ADD" | "MESSAGE_REACTION_REMOVE" => {
                #[derive(Deserialize)]
                struct ReactionEvent {
                    channel_id: Snowflake,
                    message_id: Snowflake,
                    user_id: Snowflake,
                    emoji: Emoji,
                    #[serde(default)]
                    guild_id: Option<Snowflake>,
                    #[serde(default)]
                    member: Option<Member>,
                }
                let change: ReactionEvent = decode(event, data)?;
                if let Some((guild, member)) = change.guild_id.zip(change.member) {
                    self.hydrate_member_if_missing(guild, member);
                }
                let me = self
                    .current_user
                    .as_ref()
                    .is_some_and(|u| u.id == change.user_id);
                let Some(message) = self
                    .messages
                    .get_mut(&change.channel_id)
                    .and_then(|m| m.get_mut(&change.message_id))
                else {
                    return Ok(ApplyOutcome::Ignored);
                };
                let index = message
                    .reactions
                    .iter()
                    .position(|r| r.emoji.same_as(&change.emoji));
                if event == "MESSAGE_REACTION_ADD" {
                    if let Some(index) = index {
                        let reaction = &mut message.reactions[index];
                        reaction.count = reaction.count.saturating_add(1);
                        reaction.me |= me;
                    } else {
                        message.reactions.push(Reaction {
                            emoji: change.emoji,
                            count: 1,
                            me,
                        });
                    }
                } else if let Some(index) = index {
                    let reaction = &mut message.reactions[index];
                    reaction.count = reaction.count.saturating_sub(1);
                    if me {
                        reaction.me = false;
                    }
                    if reaction.count == 0 {
                        message.reactions.remove(index);
                    }
                }
            }
            "MESSAGE_REACTION_ADD_MANY" => {
                // session_dispatch_voice.erl:161-189 groups only additions.
                // Removes remain ordinary events or cancel an unflushed add.
                #[derive(Deserialize, Serialize)]
                struct Entry {
                    user_id: Snowflake,
                    emoji: Emoji,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    member: Option<Member>,
                }
                #[derive(Deserialize)]
                struct Batch {
                    channel_id: Snowflake,
                    message_id: Snowflake,
                    #[serde(default)]
                    guild_id: Option<Snowflake>,
                    reactions: Vec<Entry>,
                }
                let batch: Batch = decode(event, data)?;
                for reaction in batch.reactions {
                    let mut single =
                        serde_json::to_value(reaction).expect("reaction serialization cannot fail");
                    let fields = single.as_object_mut().expect("reaction is an object");
                    fields.insert(
                        "channel_id".into(),
                        serde_json::to_value(batch.channel_id).unwrap(),
                    );
                    fields.insert(
                        "message_id".into(),
                        serde_json::to_value(batch.message_id).unwrap(),
                    );
                    if let Some(guild) = batch.guild_id {
                        fields.insert("guild_id".into(), serde_json::to_value(guild).unwrap());
                    }
                    self.apply_gateway("MESSAGE_REACTION_ADD", single)?;
                }
            }
            "MESSAGE_REACTION_REMOVE_ALL" => {
                #[derive(Deserialize)]
                struct Target {
                    channel_id: Snowflake,
                    message_id: Snowflake,
                }
                let target: Target = decode(event, data)?;
                if let Some(m) = self
                    .messages
                    .get_mut(&target.channel_id)
                    .and_then(|m| m.get_mut(&target.message_id))
                {
                    m.reactions.clear();
                }
            }
            "MESSAGE_REACTION_REMOVE_EMOJI" => {
                #[derive(Deserialize)]
                struct Target {
                    channel_id: Snowflake,
                    message_id: Snowflake,
                    emoji: Emoji,
                }
                let target: Target = decode(event, data)?;
                if let Some(m) = self
                    .messages
                    .get_mut(&target.channel_id)
                    .and_then(|m| m.get_mut(&target.message_id))
                {
                    m.reactions.retain(|r| !r.emoji.same_as(&target.emoji));
                }
            }
            "THREAD_LIST_SYNC" => {
                #[derive(Deserialize)]
                struct ThreadList {
                    guild_id: Snowflake,
                    channel_ids: Option<BTreeSet<Snowflake>>,
                    threads: Vec<Channel>,
                    #[serde(default)]
                    members: Vec<Value>,
                }
                let list: ThreadList = decode(event, data)?;
                if !self.thread_guilds.contains(&list.guild_id) {
                    return Ok(ApplyOutcome::Ignored);
                }
                let scoped = |parent: Option<Snowflake>| {
                    parent.is_some_and(|parent| {
                        list.channel_ids
                            .as_ref()
                            .is_none_or(|ids| ids.contains(&parent))
                    })
                };
                let incoming: BTreeSet<_> = list
                    .threads
                    .iter()
                    .filter(|thread| thread.kind.is_thread() && scoped(thread.parent_id))
                    .map(|thread| thread.id)
                    .collect();
                let removed: Vec<_> = self
                    .channels
                    .values()
                    .filter(|thread| {
                        thread.guild_id == Some(list.guild_id)
                            && thread.kind.is_thread()
                            && scoped(thread.parent_id)
                            && !thread.thread_archived()
                            && !incoming.contains(&thread.id)
                    })
                    .map(|thread| thread.id)
                    .collect();
                for id in removed {
                    self.remove_channel(id);
                }
                let mut memberships: BTreeMap<_, _> = list
                    .members
                    .into_iter()
                    .filter_map(|member| {
                        let id = member.get("id")?.as_str()?.parse::<Snowflake>().ok()?;
                        Some((id, member))
                    })
                    .collect();
                for mut thread in list.threads {
                    if incoming.contains(&thread.id) {
                        thread.guild_id = Some(list.guild_id);
                        thread.extra.remove("member");
                        if let Some(member) = memberships.remove(&thread.id) {
                            thread.extra.insert("member".into(), member);
                        }
                        self.channels.insert(thread.id, thread);
                    }
                }
            }
            "THREAD_MEMBER_UPDATE" => {
                #[derive(Deserialize)]
                struct Membership {
                    id: Snowflake,
                    user_id: Snowflake,
                }
                let membership: Membership = decode(event, data.clone())?;
                if self.current_user.as_ref().map(|user| user.id) == Some(membership.user_id)
                    && let Some(thread) = self.channels.get_mut(&membership.id)
                    && thread.kind.is_thread()
                {
                    thread.extra.insert("member".into(), data);
                }
            }
            "THREAD_MEMBERS_UPDATE" => {
                #[derive(Deserialize)]
                struct Memberships {
                    id: Snowflake,
                    member_count: u64,
                    #[serde(default)]
                    added_members: Vec<Value>,
                    #[serde(default)]
                    removed_member_ids: BTreeSet<Snowflake>,
                }
                let memberships: Memberships = decode(event, data)?;
                let Some(user) = self.current_user.as_ref().map(|user| user.id) else {
                    return Ok(ApplyOutcome::Ignored);
                };
                let removed = memberships.removed_member_ids.contains(&user);
                let moderator = self
                    .permissions_for(memberships.id, user)
                    .contains(Permissions::MANAGE_THREADS);
                let Some(thread) = self
                    .channels
                    .get_mut(&memberships.id)
                    .filter(|channel| channel.kind.is_thread())
                else {
                    return Ok(ApplyOutcome::Ignored);
                };
                if removed && thread.kind == ChannelType::PrivateThread && !moderator {
                    self.remove_channel(memberships.id);
                    return Ok(ApplyOutcome::Applied);
                }
                thread
                    .extra
                    .insert("member_count".into(), memberships.member_count.into());
                if removed {
                    thread.extra.remove("member");
                } else if let Some(member) = memberships.added_members.into_iter().find(|member| {
                    member
                        .get("user_id")
                        .and_then(Value::as_str)
                        .and_then(|id| id.parse::<Snowflake>().ok())
                        == Some(user)
                }) {
                    thread.extra.insert("member".into(), member);
                }
            }
            "CHANNEL_CREATE" | "THREAD_CREATE" => {
                let channel: Channel = decode(event, data)?;
                self.channels.insert(channel.id, channel);
            }
            "CHANNEL_RECIPIENT_ADD" | "CHANNEL_RECIPIENT_REMOVE" => {
                #[derive(Deserialize)]
                struct RecipientEvent {
                    channel_id: Snowflake,
                    user: User,
                }
                let recipient: RecipientEvent = decode(event, data)?;
                let Some(channel) =
                    self.channels
                        .get_mut(&recipient.channel_id)
                        .filter(|channel| {
                            channel.kind == ChannelType::GroupDm && channel.guild_id.is_none()
                        })
                else {
                    return Ok(ApplyOutcome::Ignored);
                };
                if event == "CHANNEL_RECIPIENT_ADD" {
                    if let Some(existing) = channel
                        .recipients
                        .iter_mut()
                        .find(|user| user.id == recipient.user.id)
                    {
                        *existing = recipient.user.clone();
                    } else {
                        channel.recipients.push(recipient.user.clone());
                    }
                    self.cache_user(recipient.user);
                } else {
                    channel
                        .recipients
                        .retain(|user| user.id != recipient.user.id);
                }
            }
            "CHANNEL_UPDATE" | "THREAD_UPDATE" => {
                let target: Id = decode(event, data.clone())?;
                let Some(channel) = self.channels.get(&target.id) else {
                    return Ok(ApplyOutcome::Ignored);
                };
                let channel: Channel = patch(event, channel, data)?;
                self.channels.insert(channel.id, channel);
            }
            "CHANNEL_UPDATE_BULK" => {
                // ChannelUpdateBulk.ts merges partial channel rows by ID.
                #[derive(Deserialize)]
                struct Bulk {
                    channels: Vec<Value>,
                }
                let bulk: Bulk = decode(event, data)?;
                let mut channels = Vec::with_capacity(bulk.channels.len());
                for update in bulk.channels {
                    let target: Id = decode(event, update.clone())?;
                    let channel: Channel = match self.channels.get(&target.id) {
                        Some(channel) => patch(event, channel, update)?,
                        None => decode(event, update)?,
                    };
                    channels.push(channel);
                }
                for channel in channels {
                    self.channels.insert(channel.id, channel);
                }
            }
            "CHANNEL_DELETE" | "THREAD_DELETE" => {
                let target: Id = decode(event, data)?;
                if event == "CHANNEL_DELETE" {
                    let children: Vec<_> = self
                        .channels
                        .values()
                        .filter(|channel| {
                            channel.kind.is_thread() && channel.parent_id == Some(target.id)
                        })
                        .map(|channel| channel.id)
                        .collect();
                    for id in children {
                        self.remove_channel(id);
                    }
                }
                self.remove_channel(target.id);
            }
            "GUILD_CREATE" | "GUILD_SYNC" => {
                if event == "GUILD_CREATE"
                    && data.get("unavailable").and_then(Value::as_bool) != Some(true)
                {
                    let target: Id = decode(event, data.clone())?;
                    if data.get("threads").is_some() {
                        self.thread_guilds.insert(target.id);
                    } else {
                        self.thread_guilds.remove(&target.id);
                    }
                }
                let guild: Guild = decode(event, normalize_guild(data))?;
                self.insert_guild(guild);
            }
            "GUILD_UPDATE" => {
                let target: Id = decode(event, data.clone())?;
                let Some(guild) = self.guilds.get(&target.id) else {
                    return Ok(ApplyOutcome::Ignored);
                };
                let guild: Guild = patch(event, guild, normalize_guild(data))?;
                self.insert_guild(guild);
            }
            "GUILD_STICKERS_UPDATE" => {
                #[derive(Deserialize)]
                struct Update {
                    guild_id: Snowflake,
                    stickers: Vec<Value>,
                }
                let update: Update = decode(event, data)?;
                let Some(guild) = self.guilds.get_mut(&update.guild_id) else {
                    return Ok(ApplyOutcome::Ignored);
                };
                guild
                    .extra
                    .insert("stickers".into(), Value::Array(update.stickers));
            }
            "GUILD_DELETE" => {
                #[derive(Deserialize)]
                struct Deleted {
                    id: Snowflake,
                    #[serde(default)]
                    unavailable: bool,
                }
                let target: Deleted = decode(event, data)?;
                if target.unavailable {
                    if let Some(guild) = self.guilds.get_mut(&target.id) {
                        guild.unavailable = true;
                    }
                } else {
                    self.thread_guilds.remove(&target.id);
                    self.guilds.remove(&target.id);
                    self.members.retain(|(guild, _), _| *guild != target.id);
                    self.member_lists
                        .retain(|(guild, _), _| *guild != target.id);
                    let channels: Vec<_> = self
                        .channels
                        .values()
                        .filter(|c| c.guild_id == Some(target.id))
                        .map(|c| c.id)
                        .collect();
                    for channel in channels {
                        self.remove_channel(channel);
                    }
                }
            }
            "USER_UPDATE" => {
                let target: Id = decode(event, data.clone())?;
                let user: User = if let Some(user) = self.users.get(&target.id) {
                    patch(event, user, data)?
                } else {
                    decode(event, data)?
                };
                if self.current_user.as_ref().is_some_and(|u| u.id == user.id) {
                    self.current_user = Some(user.clone());
                }
                self.users.insert(user.id, user);
            }
            "USER_SETTINGS_UPDATE" => {
                let update: serde_json::Map<String, Value> = decode(event, data)?;
                let settings = self.user_settings.as_object_mut();
                if let Some(settings) = settings {
                    settings.extend(update);
                } else {
                    self.user_settings = Value::Object(update);
                }
            }
            "GUILD_MEMBER_ADD" | "GUILD_MEMBER_UPDATE" => {
                #[derive(Deserialize)]
                struct Target {
                    guild_id: Snowflake,
                    user: Id,
                }
                let target: Target = decode(event, data.clone())?;
                let member: Member =
                    if let Some(member) = self.members.get(&(target.guild_id, target.user.id)) {
                        patch(event, member, data)?
                    } else {
                        decode(event, data)?
                    };
                self.cache_user(member.user.clone());
                self.members
                    .insert((target.guild_id, member.user.id), member);
            }
            "GUILD_MEMBER_REMOVE" => {
                #[derive(Deserialize)]
                struct Target {
                    guild_id: Snowflake,
                    user: Id,
                }
                let target: Target = decode(event, data)?;
                self.members.remove(&(target.guild_id, target.user.id));
            }
            "GUILD_ROLE_CREATE" | "GUILD_ROLE_UPDATE" => {
                #[derive(Deserialize)]
                struct Target {
                    guild_id: Snowflake,
                    role: Role,
                }
                let target: Target = decode(event, data)?;
                if let Some(guild) = self.guilds.get_mut(&target.guild_id) {
                    guild.roles.retain(|role| role.id != target.role.id);
                    guild.roles.push(target.role);
                }
            }
            "GUILD_ROLE_DELETE" => {
                #[derive(Deserialize)]
                struct Target {
                    guild_id: Snowflake,
                    role_id: Snowflake,
                }
                let target: Target = decode(event, data)?;
                if let Some(guild) = self.guilds.get_mut(&target.guild_id) {
                    guild.roles.retain(|role| role.id != target.role_id);
                }
                for ((guild, _), member) in &mut self.members {
                    if *guild == target.guild_id {
                        member.roles.retain(|role| *role != target.role_id);
                    }
                }
            }
            "GUILD_ROLE_UPDATE_BULK" => {
                #[derive(Deserialize)]
                struct Bulk {
                    guild_id: Snowflake,
                    roles: Vec<Role>,
                }
                let bulk: Bulk = decode(event, data)?;
                if let Some(guild) = self.guilds.get_mut(&bulk.guild_id) {
                    for role in bulk.roles {
                        guild.roles.retain(|existing| existing.id != role.id);
                        guild.roles.push(role);
                    }
                }
            }
            "RELATIONSHIP_ADD" | "RELATIONSHIP_UPDATE" => {
                let relationship: Relationship = decode(event, data)?;
                if let Some(user) = &relationship.user {
                    self.cache_user(user.clone());
                }
                self.relationships.insert(relationship.id, relationship);
            }
            "RELATIONSHIP_REMOVE" => {
                let target: Id = decode(event, data)?;
                self.relationships.remove(&target.id);
            }
            "GUILD_MEMBERS_CHUNK" => {
                #[derive(Deserialize)]
                struct Chunk {
                    guild_id: Snowflake,
                    #[serde(default)]
                    members: Vec<Member>,
                    #[serde(default)]
                    presences: Vec<Presence>,
                }
                let chunk: Chunk = decode(event, data)?;
                if self.guilds.contains_key(&chunk.guild_id) {
                    for member in chunk.members {
                        self.cache_user(member.user.clone());
                        self.members
                            .insert((chunk.guild_id, member.user.id), member);
                    }
                    for presence in chunk.presences {
                        self.presences.insert(presence.user.id, presence);
                    }
                }
            }
            "GUILD_MEMBER_LIST_UPDATE" => {
                // Only retain server-supplied indexed rows. Message authors and
                // partial guild caches are never presented as the member list.
                let guild = data
                    .get("guild_id")
                    .and_then(Value::as_str)
                    .and_then(|v| v.parse().ok());
                let channel = data
                    .get("channel_id")
                    .and_then(Value::as_str)
                    .and_then(|v| v.parse().ok());
                if let (Some(guild), Some(channel), Some(id), Some(ops)) = (
                    guild,
                    channel,
                    data.get("id").and_then(Value::as_str),
                    data.get("ops").and_then(Value::as_array),
                ) {
                    let list = self.member_lists.entry((guild, channel)).or_default();
                    if list.list_id != id {
                        *list = MemberList {
                            list_id: id.to_owned(),
                            ..Default::default()
                        };
                    }
                    list.member_count = data
                        .get("member_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    list.groups = data
                        .get("groups")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    list.online_count = data
                        .get("online_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    for op in ops {
                        let Some(range) = op.get("range").and_then(Value::as_array) else {
                            continue;
                        };
                        let Some((start, end)) = range
                            .first()
                            .and_then(Value::as_u64)
                            .zip(range.get(1).and_then(Value::as_u64))
                        else {
                            continue;
                        };
                        let Some(items) = op.get("items").and_then(Value::as_array) else {
                            continue;
                        };
                        if op.get("op").and_then(Value::as_str) != Some("SYNC")
                            || end < start
                            || end > 100_000
                            || end - start > 99
                        {
                            continue;
                        }
                        list.rows
                            .retain(|index, _| (*index as u64) < start || (*index as u64) > end);
                        for (offset, item) in
                            items.iter().take((end - start + 1) as usize).enumerate()
                        {
                            list.rows.insert(start as u32 + offset as u32, item.clone());
                        }
                    }
                    let row_members: Vec<Value> = list
                        .rows
                        .values()
                        .filter_map(|row| row.get("member").cloned())
                        .collect();
                    for wire in row_members {
                        if let Ok(member) = self.decode_api::<Member>(wire) {
                            self.cache_user(member.user.clone());
                            self.members.insert((guild, member.user.id), member);
                        }
                    }
                }
            }
            "PRESENCE_UPDATE" => {
                let presence: Presence = decode(event, data)?;
                self.presences.insert(presence.user.id, presence);
            }
            "CALL_CREATE" | "CALL_UPDATE" => {
                #[derive(Deserialize)]
                struct CallDispatch {
                    channel_id: Snowflake,
                    #[serde(default)]
                    message_id: Option<Snowflake>,
                    #[serde(default)]
                    ringing: Option<Vec<Snowflake>>,
                }
                let call: CallDispatch = decode(event, data)?;
                let existing = self.private_calls.entry(call.channel_id).or_default();
                if existing.message_id.is_some()
                    && call.message_id.is_some()
                    && existing.message_id != call.message_id
                {
                    *existing = PrivateCall::default();
                }
                if let Some(id) = call.message_id {
                    existing.message_id = Some(id);
                }
                if let Some(ringing) = call.ringing {
                    existing.ringing = ringing.into_iter().collect();
                }
            }
            "CALL_DELETE" => {
                #[derive(Deserialize)]
                struct CallDelete {
                    channel_id: Snowflake,
                }
                let call: CallDelete = decode(event, data)?;
                self.private_calls.remove(&call.channel_id);
            }
            "VOICE_STATE_UPDATE" => {
                #[derive(Deserialize)]
                struct Target {
                    user_id: Snowflake,
                    #[serde(default)]
                    connection_id: Option<String>,
                    #[serde(default)]
                    session_id: Option<String>,
                }
                let target: Target = decode(event, data.clone())?;
                let connection = target.connection_id.or(target.session_id);
                let matching: Vec<_> = self
                    .voice_states
                    .iter()
                    .filter(|((user, key), _)| {
                        *user == target.user_id && connection.as_ref().is_none_or(|id| key == id)
                    })
                    .collect();
                let voice: VoiceState = if matching.len() == 1 {
                    patch(event, matching[0].1, data)?
                } else if data.get("channel_id").is_some() {
                    decode(event, data)?
                } else {
                    return Ok(ApplyOutcome::Ignored);
                };
                if let Some((guild, member)) = voice
                    .guild_id
                    .zip(voice.extra.get("member").filter(|m| !m.is_null()))
                {
                    let member: Member = decode(event, member.clone())?;
                    self.hydrate_member_if_missing(guild, member);
                }
                self.insert_voice_state(voice);
            }
            "TYPING_START" => {
                #[derive(Deserialize)]
                struct Typing {
                    channel_id: Snowflake,
                    user_id: Snowflake,
                    timestamp: u64,
                }
                let typing: Typing = decode(event, data)?;
                self.typing.entry(typing.channel_id).or_default().insert(
                    typing.user_id,
                    typing.timestamp.saturating_mul(1000).saturating_add(10_000),
                );
            }
            "MESSAGE_ACK" => {
                #[derive(Deserialize)]
                struct Ack {
                    channel_id: Snowflake,
                    message_id: Snowflake,
                    #[serde(default)]
                    mention_count: Option<u32>,
                    #[serde(default)]
                    version: Option<String>,
                    #[serde(default)]
                    manual: bool,
                }
                let ack: Ack = decode(event, data)?;
                let read = self.read_states.entry(ack.channel_id).or_insert(ReadState {
                    id: ack.channel_id,
                    ..ReadState::default()
                });
                // ReadStateServerAckMachine routes stale versions before the
                // manual/older/current/newer message cases. Decimal comparison
                // must preserve full precision and leading-zero semantics.
                if ack.version.is_some()
                    && compare_read_versions(ack.version.as_deref(), read.version.as_deref())
                        .is_lt()
                {
                    return Ok(ApplyOutcome::Ignored);
                }
                if let Some(version) = ack.version {
                    read.version = Some(version);
                }
                if ack.manual || read.last_message_id.is_none_or(|id| ack.message_id >= id) {
                    if ack.manual {
                        read.manual_ack = true;
                    } else if read.last_message_id.is_none_or(|id| ack.message_id > id) {
                        read.manual_ack = false;
                    }
                    read.last_message_id = Some(ack.message_id);
                    if let Some(count) = ack.mention_count {
                        read.mention_count = count;
                    }
                }
            }
            "CHANNEL_PINS_UPDATE" => {
                #[derive(Deserialize)]
                struct Pins {
                    channel_id: Snowflake,
                    #[serde(default)]
                    last_pin_timestamp: Option<String>,
                }
                let pins: Pins = decode(event, data)?;
                if let Some(timestamp) = pins
                    .last_pin_timestamp
                    .filter(|timestamp| !timestamp.is_empty())
                    && let Some(channel) = self.channels.get_mut(&pins.channel_id)
                {
                    channel.last_pin_timestamp = Some(timestamp);
                }
            }
            "CHANNEL_PINS_ACK" => {
                #[derive(Deserialize)]
                struct Ack {
                    channel_id: Snowflake,
                    #[serde(default)]
                    last_pin_timestamp: Option<String>,
                }
                let ack: Ack = decode(event, data)?;
                self.apply_pin_ack(ack.channel_id, ack.last_pin_timestamp.as_deref());
            }
            _ => return Ok(ApplyOutcome::Ignored),
        }
        Ok(ApplyOutcome::Applied)
    }
}
fn pin_timestamp(value: Option<&str>) -> i64 {
    value
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|time| time.timestamp_millis())
        .unwrap_or(0)
}
fn compare_read_versions(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
    fn normalized(value: &str) -> Option<&str> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let value = value.trim_start_matches('0');
        Some(if value.is_empty() { "0" } else { value })
    }
    match left.and_then(normalized).zip(right.and_then(normalized)) {
        Some((left, right)) => left.len().cmp(&right.len()).then_with(|| left.cmp(right)),
        None => std::cmp::Ordering::Equal,
    }
}
#[derive(Deserialize)]
struct Id {
    id: Snowflake,
}
#[derive(Deserialize)]
struct MessageId {
    id: Snowflake,
    channel_id: Snowflake,
}
fn decode<T: DeserializeOwned>(event: &str, data: Value) -> Result<T, StateError> {
    serde_json::from_value(data).map_err(|source| StateError::InvalidDispatch {
        event: event.to_owned(),
        source,
    })
}
/// Resolve only user-bearing wire fields; IDs themselves remain untouched. This
/// mirrors the desktop user cache rather than inventing a missing username.
fn hydrate_wire_users(
    data: &mut Value,
    users: &BTreeMap<Snowflake, User>,
    self_id: Option<Snowflake>,
) {
    fn hydrate(user: &mut Value, users: &BTreeMap<Snowflake, User>, self_id: Option<Snowflake>) {
        let Some(id) = user
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<Snowflake>().ok())
        else {
            return;
        };
        let Some(cached) = users.get(&id) else {
            return;
        };
        let mut merged = serde_json::to_value(cached).expect("user serialization cannot fail");
        // Public-only self references must not replace private account data.
        if self_id != Some(id)
            && let (Some(base), Some(update)) = (merged.as_object_mut(), user.as_object())
        {
            for (key, value) in update {
                base.insert(key.clone(), value.clone());
            }
        }
        *user = merged;
    }
    match data {
        Value::Object(fields) => {
            for (key, value) in fields {
                match key.as_str() {
                    "user" | "author" => hydrate(value, users, self_id),
                    "users" | "recipients" | "mentions" => {
                        if let Some(values) = value.as_array_mut() {
                            for user in values {
                                hydrate(user, users, self_id);
                            }
                        }
                    }
                    _ => {}
                }
                hydrate_wire_users(value, users, self_id);
            }
        }
        Value::Array(values) => {
            for value in values {
                hydrate_wire_users(value, users, self_id);
            }
        }
        _ => {}
    }
}
/// Gateway guilds wrap display properties while REST guilds are flat.
fn normalize_guild(mut data: Value) -> Value {
    if let Some(fields) = data.as_object_mut() {
        if let Some(Value::Object(properties)) = fields.remove("properties") {
            for (key, value) in properties {
                fields.entry(key).or_insert(value);
            }
        }
        if fields.get("unavailable").and_then(Value::as_bool) == Some(true) {
            fields.entry("name").or_insert(Value::String(String::new()));
        }
    }
    data
}
fn patch<T: Serialize + DeserializeOwned>(
    event: &str,
    current: &T,
    update: Value,
) -> Result<T, StateError> {
    fn merge(target: &mut Value, update: Value) {
        if let (Some(target), Some(update)) = (target.as_object_mut(), update.as_object()) {
            for (key, value) in update {
                if let Some(old) = target.get_mut(key) {
                    if old.is_object() && value.is_object() {
                        merge(old, value.clone());
                    } else {
                        *old = value.clone();
                    }
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
    }
    let mut value = serde_json::to_value(current).expect("domain model serialization cannot fail");
    merge(&mut value, update);
    decode(event, value)
}

#[cfg(test)]
mod attachment_preferences_tests {
    use super::*;

    #[test]
    fn ready_and_partial_user_settings_updates_preserve_privacy_until_authoritative_replacement() {
        // GatewayReady + UserSettingsUpdate.ts delegate the entire settings
        // object to UserSettings.updateUserSettings; omitted fields persist.
        let mut state = ClientState::default();
        state
            .apply_gateway(
                "READY",
                serde_json::json!({
                    "session_id":"fixture","user":{"id":"1","username":"fixture"},
                    "user_settings":{"locale":"en-US","synced_preferences":"ggUCGAE="}
                }),
            )
            .unwrap();
        assert_eq!(crate::preupload_override(&state.user_settings), Some(true));
        state
            .apply_gateway("USER_SETTINGS_UPDATE", serde_json::json!({"locale":"de"}))
            .unwrap();
        assert_eq!(state.user_settings["locale"], "de");
        assert_eq!(crate::preupload_override(&state.user_settings), Some(true));
        state
            .apply_gateway(
                "USER_SETTINGS_UPDATE",
                serde_json::json!({"synced_preferences":"ggUCGAA="}),
            )
            .unwrap();
        assert_eq!(state.user_settings["locale"], "de");
        assert_eq!(crate::preupload_override(&state.user_settings), Some(false));
        state
            .apply_gateway(
                "READY",
                serde_json::json!({"session_id":"fresh-session","user":{"id":"1","username":"fixture"}}),
            )
            .unwrap();
        assert!(
            state.user_settings.is_null(),
            "fresh READY settings replace the earlier settings snapshot"
        );
    }
}

#[cfg(test)]
mod navigation_state_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn server_ack_ordering_compares_versions_before_manual_or_message_order() {
        let mut state = ClientState::default();
        let channel = Snowflake(1);
        for data in [
            json!({"channel_id":"1","message_id":"100","mention_count":4,"version":"0007"}),
            json!({"channel_id":"1","message_id":"150","mention_count":0,"version":"6"}),
        ] {
            state.apply_gateway("MESSAGE_ACK", data).unwrap();
        }
        let read = &state.read_states[&channel];
        assert_eq!(read.last_message_id, Some(100.into()));
        assert_eq!(read.mention_count, 4);
        assert_eq!(read.version.as_deref(), Some("0007"));
        state
            .apply_gateway(
                "MESSAGE_ACK",
                json!({"channel_id":"1","message_id":"90","mention_count":9,"version":"8"}),
            )
            .unwrap();
        let read = &state.read_states[&channel];
        assert_eq!(read.last_message_id, Some(100.into()));
        assert_eq!(read.mention_count, 4);
        assert_eq!(read.version.as_deref(), Some("8"));
        state
            .apply_gateway(
                "MESSAGE_ACK",
                json!({"channel_id":"1","message_id":"100","mention_count":3,"version":"9"}),
            )
            .unwrap();
        assert_eq!(state.read_states[&channel].mention_count, 3);
        state.apply_gateway("MESSAGE_ACK",json!({"channel_id":"1","message_id":"50","mention_count":7,"manual":true,"version":"8"})).unwrap();
        assert_eq!(
            state.read_states[&channel].last_message_id,
            Some(100.into())
        );
        state.apply_gateway("MESSAGE_ACK",json!({"channel_id":"1","message_id":"50","mention_count":7,"manual":true,"version":"10"})).unwrap();
        assert_eq!(state.read_states[&channel].last_message_id, Some(50.into()));
        assert_eq!(state.read_states[&channel].mention_count, 7);
        state
            .apply_gateway("MESSAGE_ACK", json!({"channel_id":"1","message_id":"60"}))
            .unwrap();
        assert_eq!(
            state.read_states[&channel].mention_count, 7,
            "missing mention_count does not manufacture zero"
        );
        assert_eq!(
            compare_read_versions(
                Some("184467440737095516159999"),
                Some("18446744073709551615")
            ),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_read_versions(Some("invalid"), Some("10")),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn pin_timestamps_use_channel_latest_and_read_state_ack_without_changing_message_read_cursor() {
        let mut state = ClientState::default();
        state.apply_gateway("READY",json!({"session_id":"pins","user":{"id":"1","username":"fixture"},"guilds":[{"id":"10","name":"Fixture","channels":[{"id":"100","type":0,"last_message_id":"999","last_pin_timestamp":"2026-11-12T12:00:00Z"}]}],"read_states":[{"id":"100","last_message_id":"10","mention_count":3,"last_pin_timestamp":"2026-11-12T11:00:00Z"}]})).unwrap();
        let channel = Snowflake(100);
        assert!(state.has_unread_pins(channel));
        assert!(state.apply_pin_ack(channel, None));
        assert!(!state.has_unread_pins(channel));
        assert!(
            !state.apply_pin_ack(channel, Some("2026-11-12T13:00:00+01:00")),
            "equivalent timestamps must not repeat the ack"
        );
        state
            .apply_gateway(
                "CHANNEL_PINS_UPDATE",
                json!({"channel_id":"100","last_pin_timestamp":"2026-11-12T13:00:00Z"}),
            )
            .unwrap();
        assert!(state.has_unread_pins(channel));
        state
            .apply_gateway(
                "CHANNEL_PINS_ACK",
                json!({"channel_id":"100","last_pin_timestamp":null}),
            )
            .unwrap();
        assert!(!state.has_unread_pins(channel));
        assert_eq!(state.read_states[&channel].last_message_id, Some(10.into()));
        assert_eq!(state.read_states[&channel].mention_count, 3);
        assert!(state.is_unread(channel));
        state
            .apply_gateway(
                "CHANNEL_PINS_UPDATE",
                json!({"channel_id":"100","last_pin_timestamp":null}),
            )
            .unwrap();
        assert!(!state.has_unread_pins(channel));
    }

    #[test]
    fn sticker_gateway_update_replaces_guild_catalog() {
        let mut state = ClientState::default();
        state
            .apply_gateway(
                "GUILD_CREATE",
                json!({"id":"10", "name":"Community", "stickers":[{"id":"1","name":"old"}]}),
            )
            .unwrap();
        state
            .apply_gateway(
                "GUILD_STICKERS_UPDATE",
                json!({"guild_id":"10", "stickers":[{"id":"2","name":"new", "animated":true}]}),
            )
            .unwrap();
        assert_eq!(
            state.guilds[&10.into()].extra["stickers"],
            json!([{"id":"2","name":"new", "animated":true}])
        );
        state
            .apply_gateway(
                "GUILD_STICKERS_UPDATE",
                json!({"guild_id":"10", "stickers":[]}),
            )
            .unwrap();
        assert_eq!(state.guilds[&10.into()].extra["stickers"], json!([]));
    }

    #[test]
    fn thread_guild_activation_preserves_empty_array_presence_and_transient_unavailability() {
        let mut state = ClientState::default();
        state.apply_gateway("READY",json!({"user":{"id":"1","username":"fixture"},"guilds":[{"id":"10","name":"Enabled","threads":[]},{"id":"20","name":"Disabled"}]})).unwrap();
        assert!(state.thread_guilds.contains(&10.into()));
        assert!(!state.thread_guilds.contains(&20.into()));
        state
            .apply_gateway("GUILD_DELETE", json!({"id":"10","unavailable":true}))
            .unwrap();
        assert!(state.thread_guilds.contains(&10.into()));
        state
            .apply_gateway("GUILD_CREATE", json!({"id":"10","unavailable":true}))
            .unwrap();
        assert!(state.thread_guilds.contains(&10.into()));
        state
            .apply_gateway(
                "GUILD_CREATE",
                json!({"id":"10","name":"Projection without threads"}),
            )
            .unwrap();
        assert!(!state.thread_guilds.contains(&10.into()));
        state
            .apply_gateway(
                "GUILD_CREATE",
                json!({"id":"20","name":"Newly enabled","threads":[]}),
            )
            .unwrap();
        assert!(state.thread_guilds.contains(&20.into()));
        state
            .apply_gateway("GUILD_DELETE", json!({"id":"20"}))
            .unwrap();
        assert!(!state.thread_guilds.contains(&20.into()));
    }
}

#[cfg(test)]
mod member_list_projection_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sync_replaces_only_covered_rows_and_rejects_invalid_ranges() {
        let mut state = ClientState::default();
        let dispatch = |ops| json!({"guild_id":"1","channel_id":"2","id":"everyone","member_count":2,"online_count":1,"groups":[],"ops":ops});
        state.apply_gateway("GUILD_MEMBER_LIST_UPDATE", dispatch(json!([{ "op":"SYNC", "range":[0,2], "items":[{"group":{"id":"online","count":1}},{"member":{"user":{"id":"42"}}},{"group":{"id":"offline","count":1}}] }]))).unwrap();
        let list = &state.member_lists[&(1.into(), 2.into())];
        assert_eq!(list.rows.len(), 3);
        assert_eq!(list.rows[&1]["member"]["user"]["id"], "42");
        state.apply_gateway("GUILD_MEMBER_LIST_UPDATE", dispatch(json!([{ "op":"SYNC", "range":[1,1], "items":[] }, {"op":"SYNC","range":[0,100],"items":[{"bad":true}]}]))).unwrap();
        let list = &state.member_lists[&(1.into(), 2.into())];
        assert_eq!(list.rows.len(), 2);
        assert!(!list.rows.contains_key(&1));
        // ID-only references without a cached user do not fabricate a member.
        assert!(state.members.is_empty());
    }
}
