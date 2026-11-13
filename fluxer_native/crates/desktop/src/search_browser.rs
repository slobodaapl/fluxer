// SPDX-License-Identifier: AGPL-3.0-or-later
//! Server-owned search/pins/history state, kept separate from the live timeline.
use super::*;
use crate::{
    controls::{Icon, icon_button, quiet_button},
    message_actions::{ConfirmationChoice, ConfirmationKeys, MessageMenuContext, message_time},
};
use fluxer_client::{
    HistorySnapshot, PinsSnapshot, SearchScope, SearchSnapshot, SearchSort, SearchStatus,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BrowserTab {
    Search,
    Pins,
}
pub(super) struct PinConfirmation {
    pub message: Message,
    pub pinned: bool,
}
pub(super) struct SearchUi {
    pub open: Option<BrowserTab>,
    pub context: Option<Snowflake>,
    pub pins_context: Option<Snowflake>,
    pub result: Option<SearchSnapshot>,
    pub pins: BTreeMap<Snowflake, PinsSnapshot>,
    pub histories: BTreeMap<Snowflake, HistorySnapshot>,
    pub scope: Entity<Dropdown>,
    pub sort: Entity<Dropdown>,
    results_list: ListState,
    pins_list: ListState,
    result_ids: Vec<(Snowflake, Snowflake)>,
    pin_ids: Vec<Snowflake>,
    text: BTreeMap<(Snowflake, Snowflake), Entity<TextInput>>,
    pub confirm: Option<PinConfirmation>,
    pin_cancel_focus: FocusHandle,
    pin_confirm_focus: FocusHandle,
    pin_dialog_focus: FocusHandle,
    pin_previous_focus: Option<FocusHandle>,
    pin_keys: ConfirmationKeys,
    pub mutations: BTreeSet<(Snowflake, Snowflake)>,
    pub mutation_error: Option<String>,
    pub local_error: Option<String>,
    pub highlight: Option<(Snowflake, Snowflake)>,
    pub pending_center: Option<(Snowflake, Snowflake)>,
    center_scheduled: bool,
    pending_latest: Option<Snowflake>,
    latest_scheduled: bool,
    inflight_ack: BTreeMap<Snowflake, Snowflake>,
    failed_ack: BTreeSet<Snowflake>,
    returning_latest: BTreeSet<Snowflake>,
    pending_ack: BTreeMap<Snowflake, Snowflake>,
    ack_scheduled: bool,
    demo_started: bool,
}
impl SearchUi {
    pub(super) fn cached_text_keys(&self) -> impl Iterator<Item = (Snowflake, Snowflake)> + '_ {
        self.text.keys().copied()
    }

    pub fn return_to_latest_on_select(&mut self, channel: Snowflake) {
        self.returning_latest.insert(channel);
    }
    pub fn new(window: &mut Window, cx: &mut Context<Fluxer>) -> Self {
        Self {
            open: None,
            context: None,
            pins_context: None,
            result: None,
            pins: BTreeMap::new(),
            histories: BTreeMap::new(),
            scope: picker(
                "Search scope",
                &[
                    ("current", "This community"),
                    ("all_dms", "All DMs"),
                    ("open_dms", "Open DMs"),
                    ("all", "All DMs + communities"),
                    ("open_dms_and_all_guilds", "Open DMs + communities"),
                    ("all_guilds", "All communities"),
                ],
                Some("current"),
                window,
                cx,
            ),
            sort: picker(
                "Sort",
                &[
                    ("newest", "Newest"),
                    ("oldest", "Oldest"),
                    ("relevant", "Most relevant"),
                ],
                Some("newest"),
                window,
                cx,
            ),
            results_list: ListState::new(0, ListAlignment::Top, px(180.)),
            pins_list: ListState::new(0, ListAlignment::Top, px(180.)),
            result_ids: Vec::new(),
            pin_ids: Vec::new(),
            text: BTreeMap::new(),
            confirm: None,
            pin_cancel_focus: cx.focus_handle().tab_stop(true),
            pin_confirm_focus: cx.focus_handle().tab_stop(true),
            pin_dialog_focus: cx.focus_handle(),
            pin_previous_focus: None,
            pin_keys: ConfirmationKeys::default(),
            mutations: BTreeSet::new(),
            mutation_error: None,
            local_error: None,
            highlight: None,
            pending_center: None,
            center_scheduled: false,
            pending_latest: None,
            latest_scheduled: false,
            inflight_ack: BTreeMap::new(),
            failed_ack: BTreeSet::new(),
            returning_latest: BTreeSet::new(),
            pending_ack: BTreeMap::new(),
            ack_scheduled: false,
            demo_started: false,
        }
    }
}
impl Fluxer {
    pub(super) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_ui.open != Some(BrowserTab::Search) || self.search_ui.context.is_none() {
            self.search_ui.context = self.state.selected_channel;
        }
        self.search_ui.open = Some(BrowserTab::Search);
        self.pane = Pane::Chat;
        let private = self
            .search_ui
            .context
            .and_then(|id| self.state.channels.get(&id))
            .is_some_and(|channel| channel.guild_id.is_none());
        let mut options = vec![
            (
                "current".into(),
                if private { "This DM" } else { "This community" }.into(),
            ),
            ("all_dms".into(), "All DMs".into()),
            ("open_dms".into(), "Open DMs".into()),
            ("all".into(), "All DMs + communities".into()),
            (
                "open_dms_and_all_guilds".into(),
                "Open DMs + communities".into(),
            ),
        ];
        if !private {
            options.push(("all_guilds".into(), "All communities".into()));
        }
        self.search_ui
            .scope
            .update(cx, |picker, cx| picker.set_options(options, cx));
        self.search.focus_handle(cx).focus(window);
        cx.notify();
    }
    pub(super) fn open_pins(&mut self, cx: &mut Context<Self>) {
        let Some(channel_id) = self.state.selected_channel else {
            return;
        };
        self.search_ui.open = Some(BrowserTab::Pins);
        self.search_ui.pins_context = Some(channel_id);
        self.pane = Pane::Chat;
        self.command(
            Command::LoadPins {
                channel_id,
                more: false,
            },
            cx,
        );
    }
    pub(super) fn close_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.search_ui.open.take() {
            Some(BrowserTab::Search) => {
                if let Some(channel_id) = self.search_ui.context {
                    self.command(Command::CloseSearch { channel_id }, cx);
                }
            }
            Some(BrowserTab::Pins) => {
                if let Some(channel_id) = self.search_ui.pins_context {
                    self.command(Command::ClosePins { channel_id }, cx);
                }
            }
            None => {}
        }
        self.composer.focus_handle(cx).focus(window);
        cx.notify();
    }
    pub(super) fn clear_browser(&mut self) {
        self.search_ui.open = None;
        self.search_ui.context = None;
        self.search_ui.pins_context = None;
        self.search_ui.result = None;
        self.search_ui.pins.clear();
        self.search_ui.histories.clear();
        self.search_ui.text.clear();
        self.search_ui.result_ids.clear();
        self.search_ui.pin_ids.clear();
        self.search_ui.results_list.reset(0);
        self.search_ui.pins_list.reset(0);
        self.search_ui.confirm = None;
        self.search_ui.pin_keys.clear();
        self.search_ui.pin_previous_focus = None;
        self.search_ui.mutations.clear();
        self.search_ui.mutation_error = None;
        self.search_ui.local_error = None;
        self.search_ui.highlight = None;
        self.search_ui.pending_center = None;
        self.search_ui.center_scheduled = false;
        self.search_ui.pending_latest = None;
        self.search_ui.latest_scheduled = false;
        self.search_ui.inflight_ack.clear();
        self.search_ui.failed_ack.clear();
        self.search_ui.returning_latest.clear();
        self.search_ui.pending_ack.clear();
        self.search_ui.ack_scheduled = false;
    }
    pub(super) fn change_search_scope(&mut self, cx: &mut Context<Self>) {
        if self.search_ui.open == Some(BrowserTab::Search)
            && !self.search.read(cx).content.trim().is_empty()
        {
            let scope = self.selected_search_scope(cx);
            self.run_search_override(1, Some(scope), cx);
        }
    }
    pub(super) fn change_search_sort(&mut self, cx: &mut Context<Self>) {
        if self.search_ui.open == Some(BrowserTab::Search)
            && !self.search.read(cx).content.trim().is_empty()
        {
            let scope = self.search_ui.result.as_ref().map(|result| result.scope);
            self.run_search_override(1, scope, cx);
        }
    }
    fn selected_search_scope(&self, cx: &App) -> SearchScope {
        match self
            .search_ui
            .scope
            .read(cx)
            .selected()
            .unwrap_or("current")
        {
            "all_dms" => SearchScope::AllDms,
            "open_dms" => SearchScope::OpenDms,
            "all" => SearchScope::All,
            "open_dms_and_all_guilds" => SearchScope::OpenDmsAndAllGuilds,
            "all_guilds" => SearchScope::AllGuilds,
            _ => SearchScope::Current,
        }
    }
    fn search_page(&mut self, page: u32, cx: &mut Context<Self>) {
        if let Some(snapshot) = &self.search_ui.result {
            let command = Command::SearchMessages {
                channel_id: snapshot.channel_id,
                query: snapshot.query.clone(),
                scope: snapshot.scope,
                scope_override: Some(snapshot.scope),
                sort: snapshot.sort,
                page: page.max(1),
            };
            self.command(command, cx);
        }
    }
    pub(super) fn run_search(&mut self, page: u32, cx: &mut Context<Self>) {
        self.run_search_override(page, None, cx);
    }
    fn run_search_override(
        &mut self,
        page: u32,
        scope_override: Option<SearchScope>,
        cx: &mut Context<Self>,
    ) {
        let Some(channel_id) = self.search_ui.context.or(self.state.selected_channel) else {
            return;
        };
        let query = self.search.read(cx).content.to_string();
        if query.trim().is_empty() {
            self.search_ui.local_error = Some("Enter text or a filter, such as has:image".into());
            cx.notify();
            return;
        }
        self.search_ui.local_error = None;
        let scope = self.selected_search_scope(cx);
        let sort = match self.search_ui.sort.read(cx).selected().unwrap_or("newest") {
            "oldest" => SearchSort::Oldest,
            "relevant" => SearchSort::Relevant,
            _ => SearchSort::Newest,
        };
        self.command(
            Command::SearchMessages {
                channel_id,
                query,
                scope,
                scope_override,
                sort,
                page: page.max(1),
            },
            cx,
        );
    }
    pub(super) fn on_search_snapshot(&mut self, snapshot: SearchSnapshot, cx: &mut Context<Self>) {
        if self.search_ui.context != Some(snapshot.channel_id) {
            return;
        }
        self.search_ui.scope.update(cx, |picker, cx| {
            picker.set_selected(snapshot.scope.as_str(), cx)
        });
        let sort = match snapshot.sort {
            SearchSort::Newest => "newest",
            SearchSort::Oldest => "oldest",
            SearchSort::Relevant => "relevant",
        };
        self.search_ui
            .sort
            .update(cx, |picker, cx| picker.set_selected(sort, cx));
        let ids = snapshot
            .messages
            .iter()
            .map(|message| (message.channel_id, message.id))
            .collect::<Vec<_>>();
        if ids != self.search_ui.result_ids {
            self.search_ui.results_list.reset(ids.len());
            self.search_ui.result_ids = ids;
            self.search_ui.text.clear();
        }
        self.search_ui.result = Some(snapshot);
        cx.notify();
    }
    pub(super) fn on_pins_snapshot(&mut self, snapshot: PinsSnapshot, cx: &mut Context<Self>) {
        if self.search_ui.pins_context == Some(snapshot.channel_id) {
            let ids = snapshot
                .items
                .iter()
                .map(|item| item.message.id)
                .collect::<Vec<_>>();
            if ids != self.search_ui.pin_ids {
                if ids.starts_with(&self.search_ui.pin_ids) {
                    self.search_ui.pins_list.splice(
                        self.search_ui.pin_ids.len()..self.search_ui.pin_ids.len(),
                        ids.len() - self.search_ui.pin_ids.len(),
                    );
                } else {
                    self.search_ui.pins_list.reset(ids.len());
                }
                self.search_ui.pin_ids = ids;
            }
        }
        self.search_ui.pins.insert(snapshot.channel_id, snapshot);
        cx.notify();
    }
    pub(super) fn confirmed_pin(&mut self, channel: Snowflake, id: Snowflake, pinned: bool) {
        if let Some(snapshot) = &mut self.search_ui.result {
            for message in &mut snapshot.messages {
                if message.channel_id == channel && message.id == id {
                    message.pinned = pinned;
                }
            }
        }
    }
    pub(super) fn on_history_snapshot(
        &mut self,
        snapshot: HistorySnapshot,
        cx: &mut Context<Self>,
    ) {
        if snapshot.loading {
            self.reset_ack_context(snapshot.channel_id);
            if !self.search_ui.histories.contains_key(&snapshot.channel_id)
                && snapshot.latest
                && snapshot.target.is_none()
            {
                self.search_ui.returning_latest.insert(snapshot.channel_id);
            }
        }
        if consume_latest_ready(&mut self.search_ui.returning_latest, &snapshot) {
            self.search_ui.pending_latest = Some(snapshot.channel_id);
            self.search_ui.latest_scheduled = false;
        }
        if !snapshot.loading
            && let Some(target) = snapshot.target
        {
            self.search_ui.highlight = Some((snapshot.channel_id, target));
            if self.search_ui.pending_center == Some((snapshot.channel_id, target)) {
                self.search_ui.center_scheduled = false;
            }
        }
        if !snapshot.loading && snapshot.latest && snapshot.target.is_none() {
            self.search_ui.highlight = None;
        }
        self.search_ui
            .histories
            .insert(snapshot.channel_id, snapshot);
        cx.notify();
    }
    pub(super) fn visible_messages(&self) -> Vec<Message> {
        visible_window_messages(&self.state, &self.search_ui.histories)
    }
    pub(super) fn jump_message(&mut self, message: &Message, cx: &mut Context<Self>) {
        self.cancel_edit(cx);
        if self.state.selected_channel.is_some() {
            self.command(
                Command::SetDraft(self.composer.read(cx).content.to_string()),
                cx,
            );
        }
        if let Some(guild) = self
            .state
            .channels
            .get(&message.channel_id)
            .and_then(|channel| channel.guild_id)
        {
            self.selected_guild = Some(guild);
        }
        self.reset_ack_context(message.channel_id);
        self.search_ui.returning_latest.remove(&message.channel_id);
        self.search_ui.pending_latest = None;
        self.search_ui.latest_scheduled = false;
        self.search_ui.pending_center = Some((message.channel_id, message.id));
        self.search_ui.center_scheduled = false;
        self.search_ui.highlight = Some((message.channel_id, message.id));
        self.pane = Pane::Chat;
        self.command(
            Command::JumpToMessage {
                channel_id: message.channel_id,
                message_id: message.id,
            },
            cx,
        );
    }
    pub(super) fn schedule_history_center(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(channel) = self.search_ui.pending_latest
            && !self.search_ui.latest_scheduled
            && self.state.selected_channel == Some(channel)
            && self
                .search_ui
                .histories
                .get(&channel)
                .is_some_and(|history| {
                    !history.loading && history.latest && history.error.is_none()
                })
        {
            self.search_ui.latest_scheduled = true;
            let entity = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = entity.update(cx, |this, cx| {
                    if this.search_ui.pending_latest == Some(channel)
                        && this.state.selected_channel == Some(channel)
                        && this
                            .search_ui
                            .histories
                            .get(&channel)
                            .is_some_and(|history| !history.loading && history.latest)
                    {
                        this.timeline.scroll_to_bottom();
                        this.search_ui.pending_latest = None;
                        cx.notify();
                    }
                    this.search_ui.latest_scheduled = false;
                });
            });
        }
        let Some((channel, target)) = self.search_ui.pending_center else {
            return;
        };
        if self.search_ui.center_scheduled
            || self.state.selected_channel != Some(channel)
            || self
                .search_ui
                .histories
                .get(&channel)
                .is_none_or(|history| history.loading)
        {
            return;
        }
        let Some(index) = self
            .visible_messages()
            .iter()
            .position(|message| message.id == target)
        else {
            return;
        };
        self.search_ui.center_scheduled = true;
        let entity = cx.entity().downgrade();
        window.on_next_frame(move |_, cx| {
            let _ = entity.update(cx, |this, cx| {
                if this.search_ui.pending_center != Some((channel, target))
                    || this.state.selected_channel != Some(channel)
                {
                    return;
                }
                if let Some(bounds) = this.timeline.bounds_for_item(index) {
                    let viewport = this.timeline.bounds();
                    let y = viewport.origin.y + viewport.size.height / 2.
                        - bounds.origin.y
                        - bounds.size.height / 2.;
                    this.timeline.set_offset(point(this.timeline.offset().x, y));
                    this.search_ui.pending_center = None;
                }
                this.search_ui.center_scheduled = false;
                cx.notify();
            });
        });
    }
    pub(super) fn history_banner(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(channel) = self.state.selected_channel else {
            return div().into_any_element();
        };
        let history = self.search_ui.histories.get(&channel);
        div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_1()
            .when_some(
                history.filter(|history| {
                    !history.latest || history.loading || history.error.is_some()
                }),
                |view, history| {
                    view.child(
                        div()
                            .px_4()
                            .py_2()
                            .bg(rgb(0x302b44))
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap_2()
                            .child(div().flex_1().min_w_0().text_size(px(12.)).child(
                                if history.loading {
                                    "Loading message history…"
                                } else {
                                    "You're viewing message history"
                                },
                            ))
                            .when(history.has_newer, |view| {
                                view.child(
                                    quiet_button("history-newer", "Load newer").py_1().on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.command(
                                                Command::LoadNewerMessages {
                                                    channel_id: channel,
                                                },
                                                cx,
                                            )
                                        }),
                                    ),
                                )
                            })
                            .child(
                                quiet_button("history-latest", "Return to latest")
                                    .py_1()
                                    .text_color(rgb(ACCENT))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.search_ui.returning_latest.insert(channel);
                                        this.search_ui.pending_center = None;
                                        this.search_ui.highlight = None;
                                        this.command(
                                            Command::ReturnToLatest {
                                                channel_id: channel,
                                            },
                                            cx,
                                        );
                                    })),
                            ),
                    )
                    .when_some(history.error.as_ref(), |view, error| {
                        view.child(
                            div()
                                .px_4()
                                .py_1()
                                .text_size(px(12.))
                                .text_color(rgb(0xf0a1b5))
                                .child(error.clone()),
                        )
                    })
                },
            )
            .when(history.is_none_or(|history| history.has_older), |view| {
                view.child(
                    div().flex().justify_center().child(
                        quiet_button("history-older", "Load older messages")
                            .my_1()
                            .py_1()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.command(
                                    Command::LoadOlderMessages {
                                        channel_id: channel,
                                    },
                                    cx,
                                )
                            })),
                    ),
                )
            })
            .into_any_element()
    }
    pub(super) fn visible_message_marker(
        &self,
        message: &Message,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity().downgrade();
        let (channel, id) = (message.channel_id, message.id);
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                if window.is_window_active() && bounds.intersects(&window.content_mask().bounds) {
                    let entity = entity.clone();
                    cx.defer(move |cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.note_visible_message(channel, id, cx);
                        });
                    });
                }
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
    pub(super) fn reset_ack_context(&mut self, channel: Snowflake) {
        self.search_ui.inflight_ack.remove(&channel);
        self.search_ui.failed_ack.remove(&channel);
        self.search_ui.pending_ack.remove(&channel);
    }
    pub(super) fn on_message_ack(
        &mut self,
        channel: Snowflake,
        id: Snowflake,
        error: Option<String>,
    ) {
        if self.search_ui.inflight_ack.get(&channel) != Some(&id) {
            return;
        }
        self.search_ui.inflight_ack.remove(&channel);
        if error.is_some() && self.state.selected_channel == Some(channel) {
            self.search_ui.failed_ack.insert(channel);
        }
    }
    fn auto_ack_allowed(&self, channel: Snowflake, cx: &mut Context<Self>) -> bool {
        let active = cx
            .windows()
            .iter()
            .find_map(|window| window.downcast::<Fluxer>())
            .and_then(|window| window.is_active(cx))
            .unwrap_or(false);
        let at_bottom = self.timeline.max_offset().height + self.timeline.offset().y <= px(4.);
        let latest = self
            .search_ui
            .histories
            .get(&channel)
            .is_some_and(|history| history.latest && !history.loading);
        let manual = self
            .state
            .read_states
            .get(&channel)
            .is_some_and(|state| state.manual_ack);
        let blocking = self.forwarding.is_some()
            || self.search_ui.confirm.is_some()
            || self.message_actions.is_blocking()
            || self.message_links.is_open()
            || self.image_viewer.is_open()
            || self.attachments.picker_open
            || self.session_restoring;
        auto_ack_policy(active, at_bottom, latest, manual, blocking)
    }
    fn note_visible_message(&mut self, channel: Snowflake, id: Snowflake, cx: &mut Context<Self>) {
        if !self.auto_ack_allowed(channel, cx)
            || self.search_ui.pending_center.is_some()
            || self.state.selected_channel != Some(channel)
            || self
                .search_ui
                .histories
                .get(&channel)
                .is_none_or(|history| history.loading || !history.message_ids.contains(&id))
        {
            return;
        }
        self.search_ui
            .pending_ack
            .entry(channel)
            .and_modify(|previous| *previous = (*previous).max(id))
            .or_insert(id);
        if self.search_ui.ack_scheduled {
            return;
        }
        self.search_ui.ack_scheduled = true;
        let entity = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |this, cx| {
                this.search_ui.ack_scheduled = false;
                for (channel, id) in std::mem::take(&mut this.search_ui.pending_ack) {
                    if this.auto_ack_allowed(channel, cx)
                        && this.state.selected_channel == Some(channel)
                        && this
                            .search_ui
                            .histories
                            .get(&channel)
                            .is_some_and(|history| {
                                !history.loading && history.message_ids.contains(&id)
                            })
                        && should_request_ack(
                            this.state
                                .read_states
                                .get(&channel)
                                .and_then(|read| read.last_message_id),
                            this.search_ui.inflight_ack.contains_key(&channel),
                            this.search_ui.failed_ack.contains(&channel),
                            id,
                        )
                    {
                        this.search_ui.inflight_ack.insert(channel, id);
                        this.command(
                            Command::AcknowledgeMessage {
                                channel_id: channel,
                                message_id: id,
                            },
                            cx,
                        );
                    }
                }
            });
        });
    }
    fn preview_card(
        &mut self,
        mut message: Message,
        pinned_at: Option<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (message.channel_id, message.id);
        let text = chat_helpers::display_text(&message);
        if self.search_ui.text.len() > 96 {
            self.search_ui.text.clear();
        }
        let document = self.message_document(&message, message_markup::MessageContext::Standard);
        let input = if let Some(input) = self.search_ui.text.get(&key) {
            input.update(cx, |input, cx| input.update_message_document(document, cx));
            input.clone()
        } else {
            let input = self.message_text_input(document, window, cx);
            self.search_ui.text.insert(key, input.clone());
            input
        };
        message.pinned |= pinned_at.is_some();
        let jump = message.clone();
        let direct = message.clone();
        let menu = message.clone();
        let context = message.clone();
        let pending = self.search_ui.mutations.contains(&key);
        let channel = self
            .state
            .channels
            .get(&message.channel_id)
            .map(|channel| channel.display_name())
            .unwrap_or_else(|| "Conversation".into());
        div()
            .id(("browse-message", message.id.0))
            .group("message-preview")
            .relative()
            .focusable()
            .tab_index(0)
            .w_full()
            .min_w_0()
            .max_w_full()
            .px_3()
            .py_3()
            .mb_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x393343))
            .hover(|style| style.bg(rgb(0x2b2836)).border_color(rgb(0x514660)))
            .focus(|style| style.border_color(rgb(ACCENT)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.jump_message(&direct, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_message_menu(
                        context.clone(),
                        event.position,
                        MessageMenuContext::Preview,
                        window,
                        cx,
                    );
                }),
            )
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pr(px(64.))
                    .child(rich_message::avatar(
                        &message.author,
                        self.media_endpoint.as_deref(),
                        26.,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_ellipsis()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(message.author.display_name().to_owned()),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(MUTED))
                                    .text_ellipsis()
                                    .child(format!(
                                        "#{channel} · {}",
                                        message_time(&message.timestamp)
                                    )),
                            ),
                    ),
            )
            .when(!text.is_empty(), |view| {
                view.child(
                    div()
                        .id(("browse-text", message.id.0))
                        .w_full()
                        .min_w_0()
                        .max_w_full()
                        .max_h(px(140.))
                        .overflow_hidden()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(input),
                )
            })
            .child(
                div()
                    .id(("browse-media", message.id.0))
                    .max_h(px(180.))
                    .overflow_hidden()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(rich_message::media(
                        &message,
                        !self.image_viewer.is_open(),
                        cx,
                    )),
            )
            .when(pending, |view| {
                view.child(muted("Updating pin…").text_size(px(10.)))
            })
            .child(
                div()
                    .absolute()
                    .top(px(8.))
                    .right(px(8.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        icon_button(("browse-jump", message.id.0), Icon::Jump, "Jump to message")
                            .opacity(0.55)
                            .group_hover("message-preview", |style| style.opacity(1.))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.jump_message(&jump, cx);
                            })),
                    )
                    .child(
                        icon_button(("browse-more", message.id.0), Icon::More, "Message actions")
                            .opacity(0.55)
                            .group_hover("message-preview", |style| style.opacity(1.))
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_message_menu(
                                    menu.clone(),
                                    event.position(),
                                    MessageMenuContext::Preview,
                                    window,
                                    cx,
                                );
                            })),
                    ),
            )
            .into_any_element()
    }
    pub(super) fn browser_pane(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let pins = self.search_ui.open == Some(BrowserTab::Pins);
        let width = if window.viewport_size().width < px(1200.) {
            340.
        } else {
            390.
        };
        let mut body = div()
            .w(px(width))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(rgb(0x40374b))
            .bg(rgb(PANEL))
            .child(
                div()
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        quiet_button("browse-search-tab", "Search")
                            .px_2()
                            .py_1()
                            .when(!pins, |view| view.bg(rgb(0x57428b)))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_search(window, cx)),
                            ),
                    )
                    .child(
                        quiet_button("browse-pins-tab", "Pinned")
                            .px_2()
                            .py_1()
                            .when(pins, |view| view.bg(rgb(0x57428b)))
                            .on_click(cx.listener(|this, _, _, cx| this.open_pins(cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        icon_button("close-browser", Icon::Close, "Close search and pins")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_browser(window, cx)),
                            ),
                    ),
            )
            .when_some(self.search_ui.mutation_error.clone(), |view, error| {
                view.child(
                    div()
                        .p_3()
                        .text_color(rgb(0xf0a1b5))
                        .text_size(px(12.))
                        .child(error),
                )
            });
        if pins {
            let channel = self.search_ui.pins_context;
            let snapshot = channel.and_then(|channel| self.search_ui.pins.get(&channel));
            body = body.child(
                div()
                    .px_3()
                    .pb_2()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "Pins · {}",
                        channel.map(|id| self.channel_name(id)).unwrap_or_default()
                    )),
            );
            if let Some(snapshot) = snapshot {
                if snapshot.loading && snapshot.items.is_empty() {
                    body = body.child(browse_state("Loading pinned messages…", None));
                } else if let Some(error) = &snapshot.error {
                    let error = error.clone();
                    body = body
                        .child(browse_state("Couldn't load pins", Some(error)))
                        .when_some(channel, |view, channel| {
                            view.child(quiet_button("retry-pins", "Try again").m_3().on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.command(
                                        Command::LoadPins {
                                            channel_id: channel,
                                            more: false,
                                        },
                                        cx,
                                    )
                                }),
                            ))
                        });
                } else if snapshot.items.is_empty() {
                    body = body.child(browse_state(
                        "No pinned messages",
                        Some("Pinned messages will appear here.".into()),
                    ));
                } else {
                    body = body.child(
                        list(
                            self.search_ui.pins_list.clone(),
                            cx.processor(|this, index: usize, window, cx| {
                                let item = this
                                    .search_ui
                                    .pins_context
                                    .and_then(|channel| this.search_ui.pins.get(&channel))
                                    .and_then(|snapshot| snapshot.items.get(index))
                                    .cloned();
                                item.map(|item| {
                                    this.preview_card(
                                        item.message,
                                        Some(item.pinned_at),
                                        window,
                                        cx,
                                    )
                                })
                                .unwrap_or_else(|| div().into_any_element())
                            }),
                        )
                        .w_full()
                        .min_w_0()
                        .flex_1()
                        .min_h_0(),
                    );
                    if snapshot.has_more
                        && let Some(channel) = channel
                    {
                        let loading = snapshot.loading;
                        body = body.child(
                            quiet_button(
                                "more-pins",
                                if loading {
                                    "Loading…"
                                } else {
                                    "Load more pins"
                                },
                            )
                            .m_3()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if !loading {
                                        this.command(
                                            Command::LoadPins {
                                                channel_id: channel,
                                                more: true,
                                            },
                                            cx,
                                        );
                                    }
                                },
                            )),
                        );
                    }
                }
            } else {
                body = body.child(browse_state("Loading pinned messages…", None));
            }
        } else {
            body = body
                .child(
                    div()
                        .px_3()
                        .pb_2()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(div().flex_1().min_w_0().child(self.search.clone()))
                        .child(
                            icon_button("run-search", Icon::Search, "Search messages")
                                .on_click(cx.listener(|this, _, _, cx| this.run_search(1, cx))),
                        ),
                )
                .child(
                    div()
                        .px_3()
                        .pb_3()
                        .flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(self.search_ui.scope.clone()))
                        .child(div().flex_1().min_w_0().child(self.search_ui.sort.clone())),
                );
            if let Some(error) = &self.search_ui.local_error {
                body = body.child(browse_state("Check your search", Some(error.clone())));
            } else if let Some(snapshot) = &self.search_ui.result {
                match snapshot.status {
                    SearchStatus::Loading => body = body.child(browse_state("Searching…", None)),
                    SearchStatus::Indexing => {
                        body = body.child(browse_state(
                            "Indexing this scope…",
                            Some(
                                "Results will appear automatically when indexing finishes.".into(),
                            ),
                        ))
                    }
                    SearchStatus::Failed => {
                        body = body
                            .child(browse_state("Search failed", snapshot.error.clone()))
                            .child(
                                quiet_button("retry-search", "Try again")
                                    .m_3()
                                    .on_click(cx.listener(|this, _, _, cx| this.run_search(1, cx))),
                            )
                    }
                    SearchStatus::Ready => {
                        body = body.child(
                            div()
                                .px_3()
                                .py_1()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child(format!("{} results", snapshot.total)),
                        );
                        if snapshot.messages.is_empty() {
                            body = body.child(browse_state(
                                "No results",
                                Some("Try another query or scope.".into()),
                            ));
                        } else {
                            body = body.child(
                                list(
                                    self.search_ui.results_list.clone(),
                                    cx.processor(|this, index: usize, window, cx| {
                                        let message = this
                                            .search_ui
                                            .result
                                            .as_ref()
                                            .and_then(|snapshot| snapshot.messages.get(index))
                                            .cloned();
                                        message
                                            .map(|message| {
                                                this.preview_card(message, None, window, cx)
                                            })
                                            .unwrap_or_else(|| div().into_any_element())
                                    }),
                                )
                                .w_full()
                                .min_w_0()
                                .flex_1()
                                .min_h_0(),
                            );
                            let (page, total, hits) =
                                (snapshot.page, snapshot.total, snapshot.hits_per_page.max(1));
                            let more = search_has_next(page, total, hits);
                            body = body.child(
                                div()
                                    .p_3()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        quiet_button("search-previous", "Previous")
                                            .px_2()
                                            .py_1()
                                            .when(page <= 1, |view| view.opacity(0.4))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if page > 1 {
                                                    this.search_page(page - 1, cx);
                                                }
                                            })),
                                    )
                                    .child(muted(format!("Page {}", page)))
                                    .child(
                                        quiet_button("search-next", "Next")
                                            .px_2()
                                            .py_1()
                                            .when(!more, |view| view.opacity(0.4))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if more {
                                                    this.search_page(page + 1, cx);
                                                }
                                            })),
                                    ),
                            );
                        }
                    }
                }
            } else {
                body = body.child(browse_state(
                    "Search messages",
                    Some("Search text or use filters like from:, in: or has:image. Press Enter to search.".into()),
                ));
            }
        }
        body.into_any_element()
    }
    pub(super) fn can_pin_message(&self, message: &Message) -> bool {
        self.state.can_pin_messages(message.channel_id)
    }
    pub(super) fn request_pin(
        &mut self,
        message: Message,
        pinned: bool,
        skip_confirmation: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_pin_message(&message) {
            return;
        }
        if skip_confirmation {
            self.command(
                Command::SetMessagePinned {
                    channel_id: message.channel_id,
                    message_id: message.id,
                    pinned,
                },
                cx,
            );
        } else {
            if self.search_ui.confirm.is_none() {
                self.search_ui.pin_previous_focus = window.focused(cx);
            }
            self.search_ui.pin_keys.clear();
            self.search_ui.confirm = Some(PinConfirmation { message, pinned });
            self.search_ui.pin_cancel_focus.focus(window);
            cx.notify();
        }
    }
    pub(super) fn dismiss_pin_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_ui.confirm.take().is_some() {
            self.restore_pin_focus(window, cx);
            cx.notify();
        }
    }
    fn restore_pin_focus(&mut self, window: &mut Window, cx: &Context<Self>) {
        self.search_ui.pin_keys.clear();
        if let Some(previous) = self.search_ui.pin_previous_focus.take() {
            previous.focus(window);
        } else {
            self.composer.focus_handle(cx).focus(window);
        }
    }
    fn accept_pin_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_pin(cx);
        self.restore_pin_focus(window, cx);
    }
    pub(super) fn confirm_pin(&mut self, cx: &mut Context<Self>) {
        if let Some(confirm) = self.search_ui.confirm.take() {
            self.command(
                Command::SetMessagePinned {
                    channel_id: confirm.message.channel_id,
                    message_id: confirm.message.id,
                    pinned: confirm.pinned,
                },
                cx,
            );
        }
    }
    pub(super) fn pin_confirmation(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(confirm) = &self.search_ui.confirm else {
            return div().into_any_element();
        };
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000099))
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("pin-confirm-dialog")
                    .track_focus(&self.search_ui.pin_dialog_focus)
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => this.dismiss_pin_confirmation(window, cx),
                            "tab" => {
                                this.search_ui.pin_keys.clear();
                                if this.search_ui.pin_cancel_focus.is_focused(window) {
                                    this.search_ui.pin_confirm_focus.focus(window);
                                } else {
                                    this.search_ui.pin_cancel_focus.focus(window);
                                }
                            }
                            "enter" | "space" if !event.keystroke.modifiers.modified() => {
                                let choice = if this.search_ui.pin_confirm_focus.is_focused(window)
                                {
                                    Some(ConfirmationChoice::Confirm)
                                } else if this.search_ui.pin_cancel_focus.is_focused(window) {
                                    Some(ConfirmationChoice::Cancel)
                                } else {
                                    None
                                };
                                if let Some(choice) = choice {
                                    this.search_ui.pin_keys.press(choice, event.is_held);
                                } else {
                                    this.search_ui.pin_keys.clear();
                                }
                            }
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            let choice = this.search_ui.pin_keys.release();
                            if !event.keystroke.modifiers.modified() {
                                match choice {
                                    Some(ConfirmationChoice::Cancel) => {
                                        this.dismiss_pin_confirmation(window, cx)
                                    }
                                    Some(ConfirmationChoice::Confirm) => {
                                        this.accept_pin_confirmation(window, cx)
                                    }
                                    None => {}
                                }
                            }
                            // Opening a menu/dialog can deliver its previous key-up
                            // to the newly focused Cancel button. Consume unarmed
                            // releases before GPUI synthesizes a keyboard click.
                            cx.stop_propagation();
                        }
                    }))
                    .w(px(420.))
                    .p_5()
                    .occlude()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x514861))
                    .bg(rgb(PANEL))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(heading(if confirm.pinned {
                        "Pin message?"
                    } else {
                        "Unpin message?"
                    }))
                    .child(muted(if confirm.pinned {
                        "Pin this message for everyone in this channel?"
                    } else {
                        "Remove this message from the channel's pinned messages?"
                    }))
                    .child(
                        div()
                            .max_h(px(90.))
                            .overflow_hidden()
                            .p_3()
                            .bg(rgb(BG))
                            .rounded_md()
                            .child(format!(
                                "{}: {}",
                                confirm.message.author.display_name(),
                                chat_helpers::preview(&confirm.message)
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                quiet_button("cancel-pin", "Cancel")
                                    .track_focus(&self.search_ui.pin_cancel_focus)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.dismiss_pin_confirmation(window, cx)
                                    })),
                            )
                            .child(
                                quiet_button(
                                    "confirm-pin",
                                    if confirm.pinned {
                                        "Pin message"
                                    } else {
                                        "Unpin message"
                                    },
                                )
                                .track_focus(&self.search_ui.pin_confirm_focus)
                                .bg(rgb(0x7255c8))
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.accept_pin_confirmation(window, cx),
                                )),
                            ),
                    ),
            )
            .into_any_element()
    }
    pub(super) fn maybe_demo_search(
        &mut self,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if self.search_ui.demo_started
            || !self.demo
            || self.state.current_user.is_none()
            || self.state.selected_channel.is_none()
            || !std::env::args().any(|arg| arg == "--demo-search")
        {
            return;
        }
        self.search_ui.demo_started = true;
        self.search_ui.context = self.state.selected_channel;
        self.search_ui.open = Some(BrowserTab::Search);
        self.pane = Pane::Chat;
        self.search
            .update(cx, |input, cx| input.set("release".into(), cx));
        self.command(Command::DemoSearch, cx);
        self.run_search(1, cx);
        if let Some(window) = window {
            self.search.focus_handle(cx).focus(window);
        }
    }
}
fn browse_state(title: &str, detail: Option<String>) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .p_4()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .text_center()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .when_some(detail, |view, detail| view.child(muted(detail)))
        .into_any_element()
}

