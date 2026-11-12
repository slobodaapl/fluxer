use fluxer_core::*;
use serde_json::{Value, json};

// Wire shape: packages/schema/src/domains/gateway/GatewaySchemas.ts VoiceState,
// session_ready_collect.erl guild properties, and VoiceStateUpdate.ts handler.
fn state() -> ClientState {
    let mut state = ClientState::default();
    state.apply_gateway("READY", json!({
        "session_id":"session", "user":{"id":"100","username":"me","avatar":"my-avatar"},
        "users":[{"id":"101","username":"alex","global_name":"Alex","avatar":"global-avatar"}],
        "guilds":[{"id":"1","properties":{"name":"Guild","owner_id":"100"},
            "channels":[{"id":"10","name":"Lounge","type":2},{"id":"11","name":"Other","type":2}],
            "members":[{"user":{"id":"101"},"roles":[],"nick":"Guild Alex","avatar":"guild-avatar","profile_flags":0}],
            "voice_states":[{"user_id":"100","channel_id":"10","connection_id":"self"}]}]
    })).unwrap();
    state
}

fn voice(state: &mut ClientState, update: Value) {
    state.apply_gateway("VOICE_STATE_UPDATE", update).unwrap();
}

#[test]
fn includes_self_and_other_channels_without_local_media_membership() {
    let mut state = state();
    voice(
        &mut state,
        json!({"user_id":"101","guild_id":"1","channel_id":"11","connection_id":"remote"}),
    );
    assert!(state.voice.channel_id.is_none());
    let own = state.voice_participants(10.into());
    assert_eq!(own.len(), 1);
    assert!(own[0].is_current_user);
    assert_eq!(own[0].avatar.as_deref(), Some("my-avatar"));
    assert_eq!(own[0].livekit_identity.as_deref(), Some("user_100_self"));
    let remote = state.voice_participants(11.into());
    assert_eq!(remote[0].display_name, "Guild Alex");
    assert_eq!(remote[0].avatar.as_deref(), Some("global-avatar"));
    assert_eq!(remote[0].guild_avatar.as_deref(), Some("guild-avatar"));
    assert!(!remote[0].is_current_user);
    assert!(state.voice_participants(12.into()).is_empty());
}

#[test]
fn join_move_moderate_and_leave_preserve_other_connections() {
    let mut state = state();
    for connection in ["phone", "desktop"] {
        voice(
            &mut state,
            json!({"user_id":"101","guild_id":"1","channel_id":"10","connection_id":connection}),
        );
    }
    let members = state.voice_participants(10.into());
    assert_eq!(
        members.iter().filter(|m| m.user_id == 101.into()).count(),
        2
    );
    assert!(
        members
            .iter()
            .filter(|m| m.user_id == 101.into())
            .all(|m| m.connection_count == 2)
    );
    voice(
        &mut state,
        json!({"user_id":"101","connection_id":"phone","self_mute":true,"self_deaf":true,
        "mute":true,"deaf":true,"suppress":true,"self_video":true,"self_stream":true}),
    );
    let members = state.voice_participants(10.into());
    let phone = members
        .iter()
        .find(|m| m.connection_id.as_deref() == Some("phone"))
        .unwrap();
    assert!(phone.self_mute && phone.self_deaf && phone.mute && phone.deaf && phone.suppress);
    assert!(phone.self_video && phone.self_stream);
    let desktop = members
        .iter()
        .find(|m| m.connection_id.as_deref() == Some("desktop"))
        .unwrap();
    assert!(!desktop.self_mute && !desktop.mute && !desktop.self_video);
    voice(
        &mut state,
        json!({"user_id":"101","connection_id":"phone","channel_id":"11"}),
    );
    assert_eq!(
        state
            .voice_participants(10.into())
            .iter()
            .find(|m| m.user_id == 101.into())
            .unwrap()
            .connection_count,
        1
    );
    assert_eq!(
        state.voice_participants(11.into())[0]
            .connection_id
            .as_deref(),
        Some("phone")
    );
    voice(
        &mut state,
        json!({"user_id":"101","connection_id":"phone","channel_id":null}),
    );
    assert!(state.voice_participants(11.into()).is_empty());
    assert_eq!(state.voice_participants(10.into()).len(), 2);
    voice(
        &mut state,
        json!({"user_id":"101","connection_id":"desktop","channel_id":null}),
    );
    assert_eq!(state.voice_participants(10.into()).len(), 1);
}

