use fluxer_core::*;
use serde_json::json;

fn ready() -> serde_json::Value {
    json!({"session_id":"session", "user":{"id":"100","username":"you"},
        "guilds":[{"id":"1", "properties":{"name":"Community","owner_id":"101"},
            "channels":[{"id":"11","name":"general","type":0,"last_message_id":"1000"},
                {"id":"14","name":"Lounge","type":2}],
            "roles":[{"id":"1","permissions":"3072"}],
            "members":[{"user":{"id":"100","username":"you"},"roles":[]}],
            "voice_states":[{"user_id":"101","channel_id":"14","connection_id":"a"}]}],
        "private_channels":[{"id":"40","name":null,"type":1,"recipients":[{"id":"101","username":"Alex"}]}],
        "read_states":[{"id":"11","last_message_id":"999","mention_count":1}]})
}
fn message(id: u64) -> serde_json::Value {
    json!({"id":id.to_string(),"channel_id":"11","author":{"id":"101","username":"Alex"},
        "content":"Hello!", "timestamp":"2026-11-12T00:00:00Z", "mentions":[{"id":"100","username":"you"}]})
}
#[test]
fn ready_uses_gateway_properties_and_preserves_local_drafts() {
    let mut state = ClientState::default();
    state.set_draft(11.into(), "draft".into());
    state.apply_gateway("READY", ready()).unwrap();
    assert_eq!(state.guilds[&1.into()].name, "Community");
    assert_eq!(state.channels[&11.into()].guild_id, Some(1.into()));
    assert_eq!(state.channels[&40.into()].display_name(), "Alex");
    assert_eq!(state.members.len(), 1);
    assert_eq!(state.voice_states.len(), 1);
    assert!(state.is_unread(11.into()));
    assert!(state.select_channel(11.into()));
    assert!(!state.is_unread(11.into()));
    assert_eq!(state.drafts[&11.into()], "draft");
    assert_eq!(
        state.permissions_for(11.into(), 100.into()),
        Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES
    );
}
#[test]
fn failed_ready_is_atomic_and_cross_account_ready_is_rejected() {
    let mut state = ClientState::demo();
    let mut data = ready();
    data["guilds"][0]["channels"][0]["id"] = json!(42);
    assert!(state.apply_gateway("READY", data).is_err());
    assert_eq!(state.guilds[&1.into()].name, "Fluxer Community");
    let mut data = ready();
    data["user"]["id"] = json!("200");
    assert!(matches!(
        state.apply_gateway("READY", data),
        Err(StateError::AccountMismatch)
    ));
    assert_eq!(state.current_user.as_ref().unwrap().id, 100.into());
}
#[test]
fn message_create_patch_reactions_and_bulk_delete() {
    let mut state = ClientState::default();
    state.apply_gateway("READY", ready()).unwrap();
    state
        .apply_gateway("MESSAGE_CREATE", message(1001))
        .unwrap();
    state
        .apply_gateway("MESSAGE_CREATE", message(1002))
        .unwrap();
    state
        .apply_gateway("MESSAGE_CREATE", message(1001))
        .unwrap();
    assert_eq!(state.read_states[&11.into()].mention_count, 3); // initial 1 + two distinct messages
    state
        .apply_gateway(
            "MESSAGE_UPDATE",
            json!({"id":"1001","channel_id":"11","content":"Edited", "pinned":true}),
        )
        .unwrap();
    let first = &state.messages[&11.into()][&1001.into()];
    assert_eq!(first.content, "Edited");
    assert!(first.pinned);
    assert_eq!(first.author.username, "Alex");
    let mut reaction = json!({"channel_id":"11","message_id":"1001","user_id":"100","emoji":{"name":"👍","id":null}});
    state
        .apply_gateway("MESSAGE_REACTION_ADD", reaction.clone())
        .unwrap();
    reaction["user_id"] = json!("101");
    state
        .apply_gateway("MESSAGE_REACTION_ADD", reaction.clone())
        .unwrap();
    assert_eq!(
        state.messages[&11.into()][&1001.into()].reactions[0].count,
        2
    );
    state
        .apply_gateway("MESSAGE_REACTION_REMOVE", reaction)
        .unwrap();
    assert!(state.messages[&11.into()][&1001.into()].reactions[0].me);
    state
        .apply_gateway(
            "MESSAGE_DELETE_BULK",
            json!({"channel_id":"11","ids":["1001","1002"]}),
        )
        .unwrap();
    assert_eq!(state.channel_messages(11.into()).count(), 0);
}
#[test]
fn malformed_partial_update_does_not_replace_valid_message() {
    let mut state = ClientState::demo();
    assert!(
        state
            .apply_gateway(
                "MESSAGE_UPDATE",
                json!({"id":"1000","channel_id":"11","content":null})
            )
            .is_err()
    );
    assert!(!state.messages[&11.into()][&1000.into()].content.is_empty());
}
#[test]
fn history_does_not_increment_mentions_and_acks_never_regress() {
    let mut state = ClientState::default();
    state.apply_gateway("READY", ready()).unwrap();
    state.load_history(
        11.into(),
        vec![serde_json::from_value(message(1000)).unwrap()],
    );
    assert_eq!(state.read_states[&11.into()].mention_count, 1);
    state
        .apply_gateway(
            "MESSAGE_ACK",
            json!({"channel_id":"11","message_id":"1001","mention_count":0}),
        )
        .unwrap();
    state
        .apply_gateway(
            "MESSAGE_ACK",
            json!({"channel_id":"11","message_id":"999","mention_count":8}),
        )
        .unwrap();
    assert_eq!(
        state.read_states[&11.into()].last_message_id,
        Some(1001.into())
    );
    assert_eq!(state.read_states[&11.into()].mention_count, 0);
}
#[test]
fn voice_partials_preserve_state_and_multiple_connections_disconnect_independently() {
    let mut state = ClientState::default();
    state.apply_gateway("READY", ready()).unwrap();
    state
        .apply_gateway(
            "VOICE_STATE_UPDATE",
            json!({"user_id":"101","channel_id":"14","connection_id":"b","self_video":true}),
        )
        .unwrap();
    state
        .apply_gateway(
            "VOICE_STATE_UPDATE",
            json!({"user_id":"101","connection_id":"a","self_mute":true}),
        )
        .unwrap();
    assert_eq!(state.voice_states.len(), 2);
    assert_eq!(
        state.voice_states[&(101.into(), "a".into())].channel_id,
        Some(14.into())
    );
    assert!(state.voice_states[&(101.into(), "a".into())].self_mute);
    assert!(state.voice_states[&(101.into(), "b".into())].self_video);
    state
        .apply_gateway(
            "VOICE_STATE_UPDATE",
            json!({"user_id":"101","connection_id":"a","channel_id":null}),
        )
        .unwrap();
    assert_eq!(state.voice_states.len(), 1);
    assert!(state.voice_states.contains_key(&(101.into(), "b".into())));
}
#[test]
fn deafen_forces_mute_and_restores_prior_choice() {
    let mut controls = VoiceControls::default();
    controls.set_deafened(true);
    assert!(controls.muted && controls.deafened);
    controls.set_deafened(false);
    assert!(!controls.muted);
    controls.set_muted(true);
    controls.set_deafened(true);
    controls.set_deafened(false);
    assert!(controls.muted);
    controls.join(14.into());
    controls.camera = true;
    controls.screen_share = true;
    controls.disconnect();
    assert!(controls.channel_id.is_none());
    assert!(!controls.camera && !controls.screen_share);
}
#[test]
fn guild_deletion_clears_channel_scoped_state_and_availability_is_transient() {
    let mut state = ClientState::demo();
    state.set_draft(11.into(), "unsent".into());
    state.voice.join(14.into());
    state
        .apply_gateway("GUILD_DELETE", json!({"id":"1","unavailable":true}))
        .unwrap();
    assert!(state.guilds[&1.into()].unavailable);
    assert!(state.channels.contains_key(&11.into()));
    state
        .apply_gateway("GUILD_DELETE", json!({"id":"1"}))
        .unwrap();
    assert!(!state.channels.contains_key(&11.into()));
    assert!(!state.drafts.contains_key(&11.into()));
    assert!(!state.messages.contains_key(&11.into()));
    assert!(state.selected_channel.is_none());
    assert!(state.voice.channel_id.is_none());
    assert!(state.channels.contains_key(&40.into()));
}
#[test]
fn unknown_channels_and_events_do_not_break_session() {
    let mut state = ClientState::default();
    assert_eq!(
        state.apply_gateway("FUTURE_EVENT", json!({})).unwrap(),
        ApplyOutcome::Ignored
    );
    state
        .apply_gateway(
            "CHANNEL_CREATE",
            json!({"id":"50","type":500,"name":"future"}),
        )
        .unwrap();
    assert_eq!(state.channels[&50.into()].kind, ChannelType::Unknown(500));
    assert!(!state.select_channel(999.into()));
}
#[test]
fn typing_expires_at_deadline() {
    let mut state = ClientState::default();
    state
        .apply_gateway(
            "TYPING_START",
            json!({"channel_id":"11","user_id":"101","timestamp":10}),
        )
        .unwrap();
    state.expire_typing(19_999);
    assert_eq!(state.typing[&11.into()].len(), 1);
    state.expire_typing(20_000);
    assert!(state.typing.is_empty());
}
