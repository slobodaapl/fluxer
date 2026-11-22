//! Background search, pins and history. Results carry account and request
//! generations; loading detached results never changes the live read cursor.
use super::*;
use crate::{
    HistorySnapshot, PinnedMessage, PinsSnapshot, SearchScope, SearchSnapshot, SearchSort,
    SearchStatus,
};
use fluxer_core::Channel;
use fluxer_protocol::SearchResponse;
use std::collections::VecDeque;

const HISTORY_LIMIT: u8 = 50;
const HISTORY_WINDOW: usize = 200;
const CACHED_MESSAGES_PER_CHANNEL: usize = HISTORY_WINDOW + 250;
const CACHED_CHANNELS: usize = 50;
fn merge_forum_patch(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                merge_forum_patch(target.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        (target, patch) => *target = patch.clone(),
    }
}

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
    ArchivedThreads {
        epoch: u64,
        token: u64,
        channel: Snowflake,
        before: Option<String>,
        result: Result<Value>,
    },
    Forum {
        epoch: u64,
        token: u64,
        channel: Snowflake,
        offset: usize,
        result: Result<Value>,
    },
    Mentions {
        epoch: u64,
        token: u64,
        before: Option<Snowflake>,
        result: Result<Value>,
    },
    Slowmode {
        epoch: u64,
        channel: Snowflake,
        token: u64,
        result: Result<Value>,
    },
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
struct MentionsJob {
    token: u64,
    snapshot: crate::MentionsSnapshot,
    task: Option<Task>,
}
struct ArchivedThreadsJob {
    token: u64,
    _task: Task,
    channel_changes: BTreeMap<Snowflake, Option<Value>>,
    membership_changes: BTreeMap<Snowflake, Option<Value>>,
}

