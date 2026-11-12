use crate::{Guild, Member, PermissionOverwrite, Snowflake};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::ops::{BitAnd, BitOr, BitOrAssign, Not};

/// Permission bitset with string wire encoding, matching the API bigint schema.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Permissions(pub u64);
impl Permissions {
    pub const NONE: Self = Self(0);
    pub const CREATE_INSTANT_INVITE: Self = Self(1 << 0);
    pub const KICK_MEMBERS: Self = Self(1 << 1);
    pub const BAN_MEMBERS: Self = Self(1 << 2);
    pub const ADMINISTRATOR: Self = Self(1 << 3);
    pub const MANAGE_CHANNELS: Self = Self(1 << 4);
    pub const MANAGE_GUILD: Self = Self(1 << 5);
    pub const ADD_REACTIONS: Self = Self(1 << 6);
    pub const VIEW_AUDIT_LOG: Self = Self(1 << 7);
    pub const PRIORITY_SPEAKER: Self = Self(1 << 8);
    pub const STREAM: Self = Self(1 << 9);
    pub const VIEW_CHANNEL: Self = Self(1 << 10);
    pub const SEND_MESSAGES: Self = Self(1 << 11);
    pub const SEND_TTS_MESSAGES: Self = Self(1 << 12);
    pub const MANAGE_MESSAGES: Self = Self(1 << 13);
    pub const EMBED_LINKS: Self = Self(1 << 14);
    pub const ATTACH_FILES: Self = Self(1 << 15);
    pub const READ_MESSAGE_HISTORY: Self = Self(1 << 16);
    pub const MENTION_EVERYONE: Self = Self(1 << 17);
    pub const USE_EXTERNAL_EMOJIS: Self = Self(1 << 18);
    pub const CONNECT: Self = Self(1 << 20);
    pub const SPEAK: Self = Self(1 << 21);
    pub const MUTE_MEMBERS: Self = Self(1 << 22);
    pub const DEAFEN_MEMBERS: Self = Self(1 << 23);
    pub const MOVE_MEMBERS: Self = Self(1 << 24);
    pub const USE_VAD: Self = Self(1 << 25);
    pub const CHANGE_NICKNAME: Self = Self(1 << 26);
    pub const MANAGE_NICKNAMES: Self = Self(1 << 27);
    pub const MANAGE_ROLES: Self = Self(1 << 28);
    pub const MANAGE_WEBHOOKS: Self = Self(1 << 29);
    pub const MANAGE_EXPRESSIONS: Self = Self(1 << 30);
    pub const MANAGE_THREADS: Self = Self(1 << 34);
    pub const CREATE_PUBLIC_THREADS: Self = Self(1 << 35);
    pub const CREATE_PRIVATE_THREADS: Self = Self(1 << 36);
    pub const USE_EXTERNAL_STICKERS: Self = Self(1 << 37);
    pub const SEND_MESSAGES_IN_THREADS: Self = Self(1 << 38);
    pub const MODERATE_MEMBERS: Self = Self(1 << 40);
    pub const CREATE_EXPRESSIONS: Self = Self(1 << 43);
    pub const PIN_MESSAGES: Self = Self(1 << 51);
    pub const BYPASS_SLOWMODE: Self = Self(1 << 52);
    pub const UPDATE_RTC_REGION: Self = Self(1 << 53);
    pub const VIEW_CHANNEL_MEMBERS: Self = Self(1 << 54);
    pub const ALL: Self = Self(
        ((1 << 19) - 1) | ((1 << 31) - (1 << 20)) | (31 << 34) | (1 << 40) | (1 << 43) | (15 << 51),
    );
    pub const DEFAULT: Self = Self(
        (1 << 0)
            | (1 << 6)
            | (1 << 9)
            | (1 << 10)
            | (1 << 11)
            | (1 << 14)
            | (1 << 15)
            | (1 << 16)
            | (1 << 18)
            | (1 << 20)
            | (1 << 21)
            | (1 << 25)
            | (1 << 26)
            | (1 << 37)
            | (1 << 54),
    );
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
    pub const fn apply_overwrite(self, allow: Self, deny: Self) -> Self {
        Self((self.0 & !deny.0) | allow.0)
    }
}
impl BitOr for Permissions {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}
impl BitAnd for Permissions {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}
impl BitOrAssign for Permissions {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}
impl Not for Permissions {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}
impl Serialize for Permissions {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for Permissions {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = String::deserialize(d)?;
        if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
            return Err(de::Error::custom(
                "permissions must be an unsigned decimal string",
            ));
        }
        value.parse().map(Self).map_err(de::Error::custom)
    }
}

