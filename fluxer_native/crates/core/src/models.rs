use crate::{Permissions, Snowflake};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::BTreeMap;

fn null_default<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(
    d: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: Snowflake,
    pub username: String,
    #[serde(default)]
    pub discriminator: String,
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bot: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
impl User {
    pub fn display_name(&self) -> &str {
        self.global_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.username)
    }
}

/// Profile response, scoped by the request's optional guild.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileDetails {
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub pronouns: Option<String>,
    #[serde(default)]
    pub banner_color: Option<i64>,
    #[serde(default)]
    pub accent_color: Option<i64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UserProfile {
    pub user: User,
    #[serde(default)]
    pub user_profile: ProfileDetails,
    #[serde(default)]
    pub guild_member_profile: Option<ProfileDetails>,
    #[serde(default)]
    pub guild_member: Option<Member>,
    #[serde(default)]
    pub timezone_offset: Option<i32>,
    #[serde(default)]
    pub profile_badges: Vec<Value>,
    #[serde(default)]
    pub hidden_profile_badge_count: u64,
    #[serde(default)]
    pub mutual_friends: Option<Vec<User>>,
    #[serde(default)]
    pub mutual_guilds: Option<Vec<Value>>,
    #[serde(default)]
    pub connected_accounts: Option<Vec<Value>>,
    #[serde(default)]
    pub profile_limited: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Numeric channel types from ChannelConstants.ts. Unknown values are preserved
/// so introducing a server feature does not invalidate READY.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChannelType {
    #[default]
    Text,
    Dm,
    Voice,
    GroupDm,
    Category,
    Announcement,
    AnnouncementThread,
    PublicThread,
    PrivateThread,
    Forum,
    Media,
    Link,
    PersonalNotes,
    Unknown(u16),
}
impl ChannelType {
    pub fn code(self) -> u16 {
        match self {
            Self::Text => 0,
            Self::Dm => 1,
            Self::Voice => 2,
            Self::GroupDm => 3,
            Self::Category => 4,
            Self::Announcement => 5,
            Self::AnnouncementThread => 10,
            Self::PublicThread => 11,
            Self::PrivateThread => 12,
            Self::Forum => 15,
            Self::Media => 16,
            Self::Link => 998,
            Self::PersonalNotes => 999,
            Self::Unknown(n) => n,
        }
    }
    pub fn is_thread(self) -> bool {
        matches!(
            self,
            Self::AnnouncementThread | Self::PublicThread | Self::PrivateThread
        )
    }
    pub fn is_voice(self) -> bool {
        self == Self::Voice
    }
    pub fn is_private(self) -> bool {
        matches!(self, Self::Dm | Self::GroupDm | Self::PersonalNotes)
    }
    pub fn is_message_channel(self) -> bool {
        !matches!(self, Self::Category | Self::Link | Self::Unknown(_))
    }
}
impl Serialize for ChannelType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u16(self.code())
    }
}
impl<'de> Deserialize<'de> for ChannelType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match u16::deserialize(d)? {
            0 => Self::Text,
            1 => Self::Dm,
            2 => Self::Voice,
            3 => Self::GroupDm,
            4 => Self::Category,
            5 => Self::Announcement,
            10 => Self::AnnouncementThread,
            11 => Self::PublicThread,
            12 => Self::PrivateThread,
            15 => Self::Forum,
            16 => Self::Media,
            998 => Self::Link,
            999 => Self::PersonalNotes,
            n => Self::Unknown(n),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: Snowflake,
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: ChannelType,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub parent_id: Option<Snowflake>,
    #[serde(default)]
    pub owner_id: Option<Snowflake>,
    #[serde(default)]
    pub position: i32,
    #[serde(default)]
    pub last_message_id: Option<Snowflake>,
    #[serde(default)]
    pub last_pin_timestamp: Option<String>,
    #[serde(default)]
    pub recipients: Vec<User>,
    #[serde(default)]
    pub permission_overwrites: Vec<PermissionOverwrite>,
    #[serde(default)]
    pub nsfw: bool,
    #[serde(default)]
    pub rate_limit_per_user: u32,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
impl Channel {
    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else if self.kind == ChannelType::PersonalNotes {
            "Personal Notes".into()
        } else {
            self.recipients
                .iter()
                .map(User::display_name)
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Guild {
    pub id: Snowflake,
    pub name: String,
    #[serde(default)]
    pub owner_id: Snowflake,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub unavailable: bool,
    #[serde(default)]
    pub roles: Vec<Role>,
    #[serde(default)]
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub threads: Vec<Channel>,
    #[serde(default)]
    pub presences: Vec<Presence>,
    #[serde(default)]
    pub voice_states: Vec<VoiceState>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Role {
    pub id: Snowflake,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub position: i32,
    #[serde(default)]
    pub color: u32,
    #[serde(default)]
    pub hoist: bool,
    #[serde(default)]
    pub mentionable: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub user: User,
    #[serde(default)]
    pub nick: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub profile_flags: Option<i32>,
    #[serde(default)]
    pub roles: Vec<Snowflake>,
    #[serde(default)]
    pub communication_disabled_until: Option<String>,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub deaf: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOverwrite {
    pub id: Snowflake,
    /// 0: role, 1: member. Unknown kinds never grant permissions.
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default)]
    pub allow: Permissions,
    #[serde(default)]
    pub deny: Permissions,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Emoji {
    #[serde(default)]
    pub id: Option<Snowflake>,
    pub name: String,
    #[serde(default)]
    pub animated: bool,
}
impl Emoji {
    pub fn unicode(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
    pub fn same_as(&self, other: &Self) -> bool {
        if self.id.is_some() || other.id.is_some() {
            self.id == other.id
        } else {
            self.name == other.name
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: Emoji,
    pub count: u32,
    #[serde(default, deserialize_with = "null_default")]
    pub me: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: Snowflake,
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub flags: u32,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageReference {
    pub channel_id: Snowflake,
    #[serde(default)]
    pub message_id: Option<Snowflake>,
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    #[serde(default, rename = "type")]
    pub kind: u8,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: Snowflake,
    pub channel_id: Snowflake,
    pub author: User,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, rename = "type")]
    pub kind: u16,
    #[serde(default)]
    pub flags: u32,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub mentions: Vec<User>,
    #[serde(default)]
    pub mention_roles: Vec<Snowflake>,
    #[serde(default, deserialize_with = "null_default")]
    pub attachments: Vec<Attachment>,
    #[serde(default, deserialize_with = "null_default")]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub message_reference: Option<MessageReference>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
impl Message {
    pub fn text(
        id: Snowflake,
        channel_id: Snowflake,
        author: User,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id,
            channel_id,
            author,
            content: content.into(),
            ..Self::default()
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadState {
    #[serde(alias = "channel_id")]
    pub id: Snowflake,
    #[serde(default)]
    pub last_message_id: Option<Snowflake>,
    #[serde(default)]
    pub mention_count: u32,
    /// GatewaySchemas: timestamp of the last pin the user acknowledged.
    #[serde(default)]
    pub last_pin_timestamp: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// Local AutoAck suppression after an accepted manual mark-unread event.
    /// This is deliberately absent from REST/gateway serialization.
    #[serde(skip)]
    pub manual_ack: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceStatus {
    Online,
    Idle,
    Dnd,
    Invisible,
    #[default]
    Offline,
}
impl<'de> Deserialize<'de> for PresenceStatus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // StatusConstants.ts:22-23 normalizes null/unknown statuses to offline.
        Ok(match Value::deserialize(d)?.as_str() {
            Some("online") => Self::Online,
            Some("idle") => Self::Idle,
            Some("dnd") => Self::Dnd,
            Some("invisible") => Self::Invisible,
            _ => Self::Offline,
        })
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    pub user: User,
    #[serde(default)]
    pub status: PresenceStatus,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    pub id: Snowflake,
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub nickname: Option<String>,
}
impl Relationship {
    pub fn is_friend(&self) -> bool {
        self.kind == 1
    }
    pub fn is_blocked(&self) -> bool {
        self.kind == 2
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VoiceState {
    pub user_id: Snowflake,
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    #[serde(default)]
    pub channel_id: Option<Snowflake>,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub self_mute: bool,
    #[serde(default)]
    pub self_deaf: bool,
    #[serde(default)]
    pub self_video: bool,
    #[serde(default)]
    pub self_stream: bool,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub deaf: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    System,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientSettings {
    pub theme: Theme,
    pub compact_messages: bool,
    pub show_member_list: bool,
    pub notifications: bool,
    pub reduced_motion: bool,
    pub locale: String,
    pub font_scale: f32,
}
impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            theme: Theme::Dark,
            compact_messages: false,
            show_member_list: true,
            notifications: true,
            reduced_motion: false,
            locale: "en-US".into(),
            font_scale: 1.0,
        }
    }
}

/// Local desired call state only. A media adapter must acknowledge connection;
/// changing these controls never claims audio is being transmitted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VoiceControls {
    pub channel_id: Option<Snowflake>,
    pub muted: bool,
    pub deafened: bool,
    pub camera: bool,
    pub screen_share: bool,
    mute_before_deafen: bool,
}
impl VoiceControls {
    pub fn join(&mut self, channel_id: Snowflake) {
        self.channel_id = Some(channel_id);
    }
    pub fn disconnect(&mut self) {
        self.channel_id = None;
        self.camera = false;
        self.screen_share = false;
    }
    pub fn set_muted(&mut self, muted: bool) {
        if self.deafened {
            self.mute_before_deafen = muted;
        } else {
            self.muted = muted;
        }
    }
    pub fn set_deafened(&mut self, deafened: bool) {
        if self.deafened == deafened {
            return;
        }
        if deafened {
            self.mute_before_deafen = self.muted;
            self.muted = true;
        } else {
            self.muted = self.mute_before_deafen;
        }
        self.deafened = deafened;
    }
}