pub(super) struct NavigationState {
    forum: Option<(u64, Task, BTreeMap<Snowflake, Option<Value>>)>,
    archived_threads: Option<ArchivedThreadsJob>,
    mentions: Option<MentionsJob>,
    epoch: u64,
    next: u64,
    searches: BTreeMap<Snowflake, SearchJob>,
    result_channels: BTreeMap<Snowflake, Channel>,
    pins: BTreeMap<Snowflake, PinsJob>,
    history: BTreeMap<Snowflake, HistoryJob>,
    recent_message_channels: VecDeque<Snowflake>,
    mutations: BTreeMap<(Snowflake, Snowflake), Mutation>,
    acks: BTreeMap<Snowflake, AckJob>,
    pin_acks: BTreeMap<Snowflake, Mutation>,
    slowmode: Option<(Snowflake, u64, Task)>,
    demo_archive: BTreeMap<Snowflake, Vec<Message>>,
    demo_pins: BTreeMap<Snowflake, Vec<PinnedMessage>>,
    tx: async_mpsc::UnboundedSender<NavigationEvent>,
    pub rx: async_mpsc::UnboundedReceiver<NavigationEvent>,
}
impl NavigationState {
    pub fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        Self {
            forum: None,
            archived_threads: None,
            mentions: None,
            epoch: 1,
            next: 0,
            searches: BTreeMap::new(),
            result_channels: BTreeMap::new(),
            pins: BTreeMap::new(),
            history: BTreeMap::new(),
            recent_message_channels: VecDeque::new(),
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
    pub(super) fn load_archived_threads(
        &mut self,
        channel: Snowflake,
        before: Option<String>,
    ) -> Result<()> {
        let parent = self.state.channels.get(&channel).ok_or("Unknown channel")?;
        let guild = parent.guild_id.ok_or("Unknown community")?;
        let user = self.state.current_user.as_ref().ok_or("Sign in first")?;
        if parent.kind.standalone_thread_type().is_none()
            || !self.state.thread_guilds.contains(&guild)
            || !self
                .state
                .permissions_for(channel, user.id)
                .contains(Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY)
        {
            return Err("You cannot view archived threads in this channel".into());
        }
        let client = self.client()?;
        let epoch = self.navigation.epoch;
        let token = self.navigation.token();
        let tx = self.navigation.tx.clone();
        let task = tokio::spawn(async move {
            let result = client
                .archived_public_threads(&channel.to_string(), before.as_deref())
                .await
                .map_err(error);
            let _ = tx.send(NavigationEvent::ArchivedThreads {
                epoch,
                token,
                channel,
                before,
                result,
            });
        });
        self.navigation.archived_threads = Some(ArchivedThreadsJob {
            token,
            _task: Task(task),
            channel_changes: BTreeMap::new(),
            membership_changes: BTreeMap::new(),
        });
        Ok(())
    }

    pub(super) fn load_forum(
        &mut self,
        channel: Snowflake,
        creation_order: bool,
        offset: usize,
        tags: Vec<String>,
        match_all: bool,
    ) -> Result<()> {
        let target = self.state.channels.get(&channel).ok_or("Unknown forum")?;
        if !matches!(target.kind, ChannelType::Forum | ChannelType::Media) {
            return Err("Choose a forum".into());
        }
        let user = self.state.current_user.as_ref().ok_or("Sign in first")?;
        if !self
            .state
            .permissions_for(channel, user.id)
            .contains(Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY)
        {
            return Err("You cannot view this forum".into());
        }
        let client = if self.demo {
            None
        } else {
            Some(self.client()?)
        };
        let demo = self
            .demo
            .then(|| self.demo_forum_page(channel, creation_order, offset, &tags, match_all));
        let epoch = self.navigation.epoch;
        let token = self.navigation.token();
        let tx = self.navigation.tx.clone();
        let task = tokio::spawn(async move {
            let result = match (client, demo) {
                (Some(client), _) => client
                    .forum_posts(
                        &channel.to_string(),
                        if creation_order {
                            "creation_time"
                        } else {
                            "last_message_time"
                        },
                        offset,
                        &tags,
                        match_all,
                    )
                    .await
                    .map_err(error),
                (None, Some(page)) => Ok(page),
                (None, None) => Err("Forum posts are unavailable".to_owned()),
            };
            let _ = tx.send(NavigationEvent::Forum {
                epoch,
                token,
                channel,
                offset,
                result,
            });
        });
        self.navigation.forum = Some((token, Task(task), BTreeMap::new()));
        Ok(())
    }

    fn demo_forum_page(
        &self,
        channel: Snowflake,
        creation_order: bool,
        offset: usize,
        tags: &[String],
        match_all: bool,
    ) -> Value {
        // Small pages let the offline demo exercise pagination with just three posts.
        const DEMO_PAGE_SIZE: usize = 2;
        let mut threads: Vec<_> = self
            .state
            .channels
            .values()
            .filter(|post| post.parent_id == Some(channel) && post.kind.is_thread())
            .filter(|post| {
                if tags.is_empty() {
                    return true;
                }
                let has_tag = |tag: &String| {
                    post.extra
                        .get("applied_tags")
                        .and_then(Value::as_array)
                        .is_some_and(|applied| {
                            applied.iter().any(|id| id.as_str() == Some(tag.as_str()))
                        })
                };
                if match_all {
                    tags.iter().all(has_tag)
                } else {
                    tags.iter().any(has_tag)
                }
            })
            .collect();
        threads.sort_by_key(|post| {
            std::cmp::Reverse(if creation_order {
                post.id
            } else {
                post.last_message_id.unwrap_or(post.id)
            })
        });
        let has_more = threads.len().saturating_sub(offset) > DEMO_PAGE_SIZE;
        let threads: Vec<_> = threads
            .into_iter()
            .skip(offset)
            .take(DEMO_PAGE_SIZE)
            .collect();
        let previews: Vec<_> = threads
            .iter()
            .filter_map(|post| self.state.channel_messages(post.id).next())
            .collect();
        json!({"threads": threads, "first_messages": previews, "has_more": has_more})
    }

    /// Preserve gateway changes that race a forum search, including updates to
    /// posts that have not yet been installed in the channel cache.
    pub(super) fn navigation_forum_thread_event(&mut self, name: &str, data: &Value) {
        let Some(id) = data
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse().ok())
        else {
            return;
        };
        for changes in self
            .navigation
            .forum
            .iter_mut()
            .map(|(_, _, changes)| changes)
            .chain(
                self.navigation
                    .archived_threads
                    .iter_mut()
                    .map(|job| &mut job.channel_changes),
            )
        {
            if name == "THREAD_DELETE" {
                changes.insert(id, None);
            } else if name == "THREAD_UPDATE" {
                let entry = changes
                    .entry(id)
                    .or_insert_with(|| Some(Value::Object(Default::default())));
                if let Some(existing) = entry.as_mut() {
                    merge_forum_patch(existing, data);
                }
            }
        }
    }