/// Resolve everyone, aggregate member roles, everyone overwrite, aggregate role
/// overwrites, then member overwrite. Owners/admins bypass channel overwrites.
/// Callers must separately enforce MFA elevation, guest visibility, timeout,
/// private thread membership, archived/locked threads, and server-side checks.
pub fn resolve_permissions(
    guild: &Guild,
    member: &Member,
    overwrites: &[PermissionOverwrite],
) -> Permissions {
    if guild.owner_id == member.user.id {
        return Permissions::ALL;
    }
    let mut permissions = guild
        .roles
        .iter()
        .find(|r| r.id == guild.id)
        .map(|r| r.permissions)
        .unwrap_or(Permissions::NONE);
    for role in guild.roles.iter().filter(|r| member.roles.contains(&r.id)) {
        permissions |= role.permissions;
    }
    if permissions.contains(Permissions::ADMINISTRATOR) {
        return Permissions::ALL;
    }
    if let Some(overwrite) = overwrites.iter().find(|o| o.kind == 0 && o.id == guild.id) {
        permissions = permissions.apply_overwrite(overwrite.allow, overwrite.deny);
    }
    let (mut allow, mut deny) = (Permissions::NONE, Permissions::NONE);
    for overwrite in overwrites
        .iter()
        .filter(|o| o.kind == 0 && o.id != guild.id && member.roles.contains(&o.id))
    {
        allow |= overwrite.allow;
        deny |= overwrite.deny;
    }
    permissions = permissions.apply_overwrite(allow, deny);
    if let Some(overwrite) = overwrites
        .iter()
        .find(|o| o.kind == 1 && o.id == member.user.id)
    {
        permissions = permissions.apply_overwrite(overwrite.allow, overwrite.deny);
    }
    permissions
}

/// Gate the voice controls using effective channel permissions and timeout.
/// These are UI affordances; the server remains authoritative.
pub fn can_join_voice(permissions: Permissions, timed_out: bool) -> bool {
    !timed_out && permissions.contains(Permissions::VIEW_CHANNEL | Permissions::CONNECT)
}
pub fn can_speak(permissions: Permissions, timed_out: bool) -> bool {
    can_join_voice(permissions, timed_out) && permissions.contains(Permissions::SPEAK)
}
pub fn can_stream(permissions: Permissions, timed_out: bool) -> bool {
    can_join_voice(permissions, timed_out) && permissions.contains(Permissions::STREAM)
}

