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
async fn detached_search_can_page_and_sort_after_cross_channel_jump_without_replacing_timeline() {
    let (mut worker, _) = demo();
    worker.seed_demo_search().unwrap();
    finish_history(&mut worker).await;
    let channel = Snowflake(21);
    worker.state.selected_channel = Some(channel);
    worker.navigation_context(channel);
    let mut view = history([900, 901], true);
    view.channel_id = channel;
    install_history(&mut worker, view);
    worker
        .search_messages(
            11.into(),
            "release".into(),
            SearchScope::Current,
            None,
            SearchSort::Oldest,
            2,
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
    let search = &worker.navigation.searches[&Snowflake(11)].snapshot;
    assert_eq!(search.status, SearchStatus::Ready);
    assert_eq!(search.page, 2);
    assert_eq!(search.messages.first().unwrap().id, Snowflake(1025));
    assert_eq!(worker.state.selected_channel, Some(channel));
    assert_eq!(
        worker.navigation.history[&channel].snapshot.message_ids,
        vec![Snowflake(900), Snowflake(901)]
    );
    assert!(worker.navigation.acks.is_empty());
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
    assert!(
        worker
            .acknowledge_visible_message(channel, 149.into())
            .is_err(),
        "a historical window is not an automatic-ack context"
    );
    worker
        .navigation
        .history
        .get_mut(&channel)
        .unwrap()
        .snapshot
        .latest = true;
    worker
        .navigation
        .history
        .get_mut(&channel)
        .unwrap()
        .snapshot
        .has_newer = false;
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
        AckJob {
            token,
            message_id: 149.into(),
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
        AckJob {
            token: next,
            message_id: 149.into(),
            _task: Task(tokio::spawn(pending())),
        },
    );
    worker.navigation_event(NavigationEvent::Ack {
        epoch,
        channel,
        id: 149.into(),
        token,
        result: Ok(Value::Null),
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
async fn ack_response_uses_server_cursor_mentions_and_versions_and_rejects_stale_server_rows() {
    let (mut worker, events) = demo();
    let channel = Snowflake(11);
    let epoch = worker.navigation.epoch;
    worker.state.read_states.get_mut(&channel).unwrap().version = Some("6".into());
    let respond = |worker: &mut Worker, version: &str, cursor: &str, count: u32| {
        let token = worker.navigation.token();
        worker.navigation.acks.insert(
            channel,
            AckJob {
                token,
                message_id: 149.into(),
                _task: Task(tokio::spawn(pending())),
            },
        );
        worker.navigation_event(NavigationEvent::Ack{epoch,channel,id:149.into(),token,result:Ok(json!({"read_states":[{"id":"11","last_message_id":cursor,"mention_count":count,"version":version}]}))});
    };
    respond(&mut worker, "0007", "98", 3);
    let read = &worker.state.read_states[&channel];
    assert_eq!(read.last_message_id, Some(98.into()));
    assert_eq!(read.mention_count, 3);
    assert_eq!(read.version.as_deref(), Some("0007"));
    worker
        .state
        .apply_gateway(
            "MESSAGE_ACK",
            json!({"channel_id":"11","message_id":"130","mention_count":1,"version":"9"}),
        )
        .unwrap();
    respond(&mut worker, "8", "149", 0);
    let read = &worker.state.read_states[&channel];
    assert_eq!(read.last_message_id, Some(130.into()));
    assert_eq!(read.mention_count, 1);
    assert_eq!(read.version.as_deref(), Some("9"));
    assert!(events.try_iter().any(|event|matches!(event,Event::MessageAckResult{channel_id,message_id,error:None}if channel_id==channel&&message_id==Snowflake(149))));
}

#[tokio::test]
async fn manual_server_ack_cancels_pending_auto_ack_and_holds_until_channel_unmount() {
    let (mut worker, events) = demo();
    let channel = Snowflake(11);
    install_history(&mut worker, history(100..150, true));
    worker.state.read_states.get_mut(&channel).unwrap().version = Some("6".into());
    let token = worker.navigation.token();
    let (epoch, requested) = (worker.navigation.epoch, Snowflake(149));
    let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
    worker.navigation.acks.insert(
        channel,
        AckJob {
            token,
            message_id: requested,
            _task: Task(tokio::spawn(async move {
                let _captured = drop_tx;
                pending::<()>().await
            })),
        },
    );
    let stale =
        json!({"channel_id":"11","message_id":"50","mention_count":6,"version":"5","manual":true});
    worker
        .state
        .apply_gateway("MESSAGE_ACK", stale.clone())
        .unwrap();
    worker.navigation_gateway("MESSAGE_ACK", &stale);
    assert!(!worker.state.read_states[&channel].manual_ack);
    assert!(worker.navigation.acks.contains_key(&channel));
    let manual =
        json!({"channel_id":"11","message_id":"50","mention_count":6,"version":"7","manual":true});
    worker
        .state
        .apply_gateway("MESSAGE_ACK", manual.clone())
        .unwrap();
    worker.navigation_gateway("MESSAGE_ACK", &manual);
    assert!(worker.state.read_states[&channel].manual_ack);
    assert!(worker.navigation.acks.is_empty());
    assert!(
        worker
            .acknowledge_visible_message(channel, requested)
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), drop_rx)
            .await
            .unwrap()
            .is_err()
    );
    worker.navigation_event(NavigationEvent::Ack{epoch,channel,id:requested,token,result:Ok(json!({"read_states":[{"id":"11","last_message_id":"149","mention_count":0,"version":"8"}]}))});
    assert_eq!(
        worker.state.read_states[&channel].last_message_id,
        Some(50.into())
    );
    assert_eq!(worker.state.read_states[&channel].mention_count, 6);
    assert!(events.try_iter().any(|event|matches!(event,Event::MessageAckResult{channel_id,message_id,error:Some(_)}if channel_id==channel&&message_id==requested)));
    worker.navigation_context(channel);
    assert!(
        worker.state.read_states[&channel].manual_ack,
        "same-channel navigation does not invent a re-enable timer"
    );
    worker.navigation_context(13.into());
    assert!(!worker.state.read_states[&channel].manual_ack);
    assert!(
        serde_json::to_value(&worker.state.read_states[&channel])
            .unwrap()
            .get("manual_ack")
            .is_none()
    );
}

#[tokio::test]
async fn opt_in_demo_search_grants_pin_actions_and_real_timestamp_pagination_only_for_its_fixture()
{
    let (mut worker, _) = demo();
    assert!(!worker.state.can_pin_messages(11.into()));
    worker.seed_demo_search().unwrap();
    finish_history(&mut worker).await;
    assert!(worker.state.can_pin_messages(11.into()));
    assert!(!ClientState::demo().can_pin_messages(11.into()));
    worker.load_pins(11.into(), false).unwrap();
    loop {
        let event = worker.navigation.rx.recv().await.unwrap();
        let done = matches!(event, NavigationEvent::Pins { .. });
        worker.navigation_event(event);
        if done {
            break;
        }
    }
    let pins = &worker.navigation.pins[&Snowflake(11)].snapshot;
    assert_eq!(pins.items.len(), 25);
    assert!(pins.has_more);
    assert!(pins.items.iter().all(|pin| pin.message.pinned));
    worker.load_pins(11.into(), true).unwrap();
    loop {
        let event = worker.navigation.rx.recv().await.unwrap();
        let done = matches!(event, NavigationEvent::Pins { .. });
        worker.navigation_event(event);
        if done {
            break;
        }
    }
    let pins = &worker.navigation.pins[&Snowflake(11)].snapshot;
    assert_eq!(pins.items.len(), 26);
    assert!(!pins.has_more);
    assert_eq!(
        pins.items
            .iter()
            .map(|pin| &pin.pinned_at)
            .collect::<BTreeSet<_>>()
            .len(),
        26
    );
}

#[tokio::test]
async fn older_backfill_of_live_window_keeps_live_arrivals_without_inventing_a_newer_gap() {
    let (mut worker, _) = demo();
    let mut snapshot = history(100..150, true);
    snapshot.loading = true;
    let token = install_history(&mut worker, snapshot);
    worker.state.insert_message(messages(150, 150).remove(0));
    worker.navigation_message_inserted(11.into(), 150.into());
    worker.navigation_event(NavigationEvent::History {
        epoch: worker.navigation.epoch,
        channel: 11.into(),
        token,
        request: HistoryRequest::Before(100.into()),
        result: Ok(serde_json::to_value(messages(50, 99)).unwrap()),
    });
    let view = &worker.navigation.history[&Snowflake(11)].snapshot;
    assert_eq!(
        view.message_ids,
        (50..151).map(Snowflake).collect::<Vec<_>>()
    );
    assert!(view.latest && !view.has_newer);
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
    assert!(worker.state.can_pin_messages(40.into()));
    worker
        .state
        .channels
        .get_mut(&Snowflake(40))
        .unwrap()
        .recipients
        .push(User {
            id: Snowflake(0),
            username: "Fluxerbot".into(),
            ..Default::default()
        });
    assert!(!worker.state.can_pin_messages(40.into()));
    assert!(!worker.state.can_pin_messages(999999.into()));
}

#[tokio::test]
async fn detached_search_and_pins_follow_gateway_edit_reaction_and_delete_without_timeline_insertion()
 {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    install_search(&mut worker, channel);
    let message = messages(777, 777).remove(0);
    worker
        .navigation
        .searches
        .get_mut(&channel)
        .unwrap()
        .snapshot
        .messages = vec![message.clone()];
    worker.navigation.pins.insert(
        channel,
        PinsJob {
            token: 0,
            snapshot: PinsSnapshot {
                channel_id: channel,
                items: vec![PinnedMessage {
                    message,
                    pinned_at: "2026-11-12T12:00:00Z".into(),
                }],
                loading: false,
                fetched: true,
                has_more: false,
                can_manage: true,
                error: None,
            },
            task: None,
            open: true,
        },
    );
    worker.navigation_gateway("MESSAGE_REACTION_ADD",&json!({"channel_id":"11","message_id":"777","user_id":"100","emoji":{"name":"😀","id":null}}));
    assert_eq!(
        worker.navigation.searches[&channel].snapshot.messages[0].reactions[0].count,
        1
    );
    assert!(
        worker.navigation.pins[&channel].snapshot.items[0]
            .message
            .reactions[0]
            .me
    );
    worker.navigation_gateway(
        "MESSAGE_UPDATE",
        &json!({"channel_id":"11","id":"777","content":"edited preview","pinned":true}),
    );
    assert_eq!(
        worker.navigation.searches[&channel].snapshot.messages[0].content,
        "edited preview"
    );
    assert!(worker.navigation.searches[&channel].snapshot.messages[0].pinned);
    assert_eq!(
        worker.navigation.pins[&channel].snapshot.items[0]
            .message
            .content,
        "edited preview"
    );
    assert!(!worker.state.messages[&channel].contains_key(&Snowflake(777)));
    assert_eq!(
        worker.state.read_states[&channel].last_message_id,
        Some(10.into())
    );
    worker.navigation_gateway("MESSAGE_DELETE", &json!({"channel_id":"11","id":"777"}));
    assert!(
        worker.navigation.searches[&channel]
            .snapshot
            .messages
            .is_empty()
    );
    assert!(worker.navigation.pins[&channel].snapshot.items.is_empty());
}

#[tokio::test]
async fn live_pin_success_does_not_fetch_twice_and_gateway_invalidation_is_the_fetch_trigger() {
    let (mut worker, events) = demo();
    let channel = Snowflake(11);
    worker.demo = false;
    worker.client = Some(
        Client::new(
            Instance::from_endpoints("http://127.0.0.1:1/api", "ws://127.0.0.1:1/gateway").unwrap(),
        )
        .unwrap(),
    );
    worker.navigation.pins.insert(
        channel,
        PinsJob {
            token: 0,
            snapshot: PinsSnapshot {
                channel_id: channel,
                items: Vec::new(),
                loading: false,
                fetched: true,
                has_more: false,
                can_manage: true,
                error: None,
            },
            task: None,
            open: true,
        },
    );
    let token = worker.navigation.token();
    worker.navigation.mutations.insert(
        (channel, 777.into()),
        Mutation {
            token,
            _task: Task(tokio::spawn(pending())),
        },
    );
    worker.navigation_event(NavigationEvent::Pin {
        epoch: worker.navigation.epoch,
        channel,
        id: 777.into(),
        token,
        pinned: true,
        result: Ok(()),
    });
    assert!(worker.navigation.pins[&channel].task.is_none());
    assert!(worker.navigation.pins[&channel].snapshot.fetched);
    assert!(events.try_iter().any(|event|matches!(event,Event::PinMutation{message_id,pending:false,error:None,..}if message_id==Snowflake(777))));
    worker.navigation_gateway(
        "CHANNEL_PINS_UPDATE",
        &json!({"channel_id":"11","last_pin_timestamp":"2026-11-12T12:00:00Z"}),
    );
    assert!(worker.navigation.pins[&channel].task.is_some());
    assert!(worker.navigation.pins[&channel].snapshot.loading);
    assert_eq!(worker.navigation.next, token + 1);
    worker.clear_navigation();
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

#[tokio::test]
async fn explicit_read_unread_and_notification_changes_project_in_demo() {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    for message in messages(100, 102) {
        worker.state.insert_message(message);
    }
    worker.mark_unread(channel, Snowflake(102)).await.unwrap();
    assert_eq!(
        worker.state.read_states[&channel].last_message_id,
        Some(Snowflake(101))
    );
    assert!(worker.state.read_states[&channel].manual_ack);
    assert!(
        worker
            .acknowledge_visible_message(channel, Snowflake(102))
            .is_err()
    );
    worker
        .state
        .channels
        .get_mut(&channel)
        .unwrap()
        .last_message_id = Some(Snowflake(102));
    worker.mark_read(vec![channel]).await.unwrap();
    assert!(!worker.state.read_states[&channel].manual_ack);
    assert_eq!(
        worker.state.read_states[&channel].last_message_id,
        Some(Snowflake(102))
    );
    worker
        .set_notifications(
            Some(Snowflake(1)),
            Some(channel),
            Some(true),
            Some(900),
            Some(2),
        )
        .await
        .unwrap();
    let settings = worker.state.guild_notification_settings(Some(Snowflake(1)));
    let row = &settings.channel_overrides[&channel];
    assert!(row.muted);
    assert_eq!(row.message_notifications, 2);
    assert_eq!(
        row.mute_config.as_ref().unwrap().selected_time_window,
        Some(900_000)
    );
    worker
        .set_notifications(Some(Snowflake(1)), Some(channel), Some(false), None, None)
        .await
        .unwrap();
    let settings = worker.state.guild_notification_settings(Some(Snowflake(1)));
    assert_eq!(
        settings.channel_overrides[&channel].message_notifications,
        2
    );
    assert!(settings.channel_overrides[&channel].mute_config.is_none());
}

#[tokio::test]
async fn category_read_acknowledges_immediate_visible_message_children() {
    let (mut worker, _) = demo();
    let category = Snowflake(700);
    worker.state.channels.insert(
        category,
        Channel {
            id: category,
            guild_id: Some(Snowflake(1)),
            kind: ChannelType::Category,
            ..Default::default()
        },
    );
    worker
        .state
        .channels
        .get_mut(&Snowflake(11))
        .unwrap()
        .parent_id = Some(category);
    let nested = Snowflake(701);
    worker.state.channels.insert(
        nested,
        Channel {
            id: nested,
            guild_id: Some(Snowflake(1)),
            parent_id: Some(category),
            kind: ChannelType::Category,
            last_message_id: Some(Snowflake(55)),
            ..Default::default()
        },
    );
    worker
        .command(Command::MarkChannelRead {
            channel_id: category,
        })
        .await
        .unwrap();
    assert_eq!(
        worker.state.read_states[&Snowflake(11)].last_message_id,
        worker.state.channels[&Snowflake(11)].last_message_id
    );
    assert!(!worker.state.read_states.contains_key(&nested));
}

#[tokio::test]
async fn slowmode_snapshot_rejects_stale_account_channel_and_invalid_windows() {
    let (mut worker, events) = demo();
    let channel = Snowflake(11);
    let epoch = worker.navigation.epoch;
    let token = worker.navigation.token();
    worker.navigation.slowmode = Some((channel, token, Task(tokio::spawn(pending()))));
    let body = json!({"rate_limit_per_user":15,"retry_after_ms":2500,"can_bypass":false});
    worker.navigation_event(NavigationEvent::Slowmode {
        epoch: epoch + 1,
        channel,
        token,
        result: Ok(body.clone()),
    });
    worker.navigation_event(NavigationEvent::Slowmode {
        epoch,
        channel,
        token: token + 1,
        result: Ok(body.clone()),
    });
    worker.state.selected_channel = Some(Snowflake(12));
    worker.navigation_event(NavigationEvent::Slowmode {
        epoch,
        channel,
        token,
        result: Ok(body.clone()),
    });
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, Event::ChannelSlowmode { .. }))
    );
    worker.state.selected_channel = Some(channel);
    worker.navigation_event(NavigationEvent::Slowmode {
        epoch,
        channel,
        token,
        result: Ok(body),
    });
    assert!(events.try_iter().any(|event|matches!(event,Event::ChannelSlowmode {channel_id,rate_limit_per_user:15,retry_after,can_bypass:false} if channel_id==channel && retry_after==Duration::from_millis(2500))));
    assert_eq!(worker.state.channels[&channel].rate_limit_per_user, 15);
    let token = worker.navigation.token();
    worker.navigation.slowmode = Some((channel, token, Task(tokio::spawn(pending()))));
    worker.navigation_event(NavigationEvent::Slowmode {
        epoch,
        channel,
        token,
        result: Ok(json!({"rate_limit_per_user":15,"retry_after_ms":21600001,"can_bypass":false})),
    });
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, Event::ChannelSlowmode { .. }))
    );
}