#[test]
fn resolves_names_from_member_relationship_and_private_channel_without_fake_users() {
    let mut state = state();
    state
        .apply_gateway(
            "RELATIONSHIP_ADD",
            json!({"id":"101","type":1,"nickname":"Friend Alex"}),
        )
        .unwrap();
    state
        .apply_gateway(
            "CHANNEL_CREATE",
            json!({"id":"20","type":3,"nicks":{"101":"Group Alex"}}),
        )
        .unwrap();
    voice(
        &mut state,
        json!({"user_id":"101","channel_id":"20","connection_id":"group"}),
    );
    assert_eq!(
        state.voice_participants(20.into())[0].display_name,
        "Group Alex"
    );
    state
        .channels
        .get_mut(&20.into())
        .unwrap()
        .extra
        .remove("nicks");
    assert_eq!(
        state.voice_participants(20.into())[0].display_name,
        "Friend Alex"
    );
    state.relationships.clear();
    assert_eq!(state.voice_participants(20.into())[0].display_name, "Alex");
    voice(
        &mut state,
        json!({"user_id":"101","guild_id":"1","channel_id":"10","connection_id":"guild"}),
    );
    state.users.remove(&101.into());
    assert_eq!(
        state.voice_participants(10.into())[0].display_name,
        "Guild Alex"
    );
    voice(
        &mut state,
        json!({"user_id":"999","channel_id":"20","session_id":"session-only"}),
    );
    let before = state.users.clone();
    let unknown = state
        .voice_participants(20.into())
        .into_iter()
        .find(|m| m.user_id == 999.into())
        .unwrap();
    assert_eq!(unknown.display_name, "999");
    assert!(unknown.connection_id.is_none() && unknown.livekit_identity.is_none());
    assert!(unknown.avatar.is_none() && unknown.guild_avatar.is_none());
    assert_eq!(state.users, before);
}

#[test]
fn avatar_unset_uses_member_profile_flag_and_partial_updates_preserve_hashes() {
    let mut state = state();
    voice(
        &mut state,
        json!({"user_id":"101","guild_id":"1","channel_id":"10","connection_id":"a"}),
    );
    state
        .apply_gateway(
            "GUILD_MEMBER_UPDATE",
            json!({"guild_id":"1","user":{"id":"101"},"profile_flags":1}),
        )
        .unwrap();
    let participant = state
        .voice_participants(10.into())
        .into_iter()
        .find(|p| p.user_id == 101.into())
        .unwrap();
    assert!(participant.avatar_unset);
    assert_eq!(participant.guild_avatar.as_deref(), Some("guild-avatar"));
    assert_eq!(participant.avatar.as_deref(), Some("global-avatar"));
    state
        .apply_gateway(
            "GUILD_MEMBER_UPDATE",
            json!({"guild_id":"1","user":{"id":"101"},"profile_flags":null,"avatar":null}),
        )
        .unwrap();
    let participant = state
        .voice_participants(10.into())
        .into_iter()
        .find(|p| p.user_id == 101.into())
        .unwrap();
    assert!(!participant.avatar_unset);
    assert!(participant.guild_avatar.is_none());
}

#[test]
fn deterministic_numeric_name_order_and_distinct_same_account_identities() {
    let mut state = state();
    for (id, name) in [
        (201, "room10"),
        (202, "ROOM2"),
        (203, "room000000000000000000000000000000000003"),
    ] {
        state.users.insert(
            id.into(),
            User {
                id: id.into(),
                username: name.into(),
                ..User::default()
            },
        );
        voice(
            &mut state,
            json!({"user_id":id.to_string(),"channel_id":"11","connection_id":"a"}),
        );
    }
    assert_eq!(
        state
            .voice_participants(11.into())
            .iter()
            .map(|p| p.user_id)
            .collect::<Vec<_>>(),
        vec![202.into(), 203.into(), 201.into()]
    );
    voice(
        &mut state,
        json!({"user_id":"100","channel_id":"10","connection_id":"phone"}),
    );
    let own = state.voice_participants(10.into());
    assert_eq!(own.len(), 2);
    assert!(
        own.iter()
            .all(|p| p.is_current_user && p.connection_count == 2)
    );
    assert_ne!(own[0].livekit_identity, own[1].livekit_identity);
}
