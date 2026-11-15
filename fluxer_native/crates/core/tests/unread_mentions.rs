// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source fixtures for ReadStateEntry.shouldMentionFor/ReadStateMentionMachine.
use fluxer_core::{ChannelType, ClientState, Message, Relationship, Snowflake, UserGuildSettings};
use serde_json::json;
fn message(state: &ClientState, id: u64, channel: u64) -> Message {
    Message::text(
        Snowflake(id),
        Snowflake(channel),
        state.users[&Snowflake(101)].clone(),
        "fixture",
    )
}
fn count(state: &ClientState, channel: u64) -> u32 {
    state
        .read_states
        .get(&Snowflake(channel))
        .map_or(0, |r| r.mention_count)
}
#[test]
fn live_dm_and_role_mentions_match_the_source_and_duplicate_dispatches_do_not_count_twice() {
    let mut state = ClientState::demo();
    let before = count(&state, 40);
    let dm = message(&state, 9000, 40);
    state.insert_message(dm.clone());
    assert_eq!(count(&state, 40), before + 1);
    state.insert_message(dm);
    assert_eq!(count(&state, 40), before + 1);
    let me = state.current_user.as_ref().unwrap().id;
    state
        .members
        .get_mut(&(Snowflake(1), me))
        .unwrap()
        .roles
        .push(Snowflake(777));
    let before = count(&state, 11);
    let mut role = message(&state, 9001, 11);
    role.mention_roles.push(Snowflake(777));
    state.insert_message(role);
    assert_eq!(count(&state, 11), before + 1);
}
#[test]
fn blocked_authors_suppressed_everyone_roles_and_muted_ordinary_dms_do_not_increment() {
    let mut state = ClientState::demo();
    let before = count(&state, 11);
    state.relationships.insert(
        Snowflake(101),
        Relationship {
            id: Snowflake(101),
            kind: 2,
            ..Default::default()
        },
    );
    let mut blocked = message(&state, 9100, 11);
    blocked.mentions.push(state.current_user.clone().unwrap());
    state.insert_message(blocked);
    assert_eq!(count(&state, 11), before);
    state.relationships.clear();
    state.user_guild_settings.insert(
        Some(Snowflake(1)),
        serde_json::from_value(json!({"suppress_everyone":true,"suppress_roles":true})).unwrap(),
    );
    let me = state.current_user.as_ref().unwrap().id;
    state
        .members
        .get_mut(&(Snowflake(1), me))
        .unwrap()
        .roles
        .push(Snowflake(777));
    let mut suppressed = message(&state, 9101, 11);
    suppressed.mention_everyone = true;
    suppressed.mention_roles.push(Snowflake(777));
    state.insert_message(suppressed);
    assert_eq!(count(&state, 11), before);
    state.user_guild_settings.insert(
        None,
        UserGuildSettings {
            muted: true,
            ..Default::default()
        },
    );
    let before = count(&state, 40);
    let dm = message(&state, 9102, 40);
    state.insert_message(dm);
    assert_eq!(count(&state, 40), before);
    let mut explicit = message(&state, 9103, 40);
    explicit.mentions.push(state.current_user.clone().unwrap());
    state.insert_message(explicit);
    assert_eq!(count(&state, 40), before + 1);
}
#[test]
fn nonmember_threads_ignore_everyone_but_keep_direct_mentions_and_acked_history_stays_covered() {
    let mut state = ClientState::demo();
    let channel = state.channels.get_mut(&Snowflake(11)).unwrap();
    channel.kind = ChannelType::PublicThread;
    channel.extra.remove("member");
    let before = count(&state, 11);
    let mut everyone = message(&state, 9200, 11);
    everyone.mention_everyone = true;
    state.insert_message(everyone);
    assert_eq!(count(&state, 11), before);
    let mut direct = message(&state, 9201, 11);
    direct.mentions.push(state.current_user.clone().unwrap());
    state.insert_message(direct);
    assert_eq!(count(&state, 11), before + 1);
    state
        .apply_gateway(
            "MESSAGE_ACK",
            json!({"channel_id":"11","message_id":"9300","mention_count":0,"version":"2"}),
        )
        .unwrap();
    let mut covered = message(&state, 9250, 11);
    covered.mentions.push(state.current_user.clone().unwrap());
    state.insert_message(covered);
    assert_eq!(count(&state, 11), 0);
}