fn visible_window_messages(
    state: &ClientState,
    histories: &BTreeMap<Snowflake, HistorySnapshot>,
) -> Vec<Message> {
    let Some(channel) = state.selected_channel else {
        return Vec::new();
    };
    if let Some(history) = histories.get(&channel) {
        return history
            .message_ids
            .iter()
            .filter_map(|id| {
                state
                    .messages
                    .get(&channel)
                    .and_then(|messages| messages.get(id))
            })
            .cloned()
            .collect();
    }
    let mut messages = state
        .channel_messages(channel)
        .rev()
        .take(120)
        .cloned()
        .collect::<Vec<_>>();
    messages.reverse();
    messages
}

fn should_request_ack(read: Option<Snowflake>, pending: bool, failed: bool, id: Snowflake) -> bool {
    !pending && !failed && read.is_none_or(|read| read < id)
}

fn search_has_next(page: u32, total: u64, hits_per_page: u32) -> bool {
    u64::from(page.max(1)) * u64::from(hits_per_page.max(1)) < total
}

fn auto_ack_policy(
    active: bool,
    at_bottom: bool,
    latest: bool,
    manual: bool,
    blocking_modal: bool,
) -> bool {
    active && at_bottom && latest && !manual && !blocking_modal
}

fn consume_latest_ready(requests: &mut BTreeSet<Snowflake>, history: &HistorySnapshot) -> bool {
    !history.loading
        && history.latest
        && history.target.is_none()
        && history.error.is_none()
        && requests.remove(&history.channel_id)
}

