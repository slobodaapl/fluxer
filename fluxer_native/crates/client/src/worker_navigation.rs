//! Background search, pins and history. Results carry account and request
//! generations; loading detached results never changes the live read cursor.
use super::*;
use crate::{
    HistorySnapshot, PinnedMessage, PinsSnapshot, SearchScope, SearchSnapshot, SearchSort,
    SearchStatus,
};
use fluxer_core::Channel;
use fluxer_protocol::SearchResponse;

const HISTORY_LIMIT: u8 = 50;
struct Task(JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct SearchJob {
    token: u64,
    snapshot: SearchSnapshot,
    _task: Task,
}
struct PinsJob {
    token: u64,
    snapshot: PinsSnapshot,
    task: Option<Task>,
    open: bool,
}
struct HistoryJob {
    token: u64,
    snapshot: HistorySnapshot,
    task: Option<Task>,
    arrived: BTreeSet<Snowflake>,
}
struct Mutation {
    token: u64,
    _task: Task,
}
struct AckJob {
    token: u64,
    message_id: Snowflake,
    _task: Task,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryRequest {
    Latest,
    Around(Snowflake),
    Before(Snowflake),
    After(Snowflake),
}
pub(super) enum NavigationEvent {
    Slowmode {epoch:u64,channel:Snowflake,token:u64,result:Result<Value>},
    Search {
        epoch: u64,
        channel: Snowflake,
        token: u64,
        poll_count: u32,
        result: Result<SearchResponse>,
    },
    Pins {
        epoch: u64,
        channel: Snowflake,
        token: u64,
        more: bool,
        result: Result<Value>,
    },
    Pin {
        epoch: u64,
        channel: Snowflake,
        id: Snowflake,
        token: u64,
        pinned: bool,
        result: Result<()>,
    },
    History {
        epoch: u64,
        channel: Snowflake,
        token: u64,
        request: HistoryRequest,
        result: Result<Value>,
    },
    Ack {
        epoch: u64,
        channel: Snowflake,
        id: Snowflake,
        token: u64,
        result: Result<Value>,
    },
    PinAck {
        epoch: u64,
        channel: Snowflake,
        token: u64,
        result: Result<()>,
    },
}
pub(super) struct NavigationState {
    epoch: u64,
    next: u64,
    searches: BTreeMap<Snowflake, SearchJob>,
    result_channels: BTreeMap<Snowflake, Channel>,
    pins: BTreeMap<Snowflake, PinsJob>,
    history: BTreeMap<Snowflake, HistoryJob>,
    mutations: BTreeMap<(Snowflake, Snowflake), Mutation>,
    acks: BTreeMap<Snowflake, AckJob>,
    pin_acks: BTreeMap<Snowflake, Mutation>,
    slowmode: Option<(Snowflake,u64,Task)>,
    demo_archive: BTreeMap<Snowflake, Vec<Message>>,
    demo_pins: BTreeMap<Snowflake, Vec<PinnedMessage>>,
    tx: async_mpsc::UnboundedSender<NavigationEvent>,
    pub rx: async_mpsc::UnboundedReceiver<NavigationEvent>,
}
impl NavigationState {
    pub fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        Self {
            epoch: 1,
            next: 0,
            searches: BTreeMap::new(),
            result_channels: BTreeMap::new(),
            pins: BTreeMap::new(),
            history: BTreeMap::new(),
            mutations: BTreeMap::new(),
            acks: BTreeMap::new(),
            pin_acks: BTreeMap::new(),
            slowmode: None,
            demo_archive: BTreeMap::new(),
            demo_pins: BTreeMap::new(),
            tx,
            rx,
        }
    }
    fn token(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.next
    }
}

impl Worker {
    pub(super) fn request_channel_slowmode(&mut self, channel:Snowflake) {
        self.navigation.slowmode=None;
        if self.demo || self.state.channels.get(&channel).is_none_or(|channel|channel.guild_id.is_none()) {return;}
        let Some(client)=self.client.clone() else {return;};
        let epoch=self.navigation.epoch;
        let token=self.navigation.token();
        let tx=self.navigation.tx.clone();
        let task=tokio::spawn(async move {
            let result=tokio::time::timeout(Duration::from_secs(5),client.channel_slowmode(&channel.to_string())).await.map_err(error).and_then(|result|result.map_err(error));
            let _=tx.send(NavigationEvent::Slowmode {epoch,channel,token,result});
        });
        self.navigation.slowmode=Some((channel,token,Task(task)));
    }
    pub(super) fn clear_navigation(&mut self) {
        self.navigation.epoch = self.navigation.epoch.wrapping_add(1);
        self.navigation.searches.clear();
        self.navigation.result_channels.clear();
        self.navigation.pins.clear();
        self.navigation.history.clear();
        self.navigation.mutations.clear();
        self.navigation.acks.clear();
        self.navigation.pin_acks.clear();
        self.navigation.slowmode = None;
        self.navigation.demo_archive.clear();
        self.navigation.demo_pins.clear();
    }
    pub(super) fn navigation_context(&mut self, channel: Snowflake) {
        if let Some(previous) = self
            .state
            .selected_channel
            .filter(|previous| *previous != channel)
            && let Some(read) = self.state.read_states.get_mut(&previous)
        {
            read.manual_ack = false;
        }
        // Search panes retain their detached context across explicit result
        // jumps. CloseSearch/replacement/account reset own their cancellation.
        for (id, pins) in &mut self.navigation.pins {
            if *id != channel {
                pins.task = None;
                pins.open = false;
                pins.snapshot.loading = false;
            }
        }
        for (id, history) in &mut self.navigation.history {
            if *id != channel {
                history.task = None;
                history.snapshot.loading = false;
            }
        }
        self.navigation.acks.retain(|id, _| *id == channel);
    }
    pub(super) fn require_history(&self, channel: Snowflake) -> Result<()> {
        let target = self
            .state
            .channels
            .get(&channel)
            .ok_or("Unknown message channel")?;
        if !target.kind.is_message_channel()
            || matches!(target.kind, ChannelType::Forum | ChannelType::Media)
        {
            return Err("Choose a message conversation".into());
        }
        let user = self.state.current_user.as_ref().ok_or("Sign in first")?;
        let permissions = self.state.permissions_for(channel, user.id);
        if !permissions.contains(Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY) {
            return Err("You cannot view message history in this channel".into());
        }
        Ok(())
    }
    fn can_pin(&self, channel: Snowflake) -> bool {
        self.state.can_pin_messages(channel)
    }
    pub(super) fn close_search(&mut self, channel: Snowflake) {
        self.navigation.searches.remove(&channel);
    }
    pub(super) fn search_messages(
        &mut self,
        channel: Snowflake,
        query: String,
        scope: SearchScope,
        scope_override: Option<SearchScope>,
        sort: SearchSort,
        page: u32,
    ) -> Result<()> {
        // Submitting even an unsearchable replacement ends the previous poll.
        self.navigation.searches.remove(&channel);
        self.require_history(channel)?;
        if page == 0 {
            return Err("Search pages start at one".into());
        }
        let guild = self
            .state
            .channels
            .get(&channel)
            .and_then(|channel| channel.guild_id);
        let unique_usernames = self.client.as_ref().is_some_and(|client| {
            client
                .instance()
                .features
                .get("account_identity")
                .and_then(Value::as_str)
                == Some("username")
                || client
                    .instance()
                    .features
                    .get("tag_style")
                    .and_then(Value::as_str)
                    == Some("none")
        });
        let parsed = fluxer_core::parse_search_query_with_options(
            &query,
            &self.state,
            &fluxer_core::SearchParseOptions {
                guild_id: guild,
                unique_usernames,
                ..Default::default()
            },
        );
        if !parsed.has_query {
            self.emit(Event::Search(SearchSnapshot {
                channel_id: channel,
                query,
                scope,
                sort,
                status: SearchStatus::Failed,
                messages: Vec::new(),
                channels: Vec::new(),
                total: 0,
                page,
                hits_per_page: 25,
                poll_count: 0,
                http_status: None,
                error: Some("Nothing in this query can be searched".into()),
            }));
            return Err("Nothing in this query can be searched".into());
        }
        let mut params = parsed.params;
        let allowed = |scope: SearchScope| guild.is_some() || scope != SearchScope::AllGuilds;
        let parsed_scope =
            params
                .get("scope")
                .and_then(Value::as_str)
                .and_then(|scope| match scope {
                    "current" => Some(SearchScope::Current),
                    "all_dms" => Some(SearchScope::AllDms),
                    "open_dms" => Some(SearchScope::OpenDms),
                    "all" => Some(SearchScope::All),
                    "open_dms_and_all_guilds" => Some(SearchScope::OpenDmsAndAllGuilds),
                    "all_guilds" => Some(SearchScope::AllGuilds),
                    _ => None,
                });
        let scope = scope_override
            .filter(|scope| allowed(*scope))
            .or(parsed_scope.filter(|scope| allowed(*scope)))
            .unwrap_or(if allowed(scope) {
                scope
            } else {
                SearchScope::Current
            });
        params["context_channel_id"] = channel.to_string().into();
        if let Some(guild) = guild {
            params["context_guild_id"] = guild.to_string().into();
        }
        if let Some(ids) = params
            .get("channel_id")
            .filter(|ids| ids.as_array().is_some_and(|ids| !ids.is_empty()))
            .cloned()
        {
            params["channel_ids"] = ids;
        }
        params["scope"] = scope.as_str().into();
        params["page"] = page.into();
        params["hits_per_page"] = 25.into();
        let (sort_by, sort_order) = match sort {
            SearchSort::Newest => ("timestamp", "desc"),
            SearchSort::Oldest => ("timestamp", "asc"),
            SearchSort::Relevant => ("relevance", "desc"),
        };
        params["sort_by"] = sort_by.into();
        params["sort_order"] = sort_order.into();
        // Mature search content is opt-in in the existing UI; do not silently
        // add include_nsfw before the native mature-content gate exists.
        let token = self.navigation.token();
        let epoch = self.navigation.epoch;
        let tx = self.navigation.tx.clone();
        let snapshot = SearchSnapshot {
            channel_id: channel,
            query,
            scope,
            sort,
            status: SearchStatus::Loading,
            messages: Vec::new(),
            channels: Vec::new(),
            total: 0,
            page,
            hits_per_page: 25,
            poll_count: 0,
            http_status: None,
            error: None,
        };
        let client = if self.demo {
            None
        } else {
            Some(self.client()?.clone())
        };
        let demo = self.navigation.demo_archive.clone();
        let context_channels = self.state.channels.clone();
        let poll_query = snapshot.query.clone();
        let task = tokio::spawn(async move {
            let mut polls = 0u32;
            loop {
                let result = if let Some(client) = &client {
                    client.search_messages(&params).await.map_err(error)
                } else {
                    let content = params
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_lowercase();
                    let mut messages = demo
                        .iter()
                        .filter(|(id, _)| {
                            scope != SearchScope::Current
                                || context_channels
                                    .get(id)
                                    .and_then(|channel| channel.guild_id)
                                    == guild
                        })
                        .flat_map(|(_, messages)| messages.iter())
                        .filter(|message| message.content.to_lowercase().contains(&content))
                        .cloned()
                        .collect::<Vec<_>>();
                    messages.sort_by_key(|message| message.id);
                    if sort != SearchSort::Oldest {
                        messages.reverse();
                    }
                    let total = messages.len();
                    let offset = (page as usize - 1).saturating_mul(25);
                    let items = messages
                        .into_iter()
                        .skip(offset)
                        .take(25)
                        .collect::<Vec<_>>();
                    Ok(SearchResponse {
                        status: 200,
                        body: json!({"messages":items,"channels":context_channels.values().collect::<Vec<_>>(),"total":total,"page":page,"hits_per_page":25}),
                        retry_after: None,
                    })
                };
                let indexing = result.as_ref().is_ok_and(SearchResponse::is_indexing);
                if tx
                    .send(NavigationEvent::Search {
                        epoch,
                        channel,
                        token,
                        poll_count: polls,
                        result,
                    })
                    .is_err()
                    || !indexing
                {
                    break;
                }
                let delay = Duration::from_secs_f64((1.5 * 1.25f64.powf(polls as f64)).min(5.));
                tokio::time::sleep(delay).await;
                polls = polls.saturating_add(1);
                fluxer_core::refresh_search_time_bounds(&poll_query, &mut params);
                params["page"] = 1.into();
            }
        });
        self.emit(Event::Search(snapshot.clone()));
        self.navigation.searches.insert(
            channel,
            SearchJob {
                token,
                snapshot,
                _task: Task(task),
            },
        );
        Ok(())
    }
    pub(super) fn close_pins(&mut self, channel: Snowflake) {
        if let Some(pins) = self.navigation.pins.get_mut(&channel) {
            pins.open = false;
            pins.task = None;
            pins.snapshot.loading = false;
        }
    }
    pub(super) fn load_pins(&mut self, channel: Snowflake, more: bool) -> Result<()> {
        self.require_history(channel)?;
        if !more
            && self
                .navigation
                .pins
                .get(&channel)
                .is_none_or(|pins| !pins.open)
        {
            self.acknowledge_opened_pins(channel)?;
        }
        let previous = self.navigation.pins.get(&channel);
        if more && previous.is_none_or(|pins| pins.snapshot.loading || !pins.snapshot.has_more) {
            return Ok(());
        }
        let before = if more {
            previous
                .and_then(|pins| pins.snapshot.items.last())
                .map(|pin| pin.pinned_at.clone())
        } else {
            None
        };
        if more && before.is_none() {
            return Ok(());
        }
        let mut snapshot = if more {
            previous.expect("pins checked").snapshot.clone()
        } else {
            PinsSnapshot {
                channel_id: channel,
                items: Vec::new(),
                loading: false,
                fetched: false,
                has_more: true,
                can_manage: self.can_pin(channel),
                error: None,
            }
        };
        snapshot.loading = true;
        snapshot.error = None;
        snapshot.can_manage = self.can_pin(channel);
        let token = self.navigation.token();
        let epoch = self.navigation.epoch;
        let tx = self.navigation.tx.clone();
        let client = if self.demo {
            None
        } else {
            Some(self.client()?.clone())
        };
        let demo = self
            .navigation
            .demo_pins
            .get(&channel)
            .cloned()
            .unwrap_or_default();
        let task = tokio::spawn(async move {
            let result = if let Some(client) = client {
                client
                    .pins_page(&channel.to_string(), before.as_deref())
                    .await
                    .map_err(error)
            } else {
                let mut items = demo
                    .into_iter()
                    .filter(|pin| before.as_ref().is_none_or(|before| pin.pinned_at < *before))
                    .collect::<Vec<_>>();
                let has_more = items.len() > 25;
                items.truncate(25);
                Ok(
                    json!({"items":items.iter().map(|pin|json!({"message":pin.message,"pinned_at":pin.pinned_at})).collect::<Vec<_>>(),"has_more":has_more}),
                )
            };
            let _ = tx.send(NavigationEvent::Pins {
                epoch,
                channel,
                token,
                more,
                result,
            });
        });
        self.emit(Event::Pins(snapshot.clone()));
        self.navigation.pins.insert(
            channel,
            PinsJob {
                token,
                snapshot,
                task: Some(Task(task)),
                open: true,
            },
        );
        Ok(())
    }
    fn acknowledge_opened_pins(&mut self, channel: Snowflake) -> Result<()> {
        if !self.state.apply_pin_ack(channel, None) {
            return Ok(());
        }
        self.snapshot();
        let client = if self.demo {
            None
        } else {
            Some(self.client()?.clone())
        };
        let epoch = self.navigation.epoch;
        let token = self.navigation.token();
        let tx = self.navigation.tx.clone();
        let task = tokio::spawn(async move {
            let result = if let Some(client) = client {
                client
                    .acknowledge_pins(&channel.to_string())
                    .await
                    .map_err(error)
            } else {
                Ok(())
            };
            let _ = tx.send(NavigationEvent::PinAck {
                epoch,
                channel,
                token,
                result,
            });
        });
        self.navigation.pin_acks.insert(
            channel,
            Mutation {
                token,
                _task: Task(task),
            },
        );
        Ok(())
    }
    pub(super) fn set_message_pinned(
        &mut self,
        channel: Snowflake,
        id: Snowflake,
        pinned: bool,
    ) -> Result<()> {
        self.require_history(channel)?;
        if !self.can_pin(channel) {
            return Err("You cannot pin messages in this channel".into());
        }
        if self.navigation.mutations.contains_key(&(channel, id)) {
            return Err("A pin update is already pending".into());
        }
        let token = self.navigation.token();
        let epoch = self.navigation.epoch;
        let tx = self.navigation.tx.clone();
        let client = if self.demo {
            None
        } else {
            Some(self.client()?.clone())
        };
        let task = tokio::spawn(async move {
            let result = if let Some(client) = client {
                if pinned {
                    client
                        .pin_message(&channel.to_string(), &id.to_string())
                        .await
                        .map_err(error)
                } else {
                    client
                        .unpin_message(&channel.to_string(), &id.to_string())
                        .await
                        .map_err(error)
                }
            } else {
                Ok(())
            };
            let _ = tx.send(NavigationEvent::Pin {
                epoch,
                channel,
                id,
                token,
                pinned,
                result,
            });
        });
        self.navigation.mutations.insert(
            (channel, id),
            Mutation {
                token,
                _task: Task(task),
            },
        );
        self.emit(Event::PinMutation {
            channel_id: channel,
            message_id: id,
            pinned,
            pending: true,
            error: None,
        });
        Ok(())
    }
    pub(super) fn request_history(
        &mut self,
        channel: Snowflake,
        request: HistoryRequest,
    ) -> Result<()> {
        self.require_history(channel)?;
        if self.state.selected_channel != Some(channel) {
            return Err("Choose this conversation first".into());
        }
        let previous = self.navigation.history.get(&channel);
        if matches!(
            request,
            HistoryRequest::Before(_) | HistoryRequest::After(_)
        ) && previous.is_some_and(|history| history.snapshot.loading)
        {
            return Ok(());
        }
        let mut snapshot =
            previous
                .map(|history| history.snapshot.clone())
                .unwrap_or(HistorySnapshot {
                    channel_id: channel,
                    message_ids: Vec::new(),
                    target: None,
                    loading: false,
                    has_older: true,
                    has_newer: false,
                    latest: true,
                    error: None,
                });
        snapshot.loading = true;
        snapshot.error = None;
        if let HistoryRequest::Around(target) = request {
            snapshot.target = Some(target);
        }
        if matches!(request, HistoryRequest::Latest) {
            snapshot.target = None;
        }
        let token = self.navigation.token();
        let epoch = self.navigation.epoch;
        let tx = self.navigation.tx.clone();
        let client = if self.demo {
            None
        } else {
            Some(self.client()?.clone())
        };
        let demo = self
            .navigation
            .demo_archive
            .get(&channel)
            .cloned()
            .unwrap_or_else(|| self.state.channel_messages(channel).cloned().collect());
        let task = tokio::spawn(async move {
            let result = if let Some(client) = client {
                let (before, after, around) = match request {
                    HistoryRequest::Latest => (None, None, None),
                    HistoryRequest::Before(id) => (Some(id.to_string()), None, None),
                    HistoryRequest::After(id) => (None, Some(id.to_string()), None),
                    HistoryRequest::Around(id) => (None, None, Some(id.to_string())),
                };
                client
                    .messages_page(
                        &channel.to_string(),
                        before.as_deref(),
                        after.as_deref(),
                        around.as_deref(),
                        HISTORY_LIMIT,
                    )
                    .await
                    .map_err(error)
            } else {
                Ok(Value::Array(
                    demo_history(&demo, request)
                        .into_iter()
                        .map(|message| serde_json::to_value(message).expect("message serializes"))
                        .collect(),
                ))
            };
            let _ = tx.send(NavigationEvent::History {
                epoch,
                channel,
                token,
                request,
                result,
            });
        });
        self.emit(Event::History(snapshot.clone()));
        self.navigation.history.insert(
            channel,
            HistoryJob {
                token,
                snapshot,
                task: Some(Task(task)),
                arrived: BTreeSet::new(),
            },
        );
        Ok(())
    }
    pub(super) fn load_adjacent_history(&mut self, channel: Snowflake, newer: bool) -> Result<()> {
        let history = self
            .navigation
            .history
            .get(&channel)
            .ok_or("Load this conversation first")?;
        if newer && !history.snapshot.has_newer || !newer && !history.snapshot.has_older {
            return Ok(());
        }
        let edge = if newer {
            history.snapshot.message_ids.last()
        } else {
            history.snapshot.message_ids.first()
        }
        .copied();
        if let Some(edge) = edge {
            self.request_history(
                channel,
                if newer {
                    HistoryRequest::After(edge)
                } else {
                    HistoryRequest::Before(edge)
                },
            )?;
        }
        Ok(())
    }
    pub(super) async fn jump_to_message(
        &mut self,
        channel: Snowflake,
        id: Snowflake,
    ) -> Result<()> {
        if !self.state.channels.contains_key(&channel) {
            let target = self
                .navigation
                .result_channels
                .get(&channel)
                .cloned()
                .ok_or("Search result channel is no longer available")?;
            self.state.channels.insert(channel, target);
        }
        self.require_history(channel)?;
        self.navigation_context(channel);
        self.state.selected_channel = Some(channel);
        self.selected_guild = self.state.channels[&channel].guild_id;
        self.restore_channel = None;
        self.attachment_snapshot(channel);
        self.snapshot();
        self.request_history(channel, HistoryRequest::Around(id))?;
        if !self.demo {
            self.sync_selected_guild().await?;
        }
        self.persist_selection().await;
        Ok(())
    }
    pub(super) async fn mark_read(&mut self, channels: Vec<Snowflake>) -> Result<()> {
        let entries: Vec<_> = channels
            .into_iter()
            .filter_map(|id| {
                self.state
                    .channels
                    .get(&id)
                    .and_then(|c| c.last_message_id)
                    .map(|message| (id, message))
            })
            .collect();
        for chunk in entries.chunks(100) {
            let rows: Vec<_> = chunk
                .iter()
                .map(|(channel, message)| json!({"channel_id":channel,"message_id":message}))
                .collect();
            let mut response = Value::Null;
            if !self.demo {
                let client = self.client()?.clone();
                let body = json!({"read_states":rows});
                match client
                    .request(
                        fluxer_protocol::Method::POST,
                        "/read-states/ack",
                        Some(&body),
                    )
                    .await
                {
                    Err(fluxer_protocol::Error::Api {
                        status: 404 | 405, ..
                    }) => {
                        client
                            .request(
                                fluxer_protocol::Method::POST,
                                "/read-states/ack-bulk",
                                Some(&body),
                            )
                            .await
                            .map_err(error)?;
                    }
                    Err(e) => return Err(error(e)),
                    Ok(body) => response = body,
                }
            }
            let returned: Vec<fluxer_core::ReadState> = response
                .get("read_states")
                .map(|v| self.state.decode_api(v.clone()).map_err(error))
                .transpose()?
                .unwrap_or_default();
            for (channel, message) in chunk {
                self.navigation.acks.remove(channel);
                if response.get("read_states").is_some() {
                    if let Some(read) = self.state.read_states.get_mut(channel) {
                        read.manual_ack = false;
                    }
                    continue;
                }
                let read =
                    self.state
                        .read_states
                        .entry(*channel)
                        .or_insert(fluxer_core::ReadState {
                            id: *channel,
                            ..Default::default()
                        });
                read.last_message_id = Some(*message);
                read.mention_count = 0;
                read.manual_ack = false;
            }
            for row in returned {
                if let Some(message) = row.last_message_id {
                    self.state.apply_gateway("MESSAGE_ACK", json!({"channel_id":row.id,"message_id":message,"mention_count":row.mention_count,"version":row.version})).map_err(error)?;
                }
            }
        }
        self.snapshot();
        Ok(())
    }
    pub(super) async fn mark_unread(
        &mut self,
        channel: Snowflake,
        message: Snowflake,
    ) -> Result<()> {
        let messages = self
            .state
            .messages
            .get(&channel)
            .ok_or("Message is not cached")?;
        if !messages.contains_key(&message) {
            return Err("Message is not cached".into());
        }
        let previous = messages
            .range(..message)
            .next_back()
            .map(|(id, _)| *id)
            .unwrap_or(Snowflake((message.0 >> 22).saturating_sub(1) << 22));
        if previous.0 == 0 {
            return Err("No previous readable message".into());
        }
        let count = self.state.manual_ack_mention_count(channel, previous);
        // Cancel a pending automatic acknowledgement before issuing a manual one.
        self.navigation.acks.remove(&channel);
        let mut response = Value::Null;
        if !self.demo {
            let client = self.client()?.clone();
            let body = json!({"read_states":[{"channel_id":channel,"message_id":previous,"mention_count":count,"manual":true}]});
            match client
                .request(
                    fluxer_protocol::Method::POST,
                    "/read-states/ack",
                    Some(&body),
                )
                .await
            {
                Err(fluxer_protocol::Error::Api {
                    status: 404 | 405, ..
                }) => {
                    client
                        .request(
                            fluxer_protocol::Method::POST,
                            &format!("/channels/{channel}/messages/{previous}/ack"),
                            Some(&json!({"manual":true,"mention_count":count})),
                        )
                        .await
                        .map_err(error)?;
                }
                Err(e) => return Err(error(e)),
                Ok(body) => response = body,
            }
        }
        self.navigation.acks.remove(&channel);
        let read = self
            .state
            .read_states
            .entry(channel)
            .or_insert(fluxer_core::ReadState {
                id: channel,
                ..Default::default()
            });
        if response.get("read_states").is_none() {
            read.last_message_id = Some(previous);
            read.mention_count = count;
            read.manual_ack = true;
        }
        if let Some(rows) = response.get("read_states") {
            let rows: Vec<fluxer_core::ReadState> =
                self.state.decode_api(rows.clone()).map_err(error)?;
            for row in rows {
                if let Some(id) = row.last_message_id {
                    self.state.apply_gateway("MESSAGE_ACK", json!({"channel_id":row.id,"message_id":id,"mention_count":row.mention_count,"version":row.version,"manual":true})).map_err(error)?;
                }
            }
        }
        self.snapshot();
        Ok(())
    }
    pub(super) async fn set_notifications(
        &mut self,
        guild: Option<Snowflake>,
        channel: Option<Snowflake>,
        muted: Option<bool>,
        seconds: Option<u64>,
        level: Option<u8>,
    ) -> Result<()> {
        if let Some(id) = channel {
            let target = self
                .state
                .channels
                .get(&id)
                .ok_or("Unknown notification channel")?;
            if target.guild_id != guild
                || target.kind.is_thread()
                || !target.kind.is_message_channel()
                    && target.kind != fluxer_core::ChannelType::Category
            {
                return Err("Channel does not support notification settings in this scope".into());
            }
        } else if guild.is_some_and(|id| !self.state.guilds.contains_key(&id)) {
            return Err("Unknown notification community".into());
        }
        if level.is_some_and(|n| n > 3) {
            return Err("Invalid notification level".into());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let mut settings = self.state.guild_notification_settings(guild);
        let config = if muted == Some(true) {
            seconds.map(|s| fluxer_core::MuteConfig::for_seconds(s, now))
        } else {
            None
        };
        let body = if let Some(id) = channel {
            let entry = settings
                .channel_overrides
                .entry(id)
                .or_insert_with(|| fluxer_core::ChannelNotificationOverride::new(id));
            if let Some(muted) = muted {
                entry.muted = muted;
                entry.mute_config = config;
            }
            if let Some(level) = level {
                entry.message_notifications = level as i8;
            }
            json!({"channel_overrides":settings.channel_overrides})
        } else {
            let mut body = serde_json::Map::new();
            if let Some(muted) = muted {
                settings.muted = muted;
                settings.mute_config = config;
                body.insert("muted".into(), json!(muted));
                body.insert("mute_config".into(), json!(settings.mute_config));
            }
            if let Some(level) = level {
                settings.message_notifications = level as i8;
                body.insert("message_notifications".into(), json!(level));
            }
            Value::Object(body)
        };
        if !self.demo {
            self.client()?
                .request(
                    fluxer_protocol::Method::PATCH,
                    &format!(
                        "/users/@me/guilds/{}/settings",
                        guild.map_or_else(|| "@me".to_string(), |id| id.to_string())
                    ),
                    Some(&body),
                )
                .await
                .map_err(error)?;
        }
        self.state.user_guild_settings.insert(guild, settings);
        self.snapshot();
        Ok(())
    }
    pub(super) fn acknowledge_visible_message(
        &mut self,
        channel: Snowflake,
        id: Snowflake,
    ) -> Result<()> {
        if self
            .state
            .read_states
            .get(&channel)
            .is_some_and(|read| read.manual_ack)
        {
            return Err("Automatic read acknowledgement is held by a manual unread marker".into());
        }
        if self.state.selected_channel != Some(channel)
            || !self
                .navigation
                .history
                .get(&channel)
                .is_some_and(|history| {
                    !history.snapshot.loading
                        && history.snapshot.latest
                        && !history.snapshot.has_newer
                        && history.snapshot.message_ids.contains(&id)
                })
        {
            return Err(
                "Only a visible message at the live edge can be automatically marked read".into(),
            );
        }
        if self
            .state
            .read_states
            .get(&channel)
            .and_then(|read| read.last_message_id)
            .is_some_and(|read| read >= id)
        {
            self.emit(Event::MessageAckResult {
                channel_id: channel,
                message_id: id,
                error: None,
            });
            return Ok(());
        }
        let token = self.navigation.token();
        let epoch = self.navigation.epoch;
        let tx = self.navigation.tx.clone();
        let client = if self.demo {
            None
        } else {
            Some(self.client()?.clone())
        };
        let task = tokio::spawn(async move {
            let result = if let Some(client) = client {
                client
                    .acknowledge(&channel.to_string(), &id.to_string())
                    .await
                    .map_err(error)
            } else {
                Ok(Value::Null)
            };
            let _ = tx.send(NavigationEvent::Ack {
                epoch,
                channel,
                id,
                token,
                result,
            });
        });
        self.navigation.acks.insert(
            channel,
            AckJob {
                token,
                message_id: id,
                _task: Task(task),
            },
        );
        Ok(())
    }
    pub(super) fn seed_demo_search(&mut self) -> Result<()> {
        if !self.demo {
            return Err("Search fixtures are available only in offline Demo".into());
        }
        let channel = Snowflake(11);
        let user = self.state.current_user.clone().ok_or("Open Demo first")?;
        let mut messages = (1000..1120)
            .map(|id| {
                Message::text(
                    Snowflake(id),
                    channel,
                    user.clone(),
                    format!(
                        "Archive release example {id}: conversation history, pins and navigation"
                    ),
                )
            })
            .collect::<Vec<_>>();
        if let Some(guild) = self.state.guilds.get_mut(&Snowflake(1))
            && let Some(role) = guild.roles.iter_mut().find(|role| role.id == guild.id)
        {
            role.permissions |= Permissions::PIN_MESSAGES;
        }
        let pins = messages
            .iter_mut()
            .skip(42)
            .take(26)
            .enumerate()
            .map(|(index, message)| {
                message.pinned = true;
                PinnedMessage {
                    message: message.clone(),
                    pinned_at: format!("2026-11-12T12:{:02}:00.000Z", 59 - index),
                }
            })
            .collect();
        self.navigation.demo_pins.insert(channel, pins);
        self.navigation.demo_archive.insert(channel, messages);
        self.state
            .channels
            .get_mut(&channel)
            .expect("demo channel")
            .last_message_id = Some(1119.into());
        self.request_history(channel, HistoryRequest::Latest)
    }

    pub(super) fn navigation_message_inserted(&mut self, channel: Snowflake, id: Snowflake) {
        let Some(history) = self.navigation.history.get_mut(&channel) else {
            return;
        };
        if history.snapshot.loading {
            history.arrived.insert(id);
            return;
        }
        if history.snapshot.latest {
            history.snapshot.message_ids.push(id);
            history.snapshot.message_ids.sort_unstable();
            history.snapshot.message_ids.dedup();
            if history.snapshot.message_ids.len() > 200 {
                let excess = history.snapshot.message_ids.len() - 200;
                history.snapshot.message_ids.drain(..excess);
                history.snapshot.has_older = true;
            }
        } else if history
            .snapshot
            .message_ids
            .last()
            .is_none_or(|last| id > *last)
        {
            history.snapshot.has_newer = true;
        }
        self.emit(Event::History(
            self.navigation.history[&channel].snapshot.clone(),
        ));
    }
    pub(super) fn navigation_gateway(&mut self, name: &str, data: &Value) {
        let channel = data
            .get("channel_id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<Snowflake>().ok());
        if name == "READY" {
            self.clear_navigation();
            return;
        }
        let Some(channel) = channel else { return };
        if name == "MESSAGE_ACK"
            && data.get("manual").and_then(Value::as_bool) == Some(true)
            && self
                .state
                .read_states
                .get(&channel)
                .is_some_and(|read| read.manual_ack)
        {
            if let Some(pending) = self.navigation.acks.remove(&channel) {
                self.emit(Event::MessageAckResult {
                    channel_id: channel,
                    message_id: pending.message_id,
                    error: Some(
                        "Automatic acknowledgement cancelled by a manual unread marker".into(),
                    ),
                });
            }
            return;
        }
        let id = data
            .get("id")
            .or_else(|| data.get("message_id"))
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<Snowflake>().ok());
        if name == "MESSAGE_CREATE" {
            if let Some(id) = id {
                self.navigation_message_inserted(channel, id);
            }
            return;
        }
        if matches!(
            name,
            "MESSAGE_UPDATE" | "MESSAGE_DELETE" | "MESSAGE_DELETE_BULK"
        ) || name.starts_with("MESSAGE_REACTION_")
        {
            let ids = if name == "MESSAGE_DELETE_BULK" {
                data.get("ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|id| id.as_str()?.parse::<Snowflake>().ok())
                    .collect::<BTreeSet<_>>()
            } else {
                id.into_iter().collect()
            };
            let mut snapshots = Vec::new();
            for job in self.navigation.searches.values_mut() {
                let mut changed = false;
                job.snapshot.messages.retain_mut(|message| {
                    if message.channel_id != channel || !ids.contains(&message.id) {
                        return true;
                    }
                    match self
                        .state
                        .reduce_detached_message(message, name, data.clone())
                    {
                        Ok(Some(updated)) => {
                            *message = updated;
                            changed = true;
                            true
                        }
                        Ok(None) => {
                            changed = true;
                            false
                        }
                        Err(_) => true,
                    }
                });
                if changed {
                    snapshots.push(job.snapshot.clone());
                }
            }
            for snapshot in snapshots {
                self.emit(Event::Search(snapshot));
            }
        }
        if name == "CHANNEL_PINS_UPDATE" {
            let open = self
                .navigation
                .pins
                .get(&channel)
                .is_some_and(|pins| pins.open);
            if let Some(pins) = self.navigation.pins.get_mut(&channel) {
                pins.task = None;
                pins.snapshot.items.clear();
                pins.snapshot.fetched = false;
                pins.snapshot.has_more = true;
                pins.snapshot.loading = false;
            }
            if open && let Err(message) = self.load_pins(channel, false) {
                self.emit(Event::Error(message));
            }
            return;
        }
        if matches!(name, "MESSAGE_DELETE" | "MESSAGE_DELETE_BULK") {
            let ids = if name == "MESSAGE_DELETE" {
                id.into_iter().collect::<BTreeSet<_>>()
            } else {
                data.get("ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|id| id.as_str()?.parse::<Snowflake>().ok())
                    .collect()
            };
            if let Some(history) = self.navigation.history.get_mut(&channel) {
                history.snapshot.message_ids.retain(|id| !ids.contains(id));
                history.arrived.retain(|id| !ids.contains(id));
                self.emit(Event::History(
                    self.navigation.history[&channel].snapshot.clone(),
                ));
            }
            if let Some(pins) = self.navigation.pins.get_mut(&channel) {
                pins.snapshot
                    .items
                    .retain(|pin| !ids.contains(&pin.message.id));
                self.emit(Event::Pins(self.navigation.pins[&channel].snapshot.clone()));
            }
        } else if name == "MESSAGE_UPDATE" || name.starts_with("MESSAGE_REACTION_") {
            let Some(id) = id else { return };
            let mut changed = None;
            if let Some(pins) = self.navigation.pins.get_mut(&channel) {
                if data.get("pinned").and_then(Value::as_bool) == Some(false) {
                    pins.snapshot.items.retain(|pin| pin.message.id != id);
                } else if let Some(pin) = pins
                    .snapshot
                    .items
                    .iter_mut()
                    .find(|pin| pin.message.id == id)
                    && let Ok(Some(message)) =
                        self.state
                            .reduce_detached_message(&pin.message, name, data.clone())
                {
                    pin.message = message;
                }
                changed = Some(pins.snapshot.clone());
            }
            if let Some(snapshot) = changed {
                self.emit(Event::Pins(snapshot));
            }
        }
    }
    pub(super) fn navigation_event(&mut self, event: NavigationEvent) {
        match event {
            NavigationEvent::Slowmode {epoch,channel,token,result} => {
                if epoch!=self.navigation.epoch || self.state.selected_channel!=Some(channel) || !self.navigation.slowmode.as_ref().is_some_and(|(id,active,_)|*id==channel && *active==token) {return;}
                self.navigation.slowmode=None;
                let Ok(body)=result else {return;};
                let (Some(rate),Some(retry),Some(can_bypass))=(body.get("rate_limit_per_user").and_then(Value::as_u64),body.get("retry_after_ms").and_then(Value::as_u64),body.get("can_bypass").and_then(Value::as_bool)) else {return;};
                if rate>21600 || retry>21600000 {return;}
                if let Some(target)=self.state.channels.get_mut(&channel) {target.rate_limit_per_user=rate as u32;}
                self.emit(Event::ChannelSlowmode {channel_id:channel,rate_limit_per_user:rate as u32,retry_after:Duration::from_millis(retry),can_bypass});
                self.snapshot();
            }
            NavigationEvent::PinAck {
                epoch,
                channel,
                token,
                result,
            } => {
                if epoch != self.navigation.epoch
                    || !self
                        .navigation
                        .pin_acks
                        .get(&channel)
                        .is_some_and(|job| job.token == token)
                {
                    return;
                }
                self.navigation.pin_acks.remove(&channel);
                if let Err(message) = result {
                    self.emit(Event::Error(message));
                }
            }
            NavigationEvent::Search {
                epoch,
                channel,
                token,
                poll_count,
                result,
            } => {
                if epoch != self.navigation.epoch
                    || !self
                        .navigation
                        .searches
                        .get(&channel)
                        .is_some_and(|job| job.token == token)
                {
                    return;
                }
                let result = result.and_then(|response| {
                    if response.is_indexing() {
                        return Ok((response.status, None));
                    }
                    if !response.body.is_object() {
                        return Err("Search returned no response body".into());
                    }
                    let messages = self
                        .state
                        .decode_api::<Vec<Message>>(
                            response.body.get("messages").cloned().unwrap_or(json!([])),
                        )
                        .map_err(error)?;
                    let mut channels = self
                        .state
                        .decode_api::<Vec<Channel>>(
                            response.body.get("channels").cloned().unwrap_or(json!([])),
                        )
                        .map_err(error)?;
                    let members = response.body.get("members").and_then(Value::as_array);
                    for mut thread in response
                        .body
                        .get("threads")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .cloned()
                    {
                        if let Some(member) = members
                            .into_iter()
                            .flatten()
                            .find(|member| member.get("id") == thread.get("id"))
                        {
                            thread["member"] = member.clone();
                        }
                        let thread: Channel = self.state.decode_api(thread).map_err(error)?;
                        // ThreadCommands.ingestSearchThreads uses only known
                        // guilds; membership is retained on the channel object.
                        let active = thread.kind.is_thread()
                            && thread
                                .guild_id
                                .is_some_and(|guild| self.state.thread_guilds.contains(&guild));
                        if active {
                            self.state.channels.insert(thread.id, thread.clone());
                        }
                        if active && !channels.iter().any(|channel| channel.id == thread.id) {
                            channels.push(thread);
                        }
                    }
                    let total = response
                        .body
                        .get("total")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let page = response
                        .body
                        .get("page")
                        .and_then(Value::as_u64)
                        .unwrap_or(1)
                        .try_into()
                        .map_err(error)?;
                    let hits = response
                        .body
                        .get("hits_per_page")
                        .and_then(Value::as_u64)
                        .unwrap_or(25)
                        .try_into()
                        .map_err(error)?;
                    Ok((
                        response.status,
                        Some((messages, channels, total, page, hits)),
                    ))
                });
                let job = self
                    .navigation
                    .searches
                    .get_mut(&channel)
                    .expect("generation checked");
                job.snapshot.poll_count = poll_count;
                match result {
                    Ok((status, None)) => {
                        job.snapshot.http_status = Some(status);
                        job.snapshot.status = SearchStatus::Indexing;
                    }
                    Ok((status, Some((messages, channels, total, page, hits)))) => {
                        self.navigation.result_channels.extend(
                            channels
                                .iter()
                                .cloned()
                                .map(|channel| (channel.id, channel)),
                        );
                        job.snapshot.http_status = Some(status);
                        job.snapshot.status = SearchStatus::Ready;
                        job.snapshot.messages = messages;
                        job.snapshot.channels = channels;
                        job.snapshot.total = total;
                        job.snapshot.page = page;
                        job.snapshot.hits_per_page = hits;
                    }
                    Err(message) => {
                        job.snapshot.status = SearchStatus::Failed;
                        job.snapshot.error = Some(message);
                    }
                }
                self.emit(Event::Search(
                    self.navigation.searches[&channel].snapshot.clone(),
                ));
                self.snapshot();
            }
            NavigationEvent::Pins {
                epoch,
                channel,
                token,
                more,
                result,
            } => {
                if epoch != self.navigation.epoch
                    || !self
                        .navigation
                        .pins
                        .get(&channel)
                        .is_some_and(|job| job.token == token && job.open)
                {
                    return;
                }
                let result = result.and_then(|body| {
                    let mut pins = Vec::new();
                    for item in body
                        .get("items")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let message = self
                            .state
                            .decode_api::<Message>(
                                item.get("message")
                                    .cloned()
                                    .ok_or("Pin response omitted its message")?,
                            )
                            .map_err(error)?;
                        let pinned_at = item
                            .get("pinned_at")
                            .and_then(Value::as_str)
                            .ok_or("Pin response omitted its timestamp")?
                            .to_owned();
                        pins.push(PinnedMessage { message, pinned_at });
                    }
                    Ok((
                        pins,
                        body.get("has_more")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    ))
                });
                let job = self
                    .navigation
                    .pins
                    .get_mut(&channel)
                    .expect("generation checked");
                job.task = None;
                job.snapshot.loading = false;
                job.snapshot.fetched = true;
                match result {
                    Ok((items, more_items)) => {
                        if !more {
                            job.snapshot.items.clear();
                        }
                        for item in items {
                            if !job
                                .snapshot
                                .items
                                .iter()
                                .any(|old| old.message.id == item.message.id)
                            {
                                job.snapshot.items.push(item);
                            }
                        }
                        job.snapshot.has_more = more_items;
                    }
                    Err(message) => {
                        job.snapshot.error = Some(message);
                        job.snapshot.has_more = false;
                    }
                }
                self.emit(Event::Pins(self.navigation.pins[&channel].snapshot.clone()));
            }
            NavigationEvent::Pin {
                epoch,
                channel,
                id,
                token,
                pinned,
                result,
            } => {
                if epoch != self.navigation.epoch
                    || !self
                        .navigation
                        .mutations
                        .get(&(channel, id))
                        .is_some_and(|job| job.token == token)
                {
                    return;
                }
                self.navigation.mutations.remove(&(channel, id));
                if result.is_ok() && self.demo {
                    if let Some(message) = self
                        .state
                        .messages
                        .get_mut(&channel)
                        .and_then(|messages| messages.get_mut(&id))
                    {
                        message.pinned = pinned;
                    }
                    if let Some(message) = self
                        .navigation
                        .demo_archive
                        .get_mut(&channel)
                        .into_iter()
                        .flatten()
                        .find(|message| message.id == id)
                    {
                        message.pinned = pinned;
                    }
                    let pins = self.navigation.demo_pins.entry(channel).or_default();
                    pins.retain(|pin| pin.message.id != id);
                    if pinned
                        && let Some(message) = self
                            .navigation
                            .demo_archive
                            .get(&channel)
                            .into_iter()
                            .flatten()
                            .find(|message| message.id == id)
                            .cloned()
                    {
                        pins.insert(
                            0,
                            PinnedMessage {
                                message,
                                pinned_at: "2026-11-12T13:00:00.000Z".into(),
                            },
                        );
                    }
                    self.snapshot();
                }
                let succeeded = result.is_ok();
                self.emit(Event::PinMutation {
                    channel_id: channel,
                    message_id: id,
                    pinned,
                    pending: false,
                    error: result.err(),
                });
                if succeeded
                    && self.demo
                    && self
                        .navigation
                        .pins
                        .get(&channel)
                        .is_some_and(|pins| pins.open)
                    && let Err(message) = self.load_pins(channel, false)
                {
                    self.emit(Event::Error(message));
                }
            }
            NavigationEvent::History {
                epoch,
                channel,
                token,
                request,
                result,
            } => {
                if epoch != self.navigation.epoch
                    || self.state.selected_channel != Some(channel)
                    || !self
                        .navigation
                        .history
                        .get(&channel)
                        .is_some_and(|job| job.token == token)
                {
                    return;
                }
                let result = result
                    .and_then(|value| {
                        self.state
                            .decode_api::<Vec<Message>>(if value.is_null() {
                                json!([])
                            } else {
                                value
                            })
                            .map_err(error)
                    })
                    .and_then(|messages| {
                        if messages.iter().any(|message| message.channel_id != channel) {
                            Err("History response contained a different channel".into())
                        } else {
                            Ok(messages)
                        }
                    });
                let known_latest = self
                    .state
                    .channels
                    .get(&channel)
                    .and_then(|channel| channel.last_message_id);
                let job = self
                    .navigation
                    .history
                    .get_mut(&channel)
                    .expect("generation checked");
                job.task = None;
                job.snapshot.loading = false;
                match result {
                    Ok(messages) => {
                        let was_latest = job.snapshot.latest;
                        apply_history_page(&mut job.snapshot, request, &messages, known_latest);
                        if matches!(request, HistoryRequest::Latest)
                            || (matches!(request, HistoryRequest::Before(_))
                                && was_latest
                                && job.snapshot.latest)
                        {
                            let newest = job.snapshot.message_ids.last().copied();
                            job.snapshot.message_ids.extend(
                                job.arrived
                                    .iter()
                                    .copied()
                                    .filter(|id| newest.is_none_or(|newest| *id > newest)),
                            );
                            job.snapshot.message_ids.sort_unstable();
                            job.snapshot.message_ids.dedup();
                            if job.snapshot.message_ids.len() > 200 {
                                let excess = job.snapshot.message_ids.len() - 200;
                                job.snapshot.message_ids.drain(..excess);
                                job.snapshot.has_older = true;
                            }
                        } else if !job.arrived.is_empty() {
                            job.snapshot.has_newer = true;
                            job.snapshot.latest = false;
                        }
                        job.arrived.clear();
                        self.state.load_history(channel, messages);
                    }
                    Err(message) => {
                        job.snapshot.error = Some(message);
                    }
                }
                self.snapshot();
                self.emit(Event::History(
                    self.navigation.history[&channel].snapshot.clone(),
                ));
            }
            NavigationEvent::Ack {
                epoch,
                channel,
                id,
                token,
                result,
            } => {
                if epoch != self.navigation.epoch
                    || !self
                        .navigation
                        .acks
                        .get(&channel)
                        .is_some_and(|job| job.token == token)
                {
                    return;
                }
                self.navigation.acks.remove(&channel);
                let result = result.and_then(|body| {
                    body.get("read_states")
                        .map(|rows| {
                            self.state
                                .decode_api::<Vec<fluxer_core::ReadState>>(rows.clone())
                                .map(Some)
                                .map_err(error)
                        })
                        .unwrap_or(Ok(None))
                });
                self.emit(Event::MessageAckResult {
                    channel_id: channel,
                    message_id: id,
                    error: result.as_ref().err().cloned(),
                });
                match result {
                    Ok(Some(rows)) => {
                        for row in rows {
                            if let Some(message) = row.last_message_id {
                                // Use the authoritative server rows, including
                                // mention counts and version ordering, rather
                                // than synthesizing the originally requested ID.
                                let _=self.state.apply_gateway("MESSAGE_ACK",json!({"channel_id":row.id,"message_id":message,"mention_count":row.mention_count,"version":row.version}));
                            }
                        }
                        self.snapshot();
                    }
                    Ok(None) => {
                        let read = self.state.read_states.entry(channel).or_insert(
                            fluxer_core::ReadState {
                                id: channel,
                                ..Default::default()
                            },
                        );
                        read.last_message_id = read.last_message_id.max(Some(id));
                        if self
                            .state
                            .channels
                            .get(&channel)
                            .and_then(|channel| channel.last_message_id)
                            .is_some_and(|last| id >= last)
                        {
                            read.mention_count = 0;
                        }
                        self.snapshot();
                    }
                    Err(message) => self.emit(Event::Error(message)),
                }
            }
        }
    }
}

fn apply_history_page(
    snapshot: &mut HistorySnapshot,
    request: HistoryRequest,
    messages: &[Message],
    known_latest: Option<Snowflake>,
) {
    let mut ids = messages
        .iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    let count = messages.len();
    let filled = count == HISTORY_LIMIT as usize;
    match request {
        HistoryRequest::Latest => {
            snapshot.message_ids = ids;
            snapshot.has_older = filled;
            snapshot.has_newer = false;
            snapshot.latest = true;
            snapshot.target = None;
        }
        HistoryRequest::Before(edge) => {
            snapshot
                .message_ids
                .extend(ids.into_iter().filter(|id| *id < edge));
            snapshot.has_older = filled;
        }
        HistoryRequest::After(edge) => {
            snapshot
                .message_ids
                .extend(ids.into_iter().filter(|id| *id > edge));
            snapshot.has_newer = filled;
            snapshot.latest = !filled;
        }
        HistoryRequest::Around(target) => {
            let newer = ids.iter().filter(|id| **id > target).count();
            let older = ids.iter().filter(|id| **id < target).count();
            let at_latest = ids
                .last()
                .copied()
                .is_some_and(|id| Some(id) == known_latest);
            let found = ids.contains(&target);
            snapshot.has_older = older >= HISTORY_LIMIT as usize - 1 - HISTORY_LIMIT as usize / 2;
            snapshot.has_newer = if found {
                newer >= HISTORY_LIMIT as usize / 2 && !at_latest
            } else {
                !ids.is_empty() && ids.last().is_some_and(|id| *id > target) && !at_latest
            };
            snapshot.latest = !ids.is_empty() && !snapshot.has_newer;
            snapshot.message_ids = ids;
            snapshot.target = Some(target);
            if !found {
                snapshot.error = Some("The requested message is no longer available".into());
            }
        }
    }
    snapshot.message_ids.sort_unstable();
    snapshot.message_ids.dedup();
    if snapshot.message_ids.len() > 200 {
        let excess = snapshot.message_ids.len() - 200;
        if matches!(request, HistoryRequest::Before(_)) {
            snapshot.message_ids.truncate(200);
            snapshot.has_newer = true;
            snapshot.latest = false;
        } else {
            snapshot.message_ids.drain(..excess);
            snapshot.has_older = true;
        }
    }
}

fn demo_history(messages: &[Message], request: HistoryRequest) -> Vec<Message> {
    let mut sorted = messages.to_vec();
    sorted.sort_by_key(|message| message.id);
    let selected = match request {
        HistoryRequest::Latest => sorted
            .into_iter()
            .rev()
            .take(HISTORY_LIMIT as usize)
            .collect::<Vec<_>>(),
        HistoryRequest::Before(id) => sorted
            .into_iter()
            .rev()
            .filter(|message| message.id < id)
            .take(HISTORY_LIMIT as usize)
            .collect(),
        HistoryRequest::After(id) => sorted
            .into_iter()
            .filter(|message| message.id > id)
            .take(HISTORY_LIMIT as usize)
            .collect(),
        HistoryRequest::Around(id) => {
            let index = sorted
                .iter()
                .position(|message| message.id >= id)
                .unwrap_or(sorted.len());
            let start = index.saturating_sub(HISTORY_LIMIT as usize / 2 - 1);
            sorted
                .into_iter()
                .skip(start)
                .take(HISTORY_LIMIT as usize)
                .collect()
        }
    };
    let mut selected = selected;
    selected.sort_by_key(|message| std::cmp::Reverse(message.id));
    selected
}

#[cfg(test)]
#[path = "worker_navigation_tests.rs"]
mod tests;
