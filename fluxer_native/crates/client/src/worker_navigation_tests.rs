use super::*;

fn demo() -> (Worker, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let mut worker = Worker::new(tx, Arc::new(Mutex::new(BTreeMap::new())));
    worker.demo = true;
    worker.state = ClientState::demo();
    worker.state.selected_channel = Some(11.into());
    worker.state.read_states.insert(
        11.into(),
        fluxer_core::ReadState {
            id: 11.into(),
            last_message_id: Some(10.into()),
            mention_count: 3,
            ..Default::default()
        },
    );
    (worker, rx)
}
fn messages(start: u64, end: u64) -> Vec<Message> {
    let user = User {
        id: 100.into(),
        username: "fixture".into(),
        ..Default::default()
    };
    (start..=end)
        .map(|id| Message::text(id.into(), 11.into(), user.clone(), format!("message {id}")))
        .collect()
}
fn history(ids: impl IntoIterator<Item = u64>, latest: bool) -> HistorySnapshot {
    HistorySnapshot {
        channel_id: 11.into(),
        message_ids: ids.into_iter().map(Into::into).collect(),
        target: None,
        loading: false,
        has_older: true,
        has_newer: !latest,
        latest,
        error: None,
    }
}
fn install_history(worker: &mut Worker, snapshot: HistorySnapshot) -> u64 {
    let token = worker.navigation.token();
    worker.navigation.history.insert(
        snapshot.channel_id,
        HistoryJob {
            token,
            snapshot,
            task: None,
            arrived: BTreeSet::new(),
        },
    );
    token
}
fn install_search(worker: &mut Worker, channel: Snowflake) -> u64 {
    let token = worker.navigation.token();
    let snapshot = SearchSnapshot {
        channel_id: channel,
        query: "fixture".into(),
        scope: SearchScope::Current,
        sort: SearchSort::Newest,
        status: SearchStatus::Loading,
        messages: Vec::new(),
        channels: Vec::new(),
        total: 0,
        page: 1,
        hits_per_page: 25,
        poll_count: 0,
        http_status: None,
        error: None,
    };
    worker.navigation.searches.insert(
        channel,
        SearchJob {
            token,
            snapshot,
            _task: Task(tokio::spawn(pending())),
        },
    );
    token
}
async fn finish_history(worker: &mut Worker) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = worker.navigation.rx.recv().await.unwrap();
            let done = matches!(event, NavigationEvent::History { .. });
            worker.navigation_event(event);
            if done {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[test]
fn around_history_matches_source_balanced_window_and_known_latest_rules() {
    let mut window = history([], true);
    apply_history_page(
        &mut window,
        HistoryRequest::Around(275.into()),
        &messages(251, 300),
        Some(999.into()),
    );
    assert!(window.has_older && window.has_newer && !window.latest);
    assert_eq!(window.message_ids.len(), 50);
    apply_history_page(
        &mut window,
        HistoryRequest::Around(275.into()),
        &messages(251, 300),
        Some(300.into()),
    );
    assert!(window.has_older && !window.has_newer && window.latest);
    apply_history_page(
        &mut window,
        HistoryRequest::Around(275.into()),
        &messages(270, 280),
        Some(999.into()),
    );
    assert!(!window.has_older && !window.has_newer);
    apply_history_page(
        &mut window,
        HistoryRequest::Around(275.into()),
        &[],
        Some(999.into()),
    );
    assert!(!window.has_older && !window.has_newer && !window.latest);
    assert!(
        window
            .error
            .as_ref()
            .unwrap()
            .contains("no longer available")
    );
}

#[test]
fn older_and_newer_paging_deduplicate_and_expose_the_trimmed_side() {
    let mut window = history(100..300, true);
    apply_history_page(
        &mut window,
        HistoryRequest::Before(100.into()),
        &messages(50, 99),
        Some(299.into()),
    );
    assert_eq!(window.message_ids.first(), Some(&Snowflake(50)));
    assert_eq!(window.message_ids.last(), Some(&Snowflake(249)));
    assert!(window.has_older && window.has_newer && !window.latest);
    apply_history_page(
        &mut window,
        HistoryRequest::After(249.into()),
        &messages(245, 299),
        Some(299.into()),
    );
    assert_eq!(window.message_ids.first(), Some(&Snowflake(100)));
    assert_eq!(window.message_ids.last(), Some(&Snowflake(299)));
    assert_eq!(window.message_ids.len(), 200);
    assert!(window.has_older && !window.has_newer && window.latest);
    let unique = window.message_ids.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(unique.len(), 200);
}

#[tokio::test]
async fn latest_replaces_historical_window_without_merging_disjoint_cached_messages_or_ack() {
    let (mut worker, _) = demo();
    worker.state.load_history(11.into(), messages(1, 50));
    worker.state.load_history(11.into(), messages(10000, 10001));
    let mut snapshot = history(1..51, false);
    snapshot.loading = true;
    let token = install_history(&mut worker, snapshot);
    worker.navigation_event(NavigationEvent::History {
        epoch: worker.navigation.epoch,
        channel: 11.into(),
        token,
        request: HistoryRequest::Latest,
        result: Ok(serde_json::to_value(messages(950, 999)).unwrap()),
    });
    let view = &worker.navigation.history[&Snowflake(11)].snapshot;
    assert_eq!(
        view.message_ids,
        (950..1000).map(Snowflake).collect::<Vec<_>>()
    );
    assert!(
        worker.state.messages[&Snowflake(11)].contains_key(&Snowflake(10000)),
        "cache is retained but not displayed across a gap"
    );
    assert_eq!(
        worker.state.read_states[&Snowflake(11)].last_message_id,
        Some(10.into())
    );
    assert!(worker.navigation.acks.is_empty());
}

#[tokio::test]
async fn live_delivery_during_latest_fetch_keeps_only_newly_arrived_tail_and_historical_delivery_is_buffered()
 {
    let (mut worker, _) = demo();
    let mut snapshot = history(1..51, false);
    snapshot.loading = true;
    let token = install_history(&mut worker, snapshot);
    worker.state.insert_message(messages(1001, 1001).remove(0));
    worker.navigation_message_inserted(11.into(), 1001.into());
    worker.navigation_event(NavigationEvent::History {
        epoch: worker.navigation.epoch,
        channel: 11.into(),
        token,
        request: HistoryRequest::Latest,
        result: Ok(serde_json::to_value(messages(951, 1000)).unwrap()),
    });
    let view = &worker.navigation.history[&Snowflake(11)].snapshot;
    assert_eq!(view.message_ids.last(), Some(&Snowflake(1001)));
    assert!(view.latest);
    install_history(&mut worker, history(20..70, false));
    worker.navigation_message_inserted(11.into(), 1002.into());
    let view = &worker.navigation.history[&Snowflake(11)].snapshot;
    assert!(!view.message_ids.contains(&Snowflake(1002)));
    assert!(view.has_newer && !view.latest);
    assert_eq!(
        worker.state.read_states[&Snowflake(11)].last_message_id,
        Some(10.into())
    );
}

#[tokio::test]
async fn stale_history_is_rejected_after_new_jump_context_or_account_reset() {
    let (mut worker, _) = demo();
    let first = install_history(&mut worker, history([100], false));
    let latest = install_history(&mut worker, history([200], false));
    worker.navigation_event(NavigationEvent::History {
        epoch: worker.navigation.epoch,
        channel: 11.into(),
        token: first,
        request: HistoryRequest::Latest,
        result: Ok(serde_json::to_value(messages(900, 999)).unwrap()),
    });
    assert_eq!(
        worker.navigation.history[&Snowflake(11)]
            .snapshot
            .message_ids,
        vec![Snowflake(200)]
    );
    worker.state.selected_channel = Some(13.into());
    worker.navigation_context(13.into());
    worker.navigation_event(NavigationEvent::History {
        epoch: worker.navigation.epoch,
        channel: 11.into(),
        token: latest,
        request: HistoryRequest::Latest,
        result: Ok(serde_json::to_value(messages(900, 999)).unwrap()),
    });
    assert!(!worker.state.messages[&Snowflake(11)].contains_key(&Snowflake(999)));
    let epoch = worker.navigation.epoch;
    worker.clear_navigation();
    worker.navigation_event(NavigationEvent::History {
        epoch,
        channel: 11.into(),
        token: latest,
        request: HistoryRequest::Latest,
        result: Ok(serde_json::to_value(messages(900, 999)).unwrap()),
    });
    assert!(worker.navigation.history.is_empty());
}

#[tokio::test]
async fn detached_search_hydrates_users_and_active_threads_without_timeline_or_read_side_effects() {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    let token = install_search(&mut worker, channel);
    worker.state.thread_guilds.insert(1.into());
    let member =
        json!({"id":"88","user_id":"100","join_timestamp":"2026-01-01T00:00:00Z","flags":0});
    let body = json!({"messages":[{"id":"77","channel_id":"88","author":{"id":"100"},"content":"detached match"}],"channels":[{"id":"90","guild_id":"1","type":0,"name":"found"}],"threads":[{"id":"88","guild_id":"1","parent_id":"11","type":11,"name":"active"},{"id":"89","guild_id":"2","parent_id":"21","type":11,"name":"inactive"}],"members":[member],"total":1});
    worker.navigation_event(NavigationEvent::Search {
        epoch: worker.navigation.epoch,
        channel,
        token,
        poll_count: 2,
        result: Ok(SearchResponse {
            status: 200,
            body,
            retry_after: None,
        }),
    });
    let snapshot = &worker.navigation.searches[&channel].snapshot;
    assert_eq!(snapshot.messages[0].author.username, "you");
    assert_eq!(snapshot.total, 1);
    assert_eq!(snapshot.page, 1);
    assert_eq!(
        worker.state.channels[&Snowflake(88)].extra["member"]["user_id"],
        "100"
    );
    assert!(!worker.state.channels.contains_key(&Snowflake(89)));
    assert!(!worker.state.channels.contains_key(&Snowflake(90)));
    assert!(!worker.state.messages.contains_key(&Snowflake(88)));
    assert_eq!(
        worker.state.read_states[&channel].last_message_id,
        Some(10.into())
    );
    worker.close_search(channel);
    assert!(
        worker
            .navigation
            .result_channels
            .contains_key(&Snowflake(90)),
        "known detached metadata permits another explicit jump after closing/replacing the result context"
    );
}

#[tokio::test]
async fn old_search_results_and_polls_cannot_replace_new_query_or_cross_accounts() {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    let old = install_search(&mut worker, channel);
    let token = install_search(&mut worker, channel);
    let event = |epoch, token| NavigationEvent::Search {
        epoch,
        channel,
        token,
        poll_count: 0,
        result: Ok(SearchResponse {
            status: 202,
            body: json!({"indexing":true}),
            retry_after: None,
        }),
    };
    worker.navigation_event(event(worker.navigation.epoch, old));
    assert_eq!(
        worker.navigation.searches[&channel].snapshot.status,
        SearchStatus::Loading
    );
    worker.navigation_event(event(worker.navigation.epoch, token));
    assert_eq!(
        worker.navigation.searches[&channel].snapshot.status,
        SearchStatus::Indexing
    );
    assert_eq!(
        worker.navigation.searches[&channel].snapshot.http_status,
        Some(202)
    );
    worker.close_search(channel);
    worker.navigation_event(event(worker.navigation.epoch, token));
    assert!(worker.navigation.searches.is_empty());
    let epoch = worker.navigation.epoch;
    worker.clear_navigation();
    worker.navigation_event(event(epoch, token));
    assert!(worker.navigation.searches.is_empty());
}

#[tokio::test]
async fn search_indexing_uses_body_flag_and_empty_body_is_an_error() {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    let token = install_search(&mut worker, channel);
    for (status, body, expected) in [
        (200, json!({"indexing":true}), SearchStatus::Indexing),
        (202, json!({"messages":[],"total":0}), SearchStatus::Ready),
        (200, Value::Null, SearchStatus::Failed),
    ] {
        worker.navigation_event(NavigationEvent::Search {
            epoch: worker.navigation.epoch,
            channel,
            token,
            poll_count: 0,
            result: Ok(SearchResponse {
                status,
                body,
                retry_after: None,
            }),
        });
        assert_eq!(
            worker.navigation.searches[&channel].snapshot.status,
            expected
        );
    }
}

#[tokio::test]
async fn invalid_replacement_query_stops_old_poll_and_scope_precedence_matches_desktop() {
    let (mut worker, events) = demo();
    let channel = Snowflake(11);
    install_search(&mut worker, channel);
    assert!(
        worker
            .search_messages(
                channel,
                String::new(),
                SearchScope::Current,
                None,
                SearchSort::Newest,
                1
            )
            .is_err()
    );
    assert!(worker.navigation.searches.is_empty());
    assert!(events.try_iter().any(|event| matches!(
        event,
        Event::Search(SearchSnapshot {
            status: SearchStatus::Failed,
            ..
        })
    )));
    worker
        .search_messages(
            channel,
            "release scope:all_dms".into(),
            SearchScope::Current,
            None,
            SearchSort::Newest,
            1,
        )
        .unwrap();
    assert_eq!(
        worker.navigation.searches[&channel].snapshot.scope,
        SearchScope::AllDms
    );
    worker
        .search_messages(
            channel,
            "release scope:all_dms".into(),
            SearchScope::Current,
            Some(SearchScope::AllGuilds),
            SearchSort::Newest,
            1,
        )
        .unwrap();
    assert_eq!(
        worker.navigation.searches[&channel].snapshot.scope,
        SearchScope::AllGuilds
    );
    worker.state.selected_channel = Some(40.into());
    worker
        .search_messages(
            40.into(),
            "release scope:all_guilds".into(),
            SearchScope::Current,
            None,
            SearchSort::Newest,
            1,
        )
        .unwrap();
    assert_eq!(
        worker.navigation.searches[&Snowflake(40)].snapshot.scope,
        SearchScope::Current
    );
}

#[tokio::test]
async fn only_painted_window_messages_can_be_acknowledged_and_never_the_unseen_latest() {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    worker
        .state
        .channels
        .get_mut(&channel)
        .unwrap()
        .last_message_id = Some(9999.into());
    install_history(&mut worker, history(100..150, false));
    assert!(
        worker
            .acknowledge_visible_message(channel, 9999.into())
            .is_err()
    );
    worker
        .acknowledge_visible_message(channel, 149.into())
        .unwrap();
    let event = worker.navigation.rx.recv().await.unwrap();
    worker.navigation_event(event);
    assert_eq!(
        worker.state.read_states[&channel].last_message_id,
        Some(149.into())
    );
    assert_eq!(worker.state.read_states[&channel].mention_count, 3);
    assert!(worker.state.is_unread(channel));
    worker.state.selected_channel = Some(13.into());
    assert!(
        worker
            .acknowledge_visible_message(channel, 148.into())
            .is_err()
    );
}

#[tokio::test]
async fn ack_failure_reports_exact_pending_id_without_advancing_read_state_and_stale_completion_is_ignored()
 {
    let (mut worker, events) = demo();
    let channel = Snowflake(11);
    let epoch = worker.navigation.epoch;
    let token = worker.navigation.token();
    worker.navigation.acks.insert(
        channel,
        Mutation {
            token,
            _task: Task(tokio::spawn(pending())),
        },
    );
    worker.navigation_event(NavigationEvent::Ack {
        epoch,
        channel,
        id: 149.into(),
        token,
        result: Err("403 forbidden".into()),
    });
    assert_eq!(
        worker.state.read_states[&channel].last_message_id,
        Some(10.into())
    );
    assert!(worker.navigation.acks.is_empty());
    assert!(events.try_iter().any(|event|matches!(event,Event::MessageAckResult{channel_id,message_id,error:Some(error)}if channel_id==channel&&message_id==Snowflake(149)&&error=="403 forbidden")));
    let next = worker.navigation.token();
    worker.navigation.acks.insert(
        channel,
        Mutation {
            token: next,
            _task: Task(tokio::spawn(pending())),
        },
    );
    worker.navigation_event(NavigationEvent::Ack {
        epoch,
        channel,
        id: 149.into(),
        token,
        result: Ok(()),
    });
    assert_eq!(
        worker.state.read_states[&channel].last_message_id,
        Some(10.into())
    );
    assert_eq!(worker.navigation.acks[&channel].token, next);
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, Event::MessageAckResult { .. }))
    );
}

