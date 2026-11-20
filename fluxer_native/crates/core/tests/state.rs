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

#[test]
fn actual_ready_user_reference_shape_hydrates_before_typed_deserialization() {
    let mut state = ClientState::default();
    let raw = serde_json::from_str(include_str!("fixtures/ready_user_refs.json")).unwrap();
    state.apply_gateway("READY", raw).unwrap();
    assert_eq!(state.guilds[&1.into()].name, "Reference Community");
    assert!(state.guilds[&2.into()].unavailable);
    assert_eq!(
        state.members[&(1.into(), 100.into())].user.display_name(),
        "Private Self"
    );
    assert_eq!(state.members[&(1.into(), 101.into())].user.username, "alex");
    assert_eq!(
        state.users[&100.into()].extra["email"],
        "fixture@example.test"
    );
    assert_eq!(
        state.users[&100.into()].global_name.as_deref(),
        Some("Private Self")
    );
    assert_eq!(state.channels[&40.into()].display_name(), "Alex");
    assert_eq!(state.channels[&41.into()].display_name(), "Alex, River");
    assert_eq!(
        state.relationships[&101.into()]
            .user
            .as_ref()
            .unwrap()
            .username,
        "alex"
    );
    assert_eq!(state.presences[&101.into()].status, PresenceStatus::Online);
    assert_eq!(
        state.voice_states[&(101.into(), "connection-a".into())].channel_id,
        Some(14.into())
    );
    assert!(
        state
            .permissions_for(11.into(), 100.into())
            .contains(Permissions::SEND_MESSAGES)
    );
}

#[test]
fn unresolved_ready_reference_is_atomic_and_does_not_fabricate_username() {
    let mut state = ClientState::demo();
    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/ready_user_refs.json")).unwrap();
    raw["guilds"][0]["members"][0]["user"]["id"] = json!("999");
    assert!(state.apply_gateway("READY", raw).is_err());
    assert_eq!(state.guilds[&1.into()].name, "Fluxer Community");
    assert!(!state.users.contains_key(&999.into()));
}

#[test]
fn offline_presence_accepts_cached_id_only_user_and_matches_desktop_status_normalization() {
    let mut state = ClientState::demo();
    state.apply_gateway("PRESENCE_UPDATE", json!({"user":{"id":"101"}, "status":"offline", "mobile":false, "afk":false, "custom_status":null})).unwrap();
    assert_eq!(state.presences[&101.into()].status, PresenceStatus::Offline);
    assert_eq!(state.presences[&101.into()].user.display_name(), "Alex");
    state
        .apply_gateway(
            "PRESENCE_UPDATE",
            json!({"user":{"id":"101"}, "status":null}),
        )
        .unwrap();
    assert_eq!(state.presences[&101.into()].status, PresenceStatus::Offline);
}

#[test]
fn debounced_reaction_add_many_matches_single_adds_and_ordinary_remove() {
    let mut state = ClientState::demo();
    let mut reference = state.clone();
    let reactions = json!([
        {"user_id":"100","emoji":{"id":null,"name":"🔥"}},
        {"user_id":"101","emoji":{"id":null,"name":"🔥"}},
        {"user_id":"102","emoji":{"id":"500","name":"custom"},"member":{"user":{"id":"102"},"roles":[]}}
    ]);
    state
        .apply_gateway(
            "MESSAGE_REACTION_ADD_MANY",
            json!({"guild_id":"1","channel_id":"11","message_id":"1000","reactions":reactions}),
        )
        .unwrap();
    for reaction in reactions.as_array().unwrap() {
        let mut single = reaction.clone();
        single["channel_id"] = json!("11");
        single["message_id"] = json!("1000");
        single["guild_id"] = json!("1");
        reference
            .apply_gateway("MESSAGE_REACTION_ADD", single)
            .unwrap();
    }
    assert_eq!(
        state.messages[&11.into()][&1000.into()].reactions,
        reference.messages[&11.into()][&1000.into()].reactions
    );
    let fire = state.messages[&11.into()][&1000.into()]
        .reactions
        .iter()
        .find(|r| r.emoji.name == "🔥")
        .unwrap();
    assert_eq!(fire.count, 2);
    assert!(fire.me);
    state.apply_gateway("MESSAGE_REACTION_REMOVE", json!({"user_id":"100","channel_id":"11","message_id":"1000","emoji":{"id":null,"name":"🔥"}})).unwrap();
    let fire = state.messages[&11.into()][&1000.into()]
        .reactions
        .iter()
        .find(|r| r.emoji.name == "🔥")
        .unwrap();
    assert_eq!(fire.count, 1);
    assert!(!fire.me);
}