#[tokio::test]
async fn mentions_pages_fence_generations_preserve_reads_and_keep_data_on_error() {
    let (mut worker, events) = demo();
    let epoch = worker.navigation.epoch;
    let token = worker.navigation.token();
    let read = worker.state.read_states[&Snowflake(11)].last_message_id;
    worker.navigation.mentions = Some(MentionsJob {
        token,
        snapshot: crate::MentionsSnapshot {
            account_id: Some(Snowflake(100)),
            loading: true,
            has_more: true,
            ..Default::default()
        },
        task: None,
    });
    let body = serde_json::to_value(messages(100, 149)).unwrap();
    worker.navigation_event(NavigationEvent::Mentions {
        epoch: epoch + 1,
        token,
        before: None,
        result: Ok(body.clone()),
    });
    assert!(
        events
            .try_iter()
            .all(|event| !matches!(event, Event::Mentions(_)))
    );
    worker.navigation_event(NavigationEvent::Mentions {
        epoch,
        token,
        before: None,
        result: Ok(body),
    });
    let first = worker.navigation.mentions.as_ref().unwrap();
    assert_eq!(first.snapshot.messages.len(), 50);
    assert!(first.snapshot.has_more);
    assert_eq!(first.snapshot.messages[0].id, Snowflake(149));
    worker.navigation.mentions.as_mut().unwrap().token = token + 1;
    worker.navigation_event(NavigationEvent::Mentions {
        epoch,
        token: token + 1,
        before: Some(Snowflake(100)),
        result: Ok(serde_json::to_value(messages(90, 100)).unwrap()),
    });
    assert_eq!(
        worker
            .navigation
            .mentions
            .as_ref()
            .unwrap()
            .snapshot
            .messages
            .len(),
        60
    );
    assert!(
        !worker
            .navigation
            .mentions
            .as_ref()
            .unwrap()
            .snapshot
            .has_more
    );
    worker.navigation_event(NavigationEvent::Mentions {
        epoch,
        token,
        before: None,
        result: Ok(json!([])),
    });
    assert_eq!(
        worker
            .navigation
            .mentions
            .as_ref()
            .unwrap()
            .snapshot
            .messages
            .len(),
        60
    );
    worker.navigation_event(NavigationEvent::Mentions {
        epoch,
        token: token + 1,
        before: Some(Snowflake(90)),
        result: Err("offline fixture failure".into()),
    });
    assert_eq!(
        worker
            .navigation
            .mentions
            .as_ref()
            .unwrap()
            .snapshot
            .messages
            .len(),
        60
    );
    assert!(
        worker
            .navigation
            .mentions
            .as_ref()
            .unwrap()
            .snapshot
            .error
            .is_some()
    );
    assert_eq!(
        worker.state.read_states[&Snowflake(11)].last_message_id,
        read
    );
    let mut live = Message::text(
        Snowflake(160),
        Snowflake(11),
        worker.state.users[&Snowflake(101)].clone(),
        "new mention",
    );
    live.mentions
        .push(worker.state.current_user.clone().unwrap());
    worker.navigation_mention_message(&live);
    assert_eq!(
        worker
            .navigation
            .mentions
            .as_ref()
            .unwrap()
            .snapshot
            .messages[0]
            .id,
        Snowflake(160)
    );
    worker.navigation_gateway(
        "MESSAGE_UPDATE",
        &json!({"id":"160","channel_id":"11","content":"edited mention"}),
    );
    assert_eq!(
        worker
            .navigation
            .mentions
            .as_ref()
            .unwrap()
            .snapshot
            .messages[0]
            .content,
        "edited mention"
    );
    worker.navigation_gateway("MESSAGE_DELETE", &json!({"id":"160","channel_id":"11"}));
    assert!(
        !worker
            .navigation
            .mentions
            .as_ref()
            .unwrap()
            .snapshot
            .messages
            .iter()
            .any(|message| message.id == Snowflake(160))
    );
    worker.navigation_close_mentions();
    worker.navigation_event(NavigationEvent::Mentions {
        epoch,
        token: token + 1,
        before: None,
        result: Ok(json!([])),
    });
    assert_eq!(
        worker
            .navigation
            .mentions
            .as_ref()
            .unwrap()
            .snapshot
            .messages
            .len(),
        60
    );
    assert!(
        !worker
            .navigation
            .mentions
            .as_ref()
            .unwrap()
            .snapshot
            .loading
    );
}