    /// Record self membership changes even when the archived thread is not cached yet.
    pub(super) fn navigation_archive_membership_event(&mut self, name: &str, data: &Value) {
        let Some(user) = self.state.current_user.as_ref().map(|user| user.id) else {
            return;
        };
        let Some(job) = self.navigation.archived_threads.as_mut() else {
            return;
        };
        let changes = &mut job.membership_changes;
        let Some(id) = data
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<Snowflake>().ok())
        else {
            return;
        };
        if name == "THREAD_MEMBER_UPDATE" {
            if data
                .get("user_id")
                .and_then(Value::as_str)
                .and_then(|id| id.parse::<Snowflake>().ok())
                == Some(user)
            {
                changes.insert(id, Some(data.clone()));
            }
        } else if name == "THREAD_MEMBERS_UPDATE" {
            if data
                .get("removed_member_ids")
                .and_then(Value::as_array)
                .is_some_and(|ids| {
                    ids.iter().any(|id| {
                        id.as_str().and_then(|id| id.parse::<Snowflake>().ok()) == Some(user)
                    })
                })
            {
                changes.insert(id, None);
            } else if let Some(member) = data
                .get("added_members")
                .and_then(Value::as_array)
                .and_then(|members| {
                    members.iter().find(|member| {
                        member
                            .get("user_id")
                            .and_then(Value::as_str)
                            .and_then(|id| id.parse::<Snowflake>().ok())
                            == Some(user)
                    })
                })
            {
                changes.insert(id, Some(member.clone()));
            }
        }
    }

    pub(super) fn navigation_mention_message(&mut self, message: &Message) {
        let Some(job) = self.navigation.mentions.as_ref() else {
            return;
        };
        let filters = job.snapshot.filters;
        let Some(channel) = self.state.channels.get(&message.channel_id) else {
            return;
        };
        if !job.snapshot.fetched
            || !self.state.message_mentions_current_user(message, false)
            || self
                .state
                .relationships
                .get(&message.author.id)
                .is_some_and(|r| r.is_blocked())
            || !self.state.notification_content_allowed(channel)
            || (!filters.guilds && channel.guild_id.is_some())
            || (!filters.everyone && message.mention_everyone)
            || (!filters.roles && !message.mention_roles.is_empty())
        {
            return;
        }
        let job = self.navigation.mentions.as_mut().unwrap();
        job.snapshot
            .messages
            .retain(|existing| existing.id != message.id);
        job.snapshot.messages.insert(0, message.clone());
        self.emit(Event::Mentions(
            self.navigation.mentions.as_ref().unwrap().snapshot.clone(),
        ));
    }
    pub(super) fn navigation_close_mentions(&mut self) {
        if let Some(job) = self.navigation.mentions.as_mut() {
            job.task = None;
            job.token = job.token.wrapping_add(1);
            job.snapshot.loading = false;
        }
    }
    pub(super) fn fetch_mentions(
        &mut self,
        before: Option<Snowflake>,
        filters: crate::MentionFilters,
    ) {
        if before.is_some()
            && !self.navigation.mentions.as_ref().is_some_and(|job| {
                job.snapshot.filters == filters
                    && !job.snapshot.loading
                    && job.snapshot.has_more
                    && job.snapshot.messages.last().map(|message| message.id) == before
            })
        {
            return;
        }
        let token = self.navigation.token();
        let epoch = self.navigation.epoch;
        let mut snapshot = self
            .navigation
            .mentions
            .as_ref()
            .map(|job| job.snapshot.clone())
            .unwrap_or_else(|| crate::MentionsSnapshot {
                account_id: self.state.current_user.as_ref().map(|user| user.id),
                has_more: true,
                ..Default::default()
            });
        snapshot.filters = filters;
        snapshot.loading = true;
        snapshot.error = None;
        self.emit(Event::Mentions(snapshot.clone()));
        if self.demo {
            snapshot.loading = false;
            snapshot.fetched = true;
            snapshot.has_more = false;
            snapshot.messages = self
                .state
                .messages
                .values()
                .flat_map(|messages| messages.values())
                .filter(|message| {
                    self.state
                        .current_user
                        .as_ref()
                        .is_some_and(|me| message.mentions.iter().any(|user| user.id == me.id))
                })
                .map(|message| message.as_ref().clone())
                .collect();
            snapshot
                .messages
                .sort_by_key(|message| std::cmp::Reverse(message.id));
            self.navigation.mentions = Some(MentionsJob {
                token,
                snapshot: snapshot.clone(),
                task: None,
            });
            self.emit(Event::Mentions(snapshot));
            return;
        }
        let Some(client) = self.client.clone() else {
            snapshot.loading = false;
            snapshot.error = Some("Connect an account to load recent mentions".into());
            self.emit(Event::Mentions(snapshot));
            return;
        };
        let tx = self.navigation.tx.clone();
        let task = tokio::spawn(async move {
            let before_wire = before.map(|id| id.to_string());
            let result = tokio::time::timeout(
                Duration::from_secs(15),
                client.recent_mentions(
                    before_wire.as_deref(),
                    filters.everyone,
                    filters.roles,
                    filters.guilds,
                ),
            )
            .await
            .map_err(error)
            .and_then(|result| result.map_err(error));
            let _ = tx.send(NavigationEvent::Mentions {
                epoch,
                token,
                before,
                result,
            });
        });
        self.navigation.mentions = Some(MentionsJob {
            token,
            snapshot,
            task: Some(Task(task)),
        });
    }
    pub(super) fn request_channel_slowmode(&mut self, channel: Snowflake) {
        self.navigation.slowmode = None;
        if self.demo
            || self
                .state
                .channels
                .get(&channel)
                .is_none_or(|channel| channel.guild_id.is_none())
        {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let epoch = self.navigation.epoch;
        let token = self.navigation.token();
        let tx = self.navigation.tx.clone();
        let task = tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                client.channel_slowmode(&channel.to_string()),
            )
            .await
            .map_err(error)
            .and_then(|result| result.map_err(error));
            let _ = tx.send(NavigationEvent::Slowmode {
                epoch,
                channel,
                token,
                result,
            });
        });
        self.navigation.slowmode = Some((channel, token, Task(task)));
    }
    pub(super) fn clear_navigation(&mut self) {
        self.navigation.forum = None;
        self.navigation.archived_threads = None;
        self.navigation.epoch = self.navigation.epoch.wrapping_add(1);
        self.navigation.searches.clear();
        self.navigation.mentions = None;
        self.navigation.result_channels.clear();
        self.navigation.pins.clear();
        self.navigation.history.clear();
        self.navigation.recent_message_channels.clear();
        self.navigation.mutations.clear();
        self.navigation.acks.clear();
        self.navigation.pin_acks.clear();
        self.navigation.slowmode = None;
        self.navigation.demo_archive.clear();
        self.navigation.demo_pins.clear();
    }
    pub(super) fn navigation_context(&mut self, channel: Snowflake) {
        self.touch_message_channel(channel);
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
            }
        }
        self.navigation.acks.retain(|id, _| *id == channel);
    }

    fn touch_message_channel(&mut self, channel: Snowflake) {
        let recent = &mut self.navigation.recent_message_channels;
        recent.retain(|id| *id != channel);
        recent.push_back(channel);
        if recent.len() > CACHED_CHANNELS {
            recent.pop_front();
        }
    }

    pub(super) fn bound_message_cache(&mut self) {
        if self.demo {
            return;
        }
        let selected = self.state.selected_channel;
        let recent = &self.navigation.recent_message_channels;
        while self.state.messages.len() > CACHED_CHANNELS {
            let victim =
                self.state
                    .messages
                    .keys()
                    .copied()
                    .find(|id| Some(*id) != selected && !recent.contains(id))
                    .or_else(|| {
                        recent.iter().copied().find(|id| {
                            Some(*id) != selected && self.state.messages.contains_key(id)
                        })
                    });
            let Some(victim) = victim else { break };
            self.state.messages.remove(&victim);
            self.navigation.history.remove(&victim);
            self.emit(Event::HistoryCacheEvicted(victim));
        }
        for (channel, messages) in &mut self.state.messages {
            if messages.len() <= CACHED_MESSAGES_PER_CHANNEL {
                continue;
            }
            let history = self.navigation.history.get(channel);
            let mut keep = BTreeSet::new();
            if let Some(history) = history {
                keep.extend(history.snapshot.message_ids.iter().copied());
                // Live arrivals can race the HTTP response for a page.
                keep.extend(history.arrived.iter().rev().take(HISTORY_WINDOW).copied());
            }
            // Forums and message-started threads use the starter as a preview.
            if messages.contains_key(channel) {
                keep.insert(*channel);
            }
            // Keep the active window and use the rest of the budget for recent
            // records. Gaps outside the window must be fetched from the server.
            for id in messages.keys().rev() {
                if keep.len() >= CACHED_MESSAGES_PER_CHANNEL {
                    break;
                }
                keep.insert(*id);
            }
            messages.retain(|id| keep.contains(&id));
        }
        // Empty conversations and cancelled first pages may have no records.
        let mut expired = Vec::new();
        self.navigation.history.retain(|channel, _| {
            let keep = Some(*channel) == selected
                || self.state.messages.contains_key(channel)
                || (self.state.channels.contains_key(channel) && recent.contains(channel));
            if !keep {
                expired.push(*channel);
            }
            keep
        });
        for channel in expired {
            self.emit(Event::HistoryCacheEvicted(channel));
        }
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
    pub(super) fn open_channel_history(&mut self, channel: Snowflake) -> Result<()> {
        self.require_history(channel)?;
        let last = self.state.channels[&channel].last_message_id;
        if let Some(job) = self.navigation.history.get_mut(&channel)
            && !job.snapshot.loading
            && job.snapshot.error.is_none()
            && job.snapshot.latest
            && !job.snapshot.has_newer
            && job.snapshot.message_ids.last().copied() == last
            && job.snapshot.message_ids.iter().all(|id| {
                self.state
                    .messages
                    .get(&channel)
                    .is_some_and(|messages| messages.contains_key(id))
            })
        {
            // Reopening at the live edge only needs the latest page.
            let excess = job
                .snapshot
                .message_ids
                .len()
                .saturating_sub(HISTORY_LIMIT.into());
            if excess != 0 {
                job.snapshot.message_ids.drain(..excess);
                job.snapshot.has_older = true;
            }
            job.snapshot.target = None;
            let snapshot = job.snapshot.clone();
            self.emit(Event::History(snapshot));
            return Ok(());
        }
        self.request_history(channel, HistoryRequest::Latest)
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
        let demo = self.demo.then(|| {
            self.navigation
                .demo_archive
                .get(&channel)
                .cloned()
                .unwrap_or_else(|| self.state.channel_messages(channel).cloned().collect())
        });
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
                    demo_history(demo.as_deref().unwrap_or_default(), request)
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
        self.cancel_open_dm();
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
        self.persist_selection();
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
    pub(super) async fn set_guild_mention_suppression(
        &mut self,
        guild: Snowflake,
        roles: bool,
        suppressed: bool,
    ) -> Result<()> {
        if !self.state.guilds.contains_key(&guild) {
            return Err("Unknown notification community".into());
        }
        let key = if roles {
            "suppress_roles"
        } else {
            "suppress_everyone"
        };
        let mut body = serde_json::Map::new();
        body.insert(key.into(), json!(suppressed));
        if !self.demo {
            self.client()?
                .request(
                    fluxer_protocol::Method::PATCH,
                    &format!("/users/@me/guilds/{guild}/settings"),
                    Some(&Value::Object(body)),
                )
                .await
                .map_err(error)?;
        }
        let mut settings = self.state.guild_notification_settings(Some(guild));
        settings.extra.insert(key.into(), json!(suppressed));
        self.state.user_guild_settings.insert(Some(guild), settings);
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
        self.touch_message_channel(channel);
        let Some(history) = self.navigation.history.get_mut(&channel) else {
            return;
        };
        if history.snapshot.loading {
            history.arrived.insert(id);
            if history.arrived.len() > HISTORY_WINDOW {
                history.arrived.pop_first();
            }
            return;
        }
        if history.snapshot.latest {
            history.snapshot.message_ids.push(id);
            history.snapshot.message_ids.sort_unstable();
            history.snapshot.message_ids.dedup();
            if history.snapshot.message_ids.len() > HISTORY_WINDOW {
                let excess = history.snapshot.message_ids.len() - HISTORY_WINDOW;
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
            if let Some(job) = self.navigation.mentions.as_mut() {
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
                    self.emit(Event::Mentions(
                        self.navigation.mentions.as_ref().unwrap().snapshot.clone(),
                    ));
                }
            }
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
            NavigationEvent::ArchivedThreads {
                epoch,
                token,
                channel,
                before,
                result,
            } => {
                if epoch != self.navigation.epoch
                    || self.state.selected_channel != Some(channel)
                    || !self
                        .navigation
                        .archived_threads
                        .as_ref()
                        .is_some_and(|job| job.token == token)
                {
                    return;
                }
                let ArchivedThreadsJob {
                    channel_changes: changes,
                    membership_changes,
                    ..
                } = self.navigation.archived_threads.take().unwrap();
                let Some(guild) = self
                    .state
                    .channels
                    .get(&channel)
                    .and_then(|parent| parent.guild_id)
                else {
                    return;
                };
                let mut threads = Vec::new();
                let mut next = None;
                let mut has_more = false;
                let mut failure = None;
                match result {
                    Ok(body) => {
                        has_more = body
                            .get("has_more")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        if let Some(items) = body.get("threads").and_then(Value::as_array) {
                            next = items
                                .last()
                                .and_then(|item| item.get("thread_metadata"))
                                .and_then(|metadata| metadata.get("archive_timestamp"))
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                            for item in items {
                                let mut value = item.clone();
                                let id = value
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .and_then(|s| s.parse::<Snowflake>().ok());
                                if let Some(change) = id.and_then(|id| changes.get(&id)) {
                                    let Some(patch) = change else {
                                        continue;
                                    };
                                    if let Some(current) =
                                        id.and_then(|id| self.state.channels.get(&id))
                                    {
                                        value = serde_json::to_value(current).unwrap_or(value);
                                    } else {
                                        merge_forum_patch(&mut value, patch);
                                    }
                                }
                                if let Some(member) = body
                                    .get("members")
                                    .and_then(Value::as_array)
                                    .and_then(|members| {
                                        members.iter().find(|m| m.get("id") == value.get("id"))
                                    })
                                {
                                    value["member"] = member.clone();
                                }
                                if let Some(change) = id.and_then(|id| membership_changes.get(&id))
                                {
                                    if let Some(member) = change {
                                        value["member"] = member.clone();
                                    } else if let Some(object) = value.as_object_mut() {
                                        object.remove("member");
                                    }
                                }
                                if let Ok(thread) = serde_json::from_value::<Channel>(value) {
                                    if thread.parent_id != Some(channel)
                                        || thread.guild_id != Some(guild)
                                        || !matches!(
                                            thread.kind,
                                            ChannelType::PublicThread
                                                | ChannelType::AnnouncementThread
                                        )
                                    {
                                        continue;
                                    }
                                    let id = thread.id;
                                    self.state.channels.insert(id, thread);
                                    threads.push(id);
                                }
                            }
                        } else {
                            failure = Some("Invalid archived thread response".into());
                        }
                    }
                    Err(error) => failure = Some(error),
                }
                self.snapshot();
                self.emit(Event::ArchivedThreads {
                    channel_id: channel,
                    before,
                    threads,
                    next,
                    has_more,
                    error: failure,
                });
            }
            NavigationEvent::Forum {
                epoch,
                token,
                channel,
                offset,
                result,
            } => {
                if epoch != self.navigation.epoch
                    || !self
                        .navigation
                        .forum
                        .as_ref()
                        .is_some_and(|(current, _, _)| *current == token)
                    || self.state.selected_channel != Some(channel)
                {
                    return;
                }
                let (_, _, changes) = self.navigation.forum.take().unwrap();
                let mut posts = Vec::new();
                let mut has_more = false;
                let mut next_offset = offset;
                let mut failure = None;
                match result {
                    Ok(body)
                        if body.get("code").and_then(Value::as_str)
                            == Some("SEARCH_INDEX_NOT_READY") =>
                    {
                        failure =
                            Some("Forum search index is still building. Try again later.".into())
                    }
                    Ok(body) => {
                        has_more = body
                            .get("has_more")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        let previews: Vec<Message> = body
                            .get("first_messages")
                            .cloned()
                            .and_then(|v| serde_json::from_value(v).ok())
                            .unwrap_or_default();
                        if let Some(threads) = body.get("threads").and_then(Value::as_array) {
                            next_offset += threads.len();
                            for value in threads {
                                let mut value = value.clone();
                                let id = value
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .and_then(|id| id.parse::<Snowflake>().ok());
                                if let Some(change) = id.and_then(|id| changes.get(&id)) {
                                    let Some(patch) = change else {
                                        continue;
                                    };
                                    if let Some(current) =
                                        id.and_then(|id| self.state.channels.get(&id))
                                    {
                                        value = serde_json::to_value(current).unwrap_or(value);
                                    } else {
                                        merge_forum_patch(&mut value, patch);
                                    }
                                }
                                if let Ok(post) =
                                    serde_json::from_value::<fluxer_core::Channel>(value)
                                {
                                    if post.parent_id != Some(channel) {
                                        continue;
                                    }
                                    let preview =
                                        previews.iter().find(|m| m.channel_id == post.id).cloned();
                                    self.state.channels.insert(post.id, post.clone());
                                    posts.push((post, preview));
                                }
                            }
                        } else {
                            failure = Some("Invalid forum response".into());
                        }
                    }
                    Err(e) => failure = Some(e),
                }
                self.snapshot();
                self.emit(Event::Forum {
                    channel_id: channel,
                    posts,
                    offset,
                    next_offset,
                    has_more,
                    error: failure,
                });
            }
            NavigationEvent::Mentions {
                epoch,
                token,
                before,
                result,
            } => {
                if epoch != self.navigation.epoch
                    || !self
                        .navigation
                        .mentions
                        .as_ref()
                        .is_some_and(|job| job.token == token)
                {
                    return;
                }
                let result = result.and_then(|body| {
                    let rows = body
                        .as_array()
                        .ok_or("Recent mentions returned an invalid response")?;
                    rows.iter()
                        .cloned()
                        .map(|row| self.state.decode_api::<Message>(row).map_err(error))
                        .collect::<Result<Vec<_>>>()
                });
                let job = self.navigation.mentions.as_mut().unwrap();
                job.task = None;
                job.snapshot.loading = false;
                match result {
                    Ok(messages) => {
                        job.snapshot.fetched = true;
                        job.snapshot.has_more = messages.len() == 50;
                        if before.is_none() {
                            job.snapshot.messages.clear();
                        }
                        for message in messages {
                            if !job
                                .snapshot
                                .messages
                                .iter()
                                .any(|existing| existing.id == message.id)
                            {
                                job.snapshot.messages.push(message);
                            }
                        }
                        job.snapshot
                            .messages
                            .sort_by_key(|message| std::cmp::Reverse(message.id));
                    }
                    Err(error) => job.snapshot.error = Some(error),
                }
                self.emit(Event::Mentions(
                    self.navigation.mentions.as_ref().unwrap().snapshot.clone(),
                ));
            }
            NavigationEvent::Slowmode {
                epoch,
                channel,
                token,
                result,
            } => {
                if epoch != self.navigation.epoch
                    || self.state.selected_channel != Some(channel)
                    || !self
                        .navigation
                        .slowmode
                        .as_ref()
                        .is_some_and(|(id, active, _)| *id == channel && *active == token)
                {
                    return;
                }
                self.navigation.slowmode = None;
                let Ok(body) = result else {
                    return;
                };
                let (Some(rate), Some(retry), Some(can_bypass)) = (
                    body.get("rate_limit_per_user").and_then(Value::as_u64),
                    body.get("retry_after_ms").and_then(Value::as_u64),
                    body.get("can_bypass").and_then(Value::as_bool),
                ) else {
                    return;
                };
                if rate > 21600 || retry > 21600000 {
                    return;
                }
                if let Some(target) = self.state.channels.get_mut(&channel) {
                    target.rate_limit_per_user = rate as u32;
                }
                self.emit(Event::ChannelSlowmode {
                    channel_id: channel,
                    rate_limit_per_user: rate as u32,
                    retry_after: Duration::from_millis(retry),
                    can_bypass,
                });
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
                            if job.snapshot.message_ids.len() > HISTORY_WINDOW {
                                let excess = job.snapshot.message_ids.len() - HISTORY_WINDOW;
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
    if snapshot.message_ids.len() > HISTORY_WINDOW {
        let excess = snapshot.message_ids.len() - HISTORY_WINDOW;
        if matches!(request, HistoryRequest::Before(_)) {
            snapshot.message_ids.truncate(HISTORY_WINDOW);
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