#[test]
fn malformed_reaction_batch_does_not_partially_apply() {
    let mut state = ClientState::demo();
    let before = state.messages[&11.into()][&1000.into()].reactions.clone();
    assert!(
        state
            .apply_gateway(
                "MESSAGE_REACTION_ADD_MANY",
                json!({"channel_id":"11","message_id":"1000","reactions":[
                    {"user_id":"101","emoji":{"name":"🔥"}}, {"user_id":102,"emoji":{"name":"🔥"}}
                ]})
            )
            .is_err()
    );
    assert_eq!(state.messages[&11.into()][&1000.into()].reactions, before);
}

#[test]
fn guild_sync_bulk_channel_and_role_updates_merge_existing_rows() {
    let mut state = ClientState::demo();
    state.apply_gateway("GUILD_SYNC", json!({"id":"1","properties":{"name":"Synced","owner_id":"101"},"channels":[{"id":"50","name":"synced-channel","type":0}],"members":[{"user":{"id":"100"},"roles":[]}],"roles":[{"id":"1","name":"@everyone","permissions":"3072"}]})).unwrap();
    assert_eq!(state.guilds[&1.into()].name, "Synced");
    assert_eq!(state.channels[&50.into()].guild_id, Some(1.into()));
    state.apply_gateway("CHANNEL_UPDATE_BULK", json!({"channels":[{"id":"11","type":0,"position":20},{"id":"50","type":0,"topic":"changed"}]})).unwrap();
    assert_eq!(state.channels[&11.into()].position, 20);
    assert_eq!(state.channels[&11.into()].name, "general");
    assert_eq!(state.channels[&50.into()].topic.as_deref(), Some("changed"));
    state.apply_gateway("GUILD_ROLE_UPDATE_BULK", json!({"guild_id":"1","roles":[{"id":"1","name":"@everyone","permissions":"1024"},{"id":"55","name":"role","permissions":"2048"}]})).unwrap();
    assert_eq!(state.guilds[&1.into()].roles.len(), 2);
    assert!(
        !state
            .permissions_for(11.into(), 100.into())
            .contains(Permissions::SEND_MESSAGES)
    );
}

#[test]
fn new_voice_member_hydrates_from_exact_dispatch_member_field() {
    let mut state = ClientState::demo();
    state.apply_gateway("VOICE_STATE_UPDATE", json!({"user_id":"500","guild_id":"1","channel_id":"14","connection_id":"fresh-connection","member":{"user":{"id":"500","username":"fresh"},"roles":[]}})).unwrap();
    assert_eq!(state.users[&500.into()].username, "fresh");
    assert!(state.members.contains_key(&(1.into(), 500.into())));
    state.apply_gateway("VOICE_STATE_UPDATE", json!({"user_id":"500","guild_id":"1","channel_id":null,"connection_id":"fresh-connection"})).unwrap();
    assert!(
        !state
            .voice_states
            .contains_key(&(500.into(), "fresh-connection".into()))
    );
}

#[test]
fn thread_list_sync_replaces_only_active_threads_in_scope_and_routes_memberships() {
    let mut state = ClientState::demo();
    state.thread_guilds.insert(1.into());
    for thread in [
        json!({"id":"90","guild_id":"1","parent_id":"11","type":11,"name":"missing"}),
        json!({"id":"91","guild_id":"1","parent_id":"11","type":11,"name":"archived","thread_metadata":{"archived":true}}),
        json!({"id":"92","guild_id":"1","parent_id":"12","type":11,"name":"outside parent"}),
        json!({"id":"93","guild_id":"2","parent_id":"11","type":11,"name":"outside guild"}),
    ] {
        state.apply_gateway("THREAD_CREATE", thread).unwrap();
    }
    state
        .apply_gateway(
            "THREAD_LIST_SYNC",
            json!({
                "guild_id":"1",
                "channel_ids":["11"],
                "threads":[{"id":"94","parent_id":"11","type":12,"name":"joined"}],
                "members":[{"id":"94","user_id":"100","flags":1}]
            }),
        )
        .unwrap();
    assert!(!state.channels.contains_key(&90.into()));
    assert!(state.channels[&91.into()].thread_archived());
    assert!(state.channels.contains_key(&92.into()));
    assert!(state.channels.contains_key(&93.into()));
    let thread = &state.channels[&94.into()];
    assert_eq!(thread.guild_id, Some(1.into()));
    assert_eq!(thread.extra["member"]["user_id"], "100");
    state
        .apply_gateway(
            "THREAD_UPDATE",
            json!({"id":"94","thread_metadata":{"archived":true}}),
        )
        .unwrap();
    assert!(state.channels[&94.into()].thread_archived());
    assert!(state.select_channel(94.into()));
    state
        .apply_gateway("CHANNEL_DELETE", json!({"id":"11"}))
        .unwrap();
    assert!(!state.channels.contains_key(&94.into()));
    assert!(state.selected_channel.is_none());
}
