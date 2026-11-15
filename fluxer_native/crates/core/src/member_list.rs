//! List identity from features/member/utils/MemberListIdentity.ts.
use crate::{Channel, Guild, Permissions};

pub fn member_list_identity(guild: &Guild, channel: &Channel) -> String {
    if guild
        .roles
        .iter()
        .any(|role| role.id == guild.id && role.permissions.contains(Permissions::VIEW_CHANNEL))
        && channel
            .permission_overwrites
            .iter()
            .all(|o| !o.deny.contains(Permissions::VIEW_CHANNEL))
    {
        return "everyone".into();
    }
    let mut tokens = Vec::new();
    for o in &channel.permission_overwrites {
        if o.allow.contains(Permissions::VIEW_CHANNEL) {
            tokens.push(format!("allow:{}", o.id));
        } else if o.deny.contains(Permissions::VIEW_CHANNEL) {
            tokens.push(format!("deny:{}", o.id));
        }
    }
    tokens.sort();
    let input = tokens.join(",");
    let bytes = input.as_bytes();
    let mut hash = 0u32;
    let (blocks, rest) = bytes.as_chunks::<4>();
    for block in blocks {
        let mut k = u32::from_le_bytes(*block);
        k = k
            .wrapping_mul(0xcc9e2d51)
            .rotate_left(15)
            .wrapping_mul(0x1b873593);
        hash ^= k;
        hash = hash
            .rotate_left(13)
            .wrapping_mul(5)
            .wrapping_add(0xe6546b64);
    }
    if !rest.is_empty() {
        let mut k = 0u32;
        for (i, byte) in rest.iter().enumerate() {
            k ^= (*byte as u32) << (8 * i);
        }
        hash ^= k
            .wrapping_mul(0xcc9e2d51)
            .rotate_left(15)
            .wrapping_mul(0x1b873593);
    }
    hash ^= bytes.len() as u32;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85ebca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2ae35);
    hash ^= hash >> 16;
    hash.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PermissionOverwrite;
    #[test]
    fn restricted_channel_fingerprint_matches_classic_utf8_murmur() {
        let guild = Guild {
            id: 1.into(),
            ..Default::default()
        };
        let channel = Channel {
            permission_overwrites: vec![PermissionOverwrite {
                id: 5.into(),
                deny: Permissions::VIEW_CHANNEL,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(member_list_identity(&guild, &channel), "147178806");
    }
}