/// Stable role hierarchy: higher position wins, ties favor the older snowflake.
pub fn role_is_higher(a_position: i32, a_id: Snowflake, b_position: i32, b_id: Snowflake) -> bool {
    a_position > b_position || (a_position == b_position && a_id < b_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Role, User};
    fn fixture() -> (Guild, Member) {
        (
            Guild {
                id: 1.into(),
                owner_id: 99.into(),
                roles: vec![
                    Role {
                        id: 1.into(),
                        permissions: Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES,
                        ..Role::default()
                    },
                    Role {
                        id: 2.into(),
                        ..Role::default()
                    },
                    Role {
                        id: 3.into(),
                        ..Role::default()
                    },
                ],
                ..Guild::default()
            },
            Member {
                user: User {
                    id: 5.into(),
                    ..User::default()
                },
                roles: vec![2.into(), 3.into()],
                ..Member::default()
            },
        )
    }
    #[test]
    fn overwrite_order_is_aggregated_and_member_is_last() {
        let (guild, member) = fixture();
        let overwrites = vec![
            PermissionOverwrite {
                id: 1.into(),
                kind: 0,
                deny: Permissions::SEND_MESSAGES,
                ..PermissionOverwrite::default()
            },
            PermissionOverwrite {
                id: 2.into(),
                kind: 0,
                deny: Permissions::SEND_MESSAGES,
                ..PermissionOverwrite::default()
            },
            PermissionOverwrite {
                id: 3.into(),
                kind: 0,
                allow: Permissions::SEND_MESSAGES,
                ..PermissionOverwrite::default()
            },
        ];
        assert!(
            resolve_permissions(&guild, &member, &overwrites).contains(Permissions::SEND_MESSAGES)
        );
        let mut reversed = overwrites.clone();
        reversed.reverse();
        assert_eq!(
            resolve_permissions(&guild, &member, &overwrites),
            resolve_permissions(&guild, &member, &reversed)
        );
        reversed.push(PermissionOverwrite {
            id: 5.into(),
            kind: 1,
            deny: Permissions::SEND_MESSAGES,
            ..PermissionOverwrite::default()
        });
        assert!(
            !resolve_permissions(&guild, &member, &reversed).contains(Permissions::SEND_MESSAGES)
        );
    }
    #[test]
    fn owner_and_administrator_bypass_denies() {
        let (mut guild, mut member) = fixture();
        let deny = [PermissionOverwrite {
            id: guild.id,
            kind: 0,
            deny: Permissions::ALL,
            ..PermissionOverwrite::default()
        }];
        guild.owner_id = member.user.id;
        assert_eq!(
            resolve_permissions(&guild, &member, &deny),
            Permissions::ALL
        );
        guild.owner_id = 99.into();
        guild.roles[1].permissions = Permissions::ADMINISTRATOR;
        assert_eq!(
            resolve_permissions(&guild, &member, &deny),
            Permissions::ALL
        );
        member.roles.clear();
        assert_eq!(
            resolve_permissions(&guild, &member, &deny),
            Permissions::NONE
        );
    }
    #[test]
    fn voice_gates_require_view_connect_and_each_capability() {
        let perms = Permissions::CONNECT | Permissions::SPEAK | Permissions::STREAM;
        assert!(!can_join_voice(perms, false));
        let perms = perms | Permissions::VIEW_CHANNEL;
        assert!(can_join_voice(perms, false));
        assert!(can_speak(perms, false));
        assert!(can_stream(perms, false));
        assert!(!can_join_voice(perms, true));
        assert!(!can_speak(perms, true));
        assert!(!can_stream(perms.without(Permissions::STREAM), false));
    }
    #[test]
    fn high_bits_round_trip() {
        let perms = Permissions::VIEW_CHANNEL_MEMBERS | Permissions::MANAGE_THREADS;
        assert_eq!(
            serde_json::from_str::<Permissions>(&serde_json::to_string(&perms).unwrap()).unwrap(),
            perms
        );
        assert!(serde_json::from_str::<Permissions>("18014398509481984").is_err());
        assert!(
            Permissions::ALL
                .contains(Permissions::PIN_MESSAGES | Permissions::SEND_MESSAGES_IN_THREADS)
        );
    }

    #[test]
    fn absent_everyone_role_fails_closed() {
        let (mut guild, member) = fixture();
        guild.roles.clear();
        let permissions = resolve_permissions(&guild, &member, &[]);
        assert_eq!(permissions, Permissions::NONE);
        assert!(!can_join_voice(permissions, false));
        assert!(!permissions.contains(Permissions::SEND_MESSAGES));
    }
}
