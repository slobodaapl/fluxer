use crate::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeMap;

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

/// One state belongs to one authenticated account. A caller switching accounts
/// must replace it, avoiding accidental cross-account messages and drafts.
#[derive(Clone, Debug, Default)]
pub struct ClientState {
    pub current_user: Option<User>,
    pub users: BTreeMap<Snowflake, User>,
    pub guilds: BTreeMap<Snowflake, Guild>,
    pub channels: BTreeMap<Snowflake, Channel>,
    pub messages: BTreeMap<Snowflake, BTreeMap<Snowflake, Message>>,
    pub members: BTreeMap<(Snowflake, Snowflake), Member>,
    pub read_states: BTreeMap<Snowflake, ReadState>,
    pub relationships: BTreeMap<Snowflake, Relationship>,
    pub presences: BTreeMap<Snowflake, Presence>,
    /// Keyed by (user, connection) to retain simultaneous voice connections.
    pub voice_states: BTreeMap<(Snowflake, String), VoiceState>,
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
            && self.current_user.as_ref().is_some_and(|me| {
                me.id != message.author.id
                    && (message.mention_everyone || message.mentions.iter().any(|u| u.id == me.id))
            })
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
            "CHANNEL_CREATE" | "THREAD_CREATE" => {
                let channel: Channel = decode(event, data)?;
                self.channels.insert(channel.id, channel);
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
                self.remove_channel(target.id);
            }
            "GUILD_CREATE" | "GUILD_SYNC" => {
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
                    self.guilds.remove(&target.id);
                    self.members.retain(|(guild, _), _| *guild != target.id);
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
            "PRESENCE_UPDATE" => {
                let presence: Presence = decode(event, data)?;
                self.presences.insert(presence.user.id, presence);
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
                    mention_count: u32,
                }
                let ack: Ack = decode(event, data)?;
                let read = self.read_states.entry(ack.channel_id).or_insert(ReadState {
                    id: ack.channel_id,
                    ..ReadState::default()
                });
                // A delayed ACK cannot move the local cursor backward.
                if read.last_message_id.is_none_or(|id| ack.message_id >= id) {
                    read.last_message_id = Some(ack.message_id);
                    read.mention_count = ack.mention_count;
                }
            }
            _ => return Ok(ApplyOutcome::Ignored),
        }
        Ok(ApplyOutcome::Applied)
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