#[tokio::test]
async fn pins_open_ack_uses_pin_timestamp_guard_and_never_marks_message_history_read() {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    worker
        .state
        .channels
        .get_mut(&channel)
        .unwrap()
        .last_pin_timestamp = Some("2026-11-12T12:00:00Z".into());
    worker.load_pins(channel, false).unwrap();
    assert!(!worker.state.has_unread_pins(channel));
    let token = worker.navigation.pin_acks[&channel].token;
    worker.load_pins(channel, false).unwrap();
    assert_eq!(worker.navigation.pin_acks[&channel].token, token);
    worker.close_pins(channel);
    worker
        .state
        .channels
        .get_mut(&channel)
        .unwrap()
        .last_pin_timestamp = Some("2026-11-12T13:00:00Z".into());
    assert!(worker.state.has_unread_pins(channel));
    worker.load_pins(channel, false).unwrap();
    assert_ne!(worker.navigation.pin_acks[&channel].token, token);
    assert_eq!(
        worker.state.read_states[&channel].last_message_id,
        Some(10.into())
    );
    assert_eq!(worker.state.read_states[&channel].mention_count, 3);
    assert!(worker.navigation.acks.is_empty());
}

#[tokio::test]
async fn pin_permissions_allow_read_only_channel_with_pin_permission_and_honor_disabled_operation()
{
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    let user = worker.state.current_user.as_ref().unwrap().id;
    worker.state.guilds.get_mut(&Snowflake(1)).unwrap().owner_id = 999.into();
    worker.state.members.insert(
        (1.into(), user),
        fluxer_core::Member {
            user: worker.state.current_user.clone().unwrap(),
            ..Default::default()
        },
    );
    worker.state.guilds.get_mut(&Snowflake(1)).unwrap().roles = vec![fluxer_core::Role {
        id: 1.into(),
        permissions: Permissions::VIEW_CHANNEL
            | Permissions::READ_MESSAGE_HISTORY
            | Permissions::PIN_MESSAGES,
        ..Default::default()
    }];
    assert!(
        !worker
            .state
            .permissions_for(channel, user)
            .contains(Permissions::SEND_MESSAGES)
    );
    assert!(worker.can_pin(channel));
    worker
        .state
        .guilds
        .get_mut(&Snowflake(1))
        .unwrap()
        .extra
        .insert("disabled_operations".into(), json!(16));
    assert!(!worker.can_pin(channel));
}

