// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source InboxPopout: gateway unread channels and a separately paginated REST mention feed.
use super::*;
use crate::controls::{Icon, icon, icon_button, quiet_button};
use fluxer_client::MentionsSnapshot;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Unread,
    Mentions,
}
#[derive(Clone)]
enum Entry {
    Channel(Snowflake),
    Mention(Box<Message>),
}
pub(super) struct InboxUi {
    pub open: bool,
    filters_initialized: bool,
    tab: Tab,
    pub snapshot: MentionsSnapshot,
    focus: FocusHandle,
    filter_focus: FocusHandle,
    button_bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    previous: Option<FocusHandle>,
    selected: usize,
    armed: Option<(Snowflake, Option<Snowflake>)>,
}
impl InboxUi {
    pub fn reset(&mut self) {
        self.open = false;
        self.filters_initialized = false;
        self.snapshot = MentionsSnapshot::default();
        self.previous = None;
        self.armed = None;
    }
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            open: false,
            filters_initialized: false,
            tab: Tab::Unread,
            snapshot: MentionsSnapshot::default(),
            focus: cx.focus_handle(),
            filter_focus: cx.focus_handle(),
            button_bounds: std::rc::Rc::new(std::cell::Cell::new(Bounds::default())),
            previous: None,
            selected: 0,
            armed: None,
        }
    }
}
fn identity(entry: &Entry) -> (Snowflake, Option<Snowflake>) {
    match entry {
        Entry::Channel(id) => (*id, None),
        Entry::Mention(message) => (message.channel_id, Some(message.id)),
    }
}
fn filter_items(
    filters: fluxer_client::MentionFilters,
) -> Vec<crate::context_menu::ContextMenuItem> {
    [
        ("everyone", "Include @everyone mentions", filters.everyone),
        ("roles", "Include @role mentions", filters.roles),
        ("guilds", "Include all community mentions", filters.guilds),
    ]
    .into_iter()
    .map(
        |(id, label, checked)| crate::context_menu::ContextMenuItem::Toggle {
            id: id.into(),
            label: label.into(),
            checked,
            enabled: true,
        },
    )
    .collect()
}
impl Fluxer {
    pub(super) fn inbox_filters_available(&self) -> bool {
        self.inbox_ui.open && self.inbox_ui.tab == Tab::Mentions
    }
    pub(super) fn inbox_filter_items(&self) -> Vec<crate::context_menu::ContextMenuItem> {
        filter_items(self.inbox_ui.snapshot.filters)
    }
    pub(super) fn update_inbox_filter(&mut self, id: &str, value: bool, cx: &mut Context<Self>) {
        match id {
            "everyone" => self.inbox_ui.snapshot.filters.everyone = value,
            "roles" => self.inbox_ui.snapshot.filters.roles = value,
            "guilds" => self.inbox_ui.snapshot.filters.guilds = value,
            _ => return,
        }
        self.inbox_ui.snapshot.loading = false;
        self.load_inbox_mentions(false, cx);
    }
    pub(super) fn inbox_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let bounds = self.inbox_ui.button_bounds.clone();
        icon_button("open-inbox", Icon::Inbox, "Inbox")
            .relative()
            .child(
                canvas(move |rect, _, _| bounds.set(rect), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .on_click(cx.listener(|this, _, window, cx| this.toggle_inbox(window, cx)))
    }
    pub(super) fn toggle_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inbox_ui.open {
            self.close_inbox(window, cx);
            return;
        }
        if self.context_menu_open(cx)
            || self.quick_switcher.open
            || self.image_viewer.is_open()
            || self.playback.modal.is_open()
        {
            return;
        }
        if !self.inbox_ui.filters_initialized {
            let (everyone, roles, guilds) =
                fluxer_core::recent_mention_filters(&self.state.user_settings);
            self.inbox_ui.snapshot.filters = fluxer_client::MentionFilters {
                everyone,
                roles,
                guilds,
            };
            self.inbox_ui.filters_initialized = true;
        }
        self.inbox_ui.previous = window.focused(cx);
        self.inbox_ui.open = true;
        self.inbox_ui.selected = 0;
        self.inbox_ui.armed = None;
        self.inbox_ui.focus.focus(window);
        if self.inbox_ui.tab == Tab::Mentions && !(self.demo && self.inbox_ui.snapshot.fetched) {
            self.load_inbox_mentions(false, cx);
        }
        cx.notify();
    }
    pub(super) fn close_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inbox_ui.open = false;
        self.inbox_ui.snapshot.loading = false;
        self.inbox_ui.armed = None;
        self.command(Command::CloseMentions, cx);
        if let Some(previous) = self.inbox_ui.previous.take() {
            previous.focus(window);
        }
        cx.notify();
    }
    fn inbox_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.inbox_ui.tab = tab;
        self.inbox_ui.selected = 0;
        self.inbox_ui.armed = None;
        if tab == Tab::Mentions {
            self.load_inbox_mentions(false, cx);
        } else {
            self.inbox_ui.snapshot.loading = false;
            self.command(Command::CloseMentions, cx);
        }
        cx.notify();
    }
    fn load_inbox_mentions(&mut self, more: bool, cx: &mut Context<Self>) {
        if self.inbox_ui.snapshot.loading {
            return;
        }
        let before = more
            .then(|| {
                self.inbox_ui
                    .snapshot
                    .messages
                    .last()
                    .map(|message| message.id)
            })
            .flatten();
        self.inbox_ui.snapshot.loading = true;
        self.inbox_ui.snapshot.error = None;
        self.command(
            Command::FetchMentions {
                before,
                filters: self.inbox_ui.snapshot.filters,
            },
            cx,
        );
        cx.notify();
    }
    fn inbox_entries(&self) -> Vec<Entry> {
        if self.inbox_ui.tab == Tab::Unread {
            let now = chrono::Utc::now().timestamp_millis();
            let mut entries: Vec<_> = self
                .state
                .channels
                .values()
                .filter(|channel| channel.kind.is_message_channel())
                .filter(|channel| {
                    self.state.unread_inbox_visible(channel.id, now)
                        && self.state.current_user.as_ref().is_some_and(|user| {
                            self.state
                                .permissions_for(channel.id, user.id)
                                .contains(fluxer_core::Permissions::VIEW_CHANNEL)
                        })
                })
                .map(|channel| Entry::Channel(channel.id))
                .collect();
            entries.sort_by_key(|entry| {
                let id = identity(entry).0;
                let channel = &self.state.channels[&id];
                (channel.guild_id, channel.position, id)
            });
            entries
        } else {
            self.inbox_ui
                .snapshot
                .messages
                .iter()
                .filter(|message| {
                    !self
                        .state
                        .relationships
                        .get(&message.author.id)
                        .is_some_and(|r| r.is_blocked())
                })
                .filter(|message| {
                    self.state
                        .channels
                        .get(&message.channel_id)
                        .is_some_and(|channel| {
                            self.state.notification_content_allowed(channel)
                                && self.state.current_user.as_ref().is_some_and(|user| {
                                    self.state
                                        .permissions_for(channel.id, user.id)
                                        .contains(fluxer_core::Permissions::VIEW_CHANNEL)
                                })
                        })
                })
                .cloned()
                .map(|message| Entry::Mention(Box::new(message)))
                .collect()
        }
    }
    fn activate_inbox_entry(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        self.close_inbox(window, cx);
        match entry {
            Entry::Channel(id) => self.select(id, cx),
            Entry::Mention(message) => self.jump_message(&message, cx),
        }
    }
    pub(super) fn apply_mentions_snapshot(
        &mut self,
        snapshot: MentionsSnapshot,
        cx: &mut Context<Self>,
    ) {
        if self.inbox_ui.open
            && snapshot.account_id == self.state.current_user.as_ref().map(|user| user.id)
        {
            self.inbox_ui.snapshot = snapshot;
            cx.notify();
        }
    }
    pub(super) fn schedule_inbox_demo(&self, window: &Window, cx: &mut Context<Self>) {
        if !self.demo || !std::env::args().any(|arg| arg == "--demo") {
            return;
        }
        let Ok(mode) = std::env::var("FLUXER_INBOX_DEMO") else {
            return;
        };
        if !matches!(mode.as_str(), "unread" | "mentions" | "error") {
            return;
        }
        let handle = window.window_handle();
        cx.spawn(async move |entity, cx| {
            Timer::after(Duration::from_millis(800)).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = entity.update(cx, |view, cx| {
                    if mode != "unread" {
                        view.inbox_ui.tab = Tab::Mentions;
                    }
                    if mode == "mentions" {
                        let mut messages: Vec<_> = view
                            .state
                            .messages
                            .values()
                            .flat_map(|messages| messages.values())
                            .map(|message| message.as_ref().clone())
                            .collect();
                        for message in &mut messages {
                            message.mentions =
                                view.state.current_user.clone().into_iter().collect();
                        }
                        view.inbox_ui.snapshot = MentionsSnapshot {
                            account_id: view.state.current_user.as_ref().map(|user| user.id),
                            messages,
                            fetched: true,
                            ..Default::default()
                        };
                    }
                    if mode == "error" {
                        view.inbox_ui.snapshot.fetched = true;
                    }
                    view.toggle_inbox(window, cx);
                    if mode == "error" {
                        view.inbox_ui.snapshot.loading = false;
                        view.inbox_ui.snapshot.error = Some(
                            "Offline fixture · the mention service could not be reached".into(),
                        );
                    }
                });
            });
        })
        .detach();
    }
    pub(super) fn inbox_overlay(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.inbox_ui.open {
            return div().into_any_element();
        }
        let entries = self.inbox_entries();
        let count = entries.len();
        self.inbox_ui.selected = self.inbox_ui.selected.min(count.saturating_sub(1));
        let row_height = if self.inbox_ui.tab == Tab::Mentions {
            94.
        } else {
            60.
        };
        let visible = ((f32::from(window.viewport_size().height) - 225.) / row_height)
            .floor()
            .clamp(1., 7.) as usize;
        let first = self.inbox_ui.selected.saturating_sub(visible - 1);
        let width = (f32::from(window.viewport_size().width) - 24.).clamp(260., 440.);
        let entries_for_keys = entries.clone();
        let bounds = self.inbox_ui.button_bounds.get();
        let anchor = if bounds.size.width > px(0.) {
            point(bounds.right(), bounds.bottom() + px(8.))
        } else {
            point(window.viewport_size().width - px(12.), px(68.))
        };
        deferred(anchored().position_mode(AnchoredPositionMode::Window).position(anchor).anchor(Corner::TopRight).snap_to_window_with_margin(px(8.)).child(
            div().id("inbox-popout").track_focus(&self.inbox_ui.focus).occlude().w(px(width)).rounded_lg().bg(rgb(PANEL)).border_1().border_color(rgb(0x4c435e)).shadow_lg()
                .on_mouse_down_out(cx.listener(|this,event:&MouseDownEvent,window,cx|{if !this.context_menu_open(cx) && !this.inbox_ui.button_bounds.get().contains(&event.position){this.close_inbox(window,cx);}}))
                .capture_key_down(cx.listener(move|this,event:&KeyDownEvent,window,cx|{if this.context_menu_open(cx) { return; }
                if this.inbox_ui.filter_focus.is_focused(window) && matches!(event.keystroke.key.as_str(), "enter" | "space") { if !event.is_held { this.open_context_menu(crate::context_actions::Target::InboxFilters, point(anchor.x - px(240.), anchor.y + px(40.)), window, cx); } cx.stop_propagation(); return; } match event.keystroke.key.as_str(){
                    "escape"=>this.close_inbox(window,cx),
                    "up"|"down"=>{if !entries_for_keys.is_empty(){this.inbox_ui.selected=if event.keystroke.key=="up"{this.inbox_ui.selected.saturating_sub(1)}else{(this.inbox_ui.selected+1).min(entries_for_keys.len()-1)};this.inbox_ui.armed=None;cx.notify();}},
                    "left"=>this.inbox_tab(Tab::Unread,cx),"right"=>this.inbox_tab(Tab::Mentions,cx),
                    "enter"=>{if !event.is_held{this.inbox_ui.armed=entries_for_keys.get(this.inbox_ui.selected).map(identity);}},_=>return,
                }cx.stop_propagation();}))
                .capture_key_up(cx.listener(|this,event:&KeyUpEvent,window,cx|{if this.context_menu_open(cx) { return; }
                if event.keystroke.key=="enter"{if let Some(armed)=this.inbox_ui.armed.take() && let Some(entry)=this.inbox_entries().get(this.inbox_ui.selected).filter(|entry|identity(entry)==armed).cloned(){this.activate_inbox_entry(entry,window,cx);}cx.stop_propagation();}}))
                .child(div().px_4().py_3().flex().items_center().gap_2()
                    .child(icon(Icon::Inbox,20.))
                    .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child("Inbox"))
                    .when(self.inbox_ui.tab == Tab::Mentions, |view| view
                        .child(icon_button("refresh-inbox",Icon::Refresh,"Refresh recent mentions")
                            .on_click(cx.listener(|this,_,_,cx|this.load_inbox_mentions(false,cx))))
                        .child(icon_button("inbox-filters",Icon::More,"Filter mentions").track_focus(&self.inbox_ui.filter_focus)
                            .on_click(cx.listener(|this,event: &ClickEvent,window,cx|this.open_context_menu(crate::context_actions::Target::InboxFilters,event.position(),window,cx)))))
                    .child(icon_button("close-inbox",Icon::Close,"Close inbox")
                        .on_click(cx.listener(|this,_,window,cx|this.close_inbox(window,cx)))))
                .child(div().px_3().flex().gap_2().border_b_1().border_color(rgb(0x40384d)).children([(Tab::Unread,"Unreads"),(Tab::Mentions,"Mentions")].into_iter().map(|(tab,label)|quiet_button(label,label).when(self.inbox_ui.tab==tab,|v|v.bg(rgb(0x514065)).text_color(rgb(TEXT))).on_click(cx.listener(move|this,_,_,cx|this.inbox_tab(tab,cx))))))
                .child(div().id("inbox-entries").py_2().on_scroll_wheel(cx.listener(move|this,event:&ScrollWheelEvent,_,cx|{let delta=event.delta.pixel_delta(px(row_height)).y;let count=this.inbox_entries().len();if count>0&&delta!=px(0.){this.inbox_ui.selected=if delta<px(0.){(this.inbox_ui.selected+1).min(count-1)}else{this.inbox_ui.selected.saturating_sub(1)};cx.notify();}cx.stop_propagation();})).children(entries.into_iter().enumerate().skip(first).take(visible).map(|(index,entry)|{
                    let channel_id=identity(&entry).0;let channel=&self.state.channels[&channel_id];let guild=channel.guild_id.and_then(|id|self.state.guilds.get(&id)).map(|guild|guild.name.as_str()).unwrap_or("Direct messages");
                    let base=div().id(("inbox-entry",index)).mx_2().px_3().py_2().min_h(px(row_height)).rounded_md().cursor_pointer().when(index==self.inbox_ui.selected,|v|v.bg(rgb(0x40344f))).hover(|v|v.bg(rgb(0x3b3349)));
                    let content=match &entry {
                        Entry::Channel(id)=>{let mentions=self.state.read_states.get(id).map_or(0,|read|read.mention_count);base.child(div().flex().items_center().gap_2().child(icon(if channel.kind.is_voice(){Icon::Volume}else{Icon::Chat},22.)).child(div().flex_1().min_w_0().child(div().font_weight(FontWeight::SEMIBOLD).text_ellipsis().child(self.display_channel_name(channel))).child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(guild.to_owned()))).when(mentions>0,|v|v.child(div().rounded_full().bg(rgb(0xb34768)).px_2().text_size(px(11.)).child(mentions.to_string()))).child(icon_button(("inbox-mark-read",id.0),Icon::MarkRead,"Mark channel read").on_click(cx.listener(move|this,_,_,cx|{this.command(Command::MarkChannelRead{channel_id},cx);cx.stop_propagation();}))))},
                        Entry::Mention(message)=>{let document=self.streamer_document(message,crate::message_markup::MessageContext::InlinePreview);let text=document.copy_range(0..document.text.len(),&BTreeSet::new());let text=if text.trim().is_empty(){message.attachments.first().map(|file|format!("Attachment: {}",file.filename)).unwrap_or_else(||"Message".into())}else{text};base.child(div().text_size(px(10.)).text_color(rgb(MUTED)).text_ellipsis().child(format!("{} · {}",self.display_channel_name(channel),guild))).child(div().mt_1().flex().gap_2().child(crate::avatar_images::user_avatar(&message.author,self.media_endpoint.as_deref(),26.)).child(div().flex_1().min_w_0().child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_ellipsis().child(self.streaming().name(message.author.display_name()).into_owned())).child(div().text_size(px(12.)).line_height(px(17.)).max_h(px(34.)).overflow_hidden().child(text))))},
                    };content.on_click(cx.listener(move|this,_,window,cx|this.activate_inbox_entry(entry.clone(),window,cx)))
                })))
                .when(count==0,|view|view.child(div().p_5().text_color(rgb(MUTED)).child(if self.inbox_ui.tab==Tab::Unread{"You're all caught up"}else if self.inbox_ui.snapshot.loading{"Loading recent mentions…"}else if self.demo{"Offline mention fixture · no cached mentions"}else{"No accessible mentions in this feed"})))
                .when_some(self.inbox_ui.snapshot.error.clone().filter(|_|self.inbox_ui.tab==Tab::Mentions),|view,error|view.child(div().px_4().py_3().text_color(rgb(0xf0a1b5)).text_size(px(12.)).child(error).child(quiet_button("retry-mentions","Retry").on_click(cx.listener(|this,_,_,cx|this.load_inbox_mentions(false,cx))))))
                .when(self.inbox_ui.tab==Tab::Mentions&&self.inbox_ui.snapshot.has_more,|view|view.child(div().p_3().child(quiet_button("more-mentions",if self.inbox_ui.snapshot.loading{"Loading…"}else{"Load older mentions"}).when(self.inbox_ui.snapshot.loading,|view|view.opacity(0.5)).on_click(cx.listener(|this,_,_,cx|this.load_inbox_mentions(true,cx))))))
                .child(div().px_4().py_2().border_t_1().border_color(rgb(0x40384d)).text_size(px(10.)).text_color(rgb(MUTED)).child(if self.demo{"Offline fixture · no account requests"}else if self.inbox_ui.tab==Tab::Mentions{"Recent mentions from your account · opening does not mark them read"}else{"Unread channels from your current read state"}))
        )).with_priority(6).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::filter_items;
    use crate::context_menu::ContextMenuItem;
    use fluxer_client::MentionFilters;

    #[test]
    fn mention_menu_preserves_each_independent_filter() {
        for mask in 0..8 {
            let filters = MentionFilters {
                everyone: mask & 1 != 0,
                roles: mask & 2 != 0,
                guilds: mask & 4 != 0,
            };
            let states = filter_items(filters)
                .into_iter()
                .map(|item| match item {
                    ContextMenuItem::Toggle {
                        id,
                        checked,
                        enabled,
                        ..
                    } => {
                        assert!(enabled);
                        (id, checked)
                    }
                    _ => panic!("mention filters must remain persistent checkbox rows"),
                })
                .collect::<Vec<_>>();
            assert_eq!(
                states,
                vec![
                    ("everyone".into(), filters.everyone),
                    ("roles".into(), filters.roles),
                    ("guilds".into(), filters.guilds)
                ]
            );
        }
    }
}
