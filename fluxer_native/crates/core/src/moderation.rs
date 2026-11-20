// SPDX-License-Identifier: AGPL-3.0-or-later
//! Guild moderation capability checks, matching GuildModerationCapabilityUtils.
use crate::{Guild, Member, Permissions, Role, resolve_permissions};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModerationCapabilities {
    pub kick: bool,
    pub ban: bool,
    pub timeout: bool,
}

/// Unknown members or roles fail closed. Channel overwrites do not affect guild
/// moderation permissions. The server remains authoritative for every action.
pub fn moderation_capabilities(
    guild: &Guild,
    actor: &Member,
    target: &Member,
) -> ModerationCapabilities {
    let none = ModerationCapabilities::default();
    if actor.user.id == target.user.id || target.user.id == guild.owner_id {
        return none;
    }
    if actor
        .roles
        .iter()
        .chain(target.roles.iter())
        .any(|id| !guild.roles.iter().any(|role| role.id == *id))
    {
        return none;
    }
    let highest = |member: &Member| -> Option<&Role> {
        guild
            .roles
            .iter()
            .filter(|role| member.roles.contains(&role.id))
            .max_by(|a, b| a.position.cmp(&b.position).then_with(|| b.id.cmp(&a.id)))
    };
    let can_manage = actor.user.id == guild.owner_id
        || match (highest(actor), highest(target)) {
            (Some(a), Some(b)) => {
                a.position > b.position || (a.position == b.position && a.id < b.id)
            }
            (Some(_), None) => true,
            _ => false,
        };
    if !can_manage {
        return none;
    }
    let permissions = resolve_permissions(guild, actor, &[]);
    let target_permissions = resolve_permissions(guild, target, &[]);
    ModerationCapabilities {
        kick: permissions.contains(Permissions::KICK_MEMBERS),
        ban: permissions.contains(Permissions::BAN_MEMBERS),
        timeout: permissions.contains(Permissions::MODERATE_MEMBERS)
            && !target_permissions.contains(Permissions::ADMINISTRATOR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Snowflake, User};

    fn member(id: u64, roles: &[u64]) -> Member {
        Member {
            user: User {
                id: Snowflake(id),
                ..Default::default()
            },
            roles: roles.iter().copied().map(Snowflake).collect(),
            ..Default::default()
        }
    }
    #[test]
    fn hierarchy_and_permissions_gate_moderation() {
        let mut guild = Guild {
            id: Snowflake(1),
            owner_id: Snowflake(2),
            roles: vec![
                Role {
                    id: Snowflake(1),
                    ..Default::default()
                },
                Role {
                    id: Snowflake(10),
                    position: 1,
                    permissions: Permissions::KICK_MEMBERS
                        | Permissions::BAN_MEMBERS
                        | Permissions::MODERATE_MEMBERS,
                    ..Default::default()
                },
                Role {
                    id: Snowflake(11),
                    position: 1,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let actor = member(3, &[10]);
        let target = member(4, &[11]);
        assert_eq!(
            moderation_capabilities(&guild, &actor, &target),
            ModerationCapabilities {
                kick: true,
                ban: true,
                timeout: true
            }
        );
        assert_eq!(
            moderation_capabilities(&guild, &target, &actor),
            ModerationCapabilities::default()
        );
        assert_eq!(
            moderation_capabilities(&guild, &actor, &actor),
            ModerationCapabilities::default()
        );
        assert_eq!(
            moderation_capabilities(&guild, &actor, &member(2, &[])),
            ModerationCapabilities::default()
        );
        assert_eq!(
            moderation_capabilities(&guild, &actor, &member(5, &[99])),
            ModerationCapabilities::default()
        );
        guild.roles[2].permissions = Permissions::ADMINISTRATOR;
        assert!(!moderation_capabilities(&guild, &actor, &target).timeout);
        assert!(moderation_capabilities(&guild, &actor, &target).kick);
    }
}