#[tokio::test]
async fn jump_subscribes_new_guild_without_ack_and_preserves_draft_attachments_and_search_metadata()
{
    let (mut worker, events) = demo();
    let source = Snowflake(11);
    let target = Snowflake(21);
    worker.state.set_draft(source, "unfinished source".into());
    worker
        .state
        .set_draft(target, "unfinished destination".into());
    worker
        .stage_clipboard(
            source,
            "fixture.bin".into(),
            "application/octet-stream".into(),
            Arc::new(vec![1]),
        )
        .unwrap();
    worker.demo = false;
    worker.gateway_ready = true;
    worker.state.session_id = Some("fixture-session".into());
    worker.client = Some(
        Client::new(
            Instance::from_endpoints("http://127.0.0.1:1/api", "ws://127.0.0.1:1/gateway").unwrap(),
        )
        .unwrap(),
    );
    let (tx, mut rx) = async_mpsc::channel(16);
    worker.gateway_commands = Some(tx);
    worker.selected_guild = Some(1.into());
    worker.jump_to_message(target, 777.into()).await.unwrap();
    assert_eq!(worker.state.selected_channel, Some(target));
    assert_eq!(worker.selected_guild, Some(2.into()));
    assert!(
        matches!(rx.try_recv().unwrap(),GatewayCommand::Send{op:14,data}if data==json!({"subscriptions":{"2":{"active":true,"sync":true}}}))
    );
    assert_eq!(worker.state.drafts[&source], "unfinished source");
    assert_eq!(worker.state.drafts[&target], "unfinished destination");
    worker.attachment_snapshot(source);
    assert!(events.try_iter().any(|event|matches!(event,Event::Attachments{channel_id,attachments}if channel_id==source&&!attachments.is_empty())));
    assert!(worker.navigation.acks.is_empty());
    worker.clear_navigation();
    worker.clear_attachments();
}