#[cfg(test)]
mod tests {
    use super::{
        auto_ack_policy, consume_latest_ready, search_has_next, should_request_ack,
        visible_window_messages,
    };
    use fluxer_client::HistorySnapshot;
    use fluxer_core::{ClientState, Message, Snowflake, User};
    use std::collections::{BTreeMap, BTreeSet};
    #[test]
    fn return_latest_scroll_waits_for_ready_window_and_is_one_shot() {
        let mut requests = BTreeSet::from([Snowflake(1)]);
        let mut history = HistorySnapshot {
            channel_id: Snowflake(1),
            message_ids: Vec::new(),
            target: None,
            loading: true,
            has_older: true,
            has_newer: false,
            latest: true,
            error: None,
        };
        assert!(!consume_latest_ready(&mut requests, &history));
        assert!(requests.contains(&Snowflake(1)));
        history.loading = false;
        history.message_ids = vec![Snowflake(1119)];
        assert!(consume_latest_ready(&mut requests, &history));
        assert!(!consume_latest_ready(&mut requests, &history));
    }
    #[test]
    fn auto_ack_requires_focused_latest_tail_and_no_manual_or_modal_block() {
        assert!(auto_ack_policy(true, true, true, false, false));
        assert!(!auto_ack_policy(false, true, true, false, false));
        assert!(!auto_ack_policy(true, false, true, false, false));
        assert!(!auto_ack_policy(true, true, false, false, false));
        assert!(!auto_ack_policy(true, true, true, true, false));
        assert!(!auto_ack_policy(true, true, true, false, true));
    }
    #[test]
    fn search_pagination_uses_one_based_server_pages() {
        assert!(search_has_next(1, 75, 25));
        assert!(search_has_next(2, 75, 25));
        assert!(!search_has_next(3, 75, 25));
        assert!(!search_has_next(1, 25, 25));
        assert!(!search_has_next(1, 0, 25));
    }
    #[test]
    fn ack_suppression_uses_authoritative_read_cursor_and_context_fences() {
        assert!(should_request_ack(None, false, false, Snowflake(10)));
        assert!(!should_request_ack(None, true, false, Snowflake(10)));
        assert!(!should_request_ack(None, false, true, Snowflake(10)));
        assert!(!should_request_ack(None, false, true, Snowflake(20)));
        assert!(should_request_ack(None, false, false, Snowflake(10))); // aborted context can retry
        assert!(!should_request_ack(
            Some(Snowflake(10)),
            false,
            false,
            Snowflake(10)
        ));
        assert!(should_request_ack(
            Some(Snowflake(10)),
            false,
            false,
            Snowflake(11)
        ));
    }
    #[test]
    fn history_projection_never_mixes_disjoint_cached_messages() {
        let mut state = ClientState::default();
        state.selected_channel = Some(Snowflake(1));
        for id in 1..=300 {
            state.insert_message(Message::text(
                Snowflake(id),
                Snowflake(1),
                User::default(),
                format!("message{id}"),
            ));
        }
        let history = HistorySnapshot {
            channel_id: Snowflake(1),
            message_ids: vec![Snowflake(20), Snowflake(21), Snowflake(22)],
            target: Some(Snowflake(21)),
            loading: false,
            has_older: true,
            has_newer: true,
            latest: false,
            error: None,
        };
        let messages = visible_window_messages(&state, &BTreeMap::from([(Snowflake(1), history)]));
        assert_eq!(
            messages
                .iter()
                .map(|message| message.id.0)
                .collect::<Vec<_>>(),
            vec![20, 21, 22]
        );
        let fallback = visible_window_messages(&state, &BTreeMap::new());
        assert_eq!(fallback.len(), 120);
        assert_eq!(fallback.first().unwrap().id, Snowflake(181));
        assert_eq!(fallback.last().unwrap().id, Snowflake(300));
    }
}