#[tokio::test]
async fn demo_archive_search_then_jump_and_latest_exercise_real_commands_without_capture() {
    let (mut worker, _) = demo();
    worker.seed_demo_search().unwrap();
    finish_history(&mut worker).await;
    worker
        .search_messages(
            11.into(),
            "release".into(),
            SearchScope::Current,
            None,
            SearchSort::Newest,
            1,
        )
        .unwrap();
    loop {
        let event = worker.navigation.rx.recv().await.unwrap();
        let done = matches!(event, NavigationEvent::Search { .. });
        worker.navigation_event(event);
        if done {
            break;
        }
    }
    let snapshot = &worker.navigation.searches[&Snowflake(11)].snapshot;
    assert_eq!(snapshot.total, 120);
    assert_eq!(snapshot.messages.len(), 25);
    assert_eq!(snapshot.status, SearchStatus::Ready);
    worker
        .jump_to_message(11.into(), 1042.into())
        .await
        .unwrap();
    finish_history(&mut worker).await;
    assert!(
        worker.navigation.history[&Snowflake(11)]
            .snapshot
            .message_ids
            .contains(&Snowflake(1042))
    );
    assert!(worker.navigation.history[&Snowflake(11)].snapshot.has_newer);
    worker
        .request_history(11.into(), HistoryRequest::Latest)
        .unwrap();
    finish_history(&mut worker).await;
    assert_eq!(
        worker.navigation.history[&Snowflake(11)]
            .snapshot
            .message_ids
            .first(),
        Some(&Snowflake(1070))
    );
    assert!(worker.media.is_none());
}
