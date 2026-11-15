// SPDX-License-Identifier: AGPL-3.0-or-later
//! Compact QuickSwitcherModal with source # / ! / @ / * query modes.
use super::*;
use crate::controls::{Icon, icon, icon_button};
use std::collections::VecDeque;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Channel(Snowflake),
    User(Snowflake),
    Guild(Snowflake),
}
#[derive(Clone)]
struct ResultRow {
    target: Target,
    title: String,
    subtitle: String,
    kind: RowKind,
    score: u32,
    mentions: u32,
    unread: bool,
}
#[derive(Clone)]
enum RowKind {
    Text,
    Voice,
    Guild,
    Group,
    User(fluxer_core::User),
}
pub(super) struct QuickSwitcher {
    pub input: Entity<TextInput>,
    pub open: bool,
    previous: Option<FocusHandle>,
    results: Vec<ResultRow>,
    selected: usize,
    history: VecDeque<Snowflake>,
    armed: Option<Target>,
    query: String,
}
impl QuickSwitcher {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            input: cx.new(|cx| TextInput::new("Where would you like to go?", cx)),
            open: false,
            previous: None,
            results: vec![],
            selected: 0,
            history: VecDeque::new(),
            armed: None,
            query: String::new(),
        }
    }
    pub fn visit(&mut self, id: Snowflake) {
        self.history.retain(|old| *old != id);
        self.history.push_front(id);
        self.history.truncate(8);
    }
}
fn query_mode(query: &str) -> (Option<char>, String) {
    let query = query.trim();
    let mode = query.chars().next().filter(|ch| "#!@*".contains(*ch));
    (
        mode,
        if mode.is_some() {
            query[1..].trim()
        } else {
            query
        }
        .to_lowercase(),
    )
}
fn score(text: &str, query: &str) -> Option<u32> {
    let text = text.to_lowercase();
    let query = query.to_lowercase();
    if query.is_empty() {
        return Some(100);
    }
    if text == query {
        return Some(0);
    }
    if text.starts_with(&query) {
        return Some(10 + text.chars().count() as u32);
    }
    if let Some(index) = text.find(&query) {
        return Some(100 + index as u32);
    }
    let mut search = query.chars();
    let mut next = search.next();
    let mut gaps = 0;
    for ch in text.chars() {
        if Some(ch) == next {
            next = search.next();
            if next.is_none() {
                return Some(300 + gaps);
            }
        } else {
            gaps += 1;
        }
    }
    None
}
impl Fluxer {
    pub(super) fn schedule_quick_switcher_demo(&self, window: &Window, cx: &mut Context<Self>) {
        if !self.demo || !std::env::args().any(|arg| arg == "--demo") {
            return;
        }
        let Ok(query) = std::env::var("FLUXER_QUICK_SWITCHER_DEMO") else {
            return;
        };
        let handle = window.window_handle();
        cx.spawn(async move |entity, cx| {
            Timer::after(Duration::from_millis(800)).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = entity.update(cx, |view, cx| {
                    view.open_quick_switcher(window, cx);
                    view.quick_switcher
                        .input
                        .update(cx, |input, cx| input.set(query, cx));
                });
            });
        })
        .detach();
    }
    pub(super) fn open_quick_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick_switcher.open {
            self.close_quick_switcher(window, cx);
            return;
        }
        if self.status_ui.is_open()
            || self.context_menu_open(cx)
            || self.image_viewer.is_open()
            || self.playback.modal.is_open()
            || self.message_actions.is_blocking()
            || self.search_ui.confirm.is_some()
        {
            return;
        }
        if self.inbox_ui.open {
            self.close_inbox(window, cx);
        }
        self.quick_switcher.previous = window.focused(cx);
        self.quick_switcher.open = true;
        self.quick_switcher.armed = None;
        self.quick_switcher.query = String::new();
        self.quick_switcher.selected = 0;
        self.quick_switcher
            .input
            .update(cx, |input, cx| input.set(String::new(), cx));
        self.quick_switcher.input.focus_handle(cx).focus(window);
        self.rebuild_quick_results(cx);
        cx.notify();
    }
    fn close_quick_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick_switcher.open = false;
        self.quick_switcher.armed = None;
        if let Some(previous) = self.quick_switcher.previous.take() {
            previous.focus(window);
        } else {
            self.composer.focus_handle(cx).focus(window);
        }
        cx.notify();
    }
    fn rebuild_quick_results(&mut self, cx: &mut Context<Self>) {
        let query = self.quick_switcher.input.read(cx).content.to_string();
        let (mode, term) = query_mode(&query);
        let previous = self
            .quick_switcher
            .results
            .get(self.quick_switcher.selected)
            .map(|row| row.target);
        let mut results = Vec::new();
        for channel in self.state.channels.values() {
            if !channel.kind.is_message_channel() {
                continue;
            }
            if channel.guild_id.is_some()
                && !self.state.current_user.as_ref().is_some_and(|user| {
                    self.state
                        .permissions_for(channel.id, user.id)
                        .contains(fluxer_core::Permissions::VIEW_CHANNEL)
                })
            {
                continue;
            }
            let voice = channel.kind.is_voice();
            let private = channel.kind.is_private();
            if matches!(mode, Some('@' | '*'))
                || (mode == Some('#') && (voice || private))
                || (mode == Some('!') && (!voice || private))
            {
                continue;
            }
            let title = channel.display_name();
            let guild = channel.guild_id.and_then(|id| self.state.guilds.get(&id));
            let subtitle = guild.map(|g| g.name.clone()).unwrap_or_else(|| {
                if channel.kind == ChannelType::GroupDm {
                    "Group direct message".into()
                } else {
                    "Direct message".into()
                }
            });
            let Some(mut rank) = score(&title, &term)
                .or_else(|| score(&format!("{} {title}", subtitle), &term).map(|s| s + 50))
            else {
                continue;
            };
            let read = self.state.read_states.get(&channel.id);
            let mentions = read.map_or(0, |read| read.mention_count);
            let unread = self.state.is_unread(channel.id);
            if term.is_empty() {
                rank = self
                    .quick_switcher
                    .history
                    .iter()
                    .position(|id| *id == channel.id)
                    .map_or(
                        if mentions > 0 {
                            20
                        } else if unread {
                            30
                        } else {
                            100
                        },
                        |position| position as u32,
                    );
            }
            let kind = if private && channel.recipients.len() == 1 {
                RowKind::User(channel.recipients[0].clone())
            } else if private {
                RowKind::Group
            } else if voice {
                RowKind::Voice
            } else {
                RowKind::Text
            };
            results.push(ResultRow {
                target: Target::Channel(channel.id),
                title,
                subtitle,
                kind,
                score: rank,
                mentions,
                unread,
            });
        }
        if !term.is_empty() && !matches!(mode, Some('#' | '!')) {
            if mode != Some('@') {
                for guild in self.state.guilds.values() {
                    if let Some(rank) = score(&guild.name, &term) {
                        results.push(ResultRow {
                            target: Target::Guild(guild.id),
                            title: guild.name.clone(),
                            subtitle: "Community".into(),
                            kind: RowKind::Guild,
                            score: rank + 20,
                            mentions: 0,
                            unread: false,
                        });
                    }
                }
            }
            if mode != Some('*') {
                for user in self.state.users.values() {
                    if self
                        .state
                        .current_user
                        .as_ref()
                        .is_some_and(|me| me.id == user.id)
                        || self
                            .state
                            .relationships
                            .get(&user.id)
                            .is_some_and(|r| r.is_blocked())
                    {
                        continue;
                    }
                    let known = self
                        .selected_guild
                        .is_some_and(|guild| self.state.members.contains_key(&(guild, user.id)))
                        || self
                            .state
                            .relationships
                            .get(&user.id)
                            .is_some_and(|r| r.is_friend());
                    if !known {
                        continue;
                    }
                    // An existing DM already supplies its user result in all-mode.
                    if mode.is_none()
                        && self.state.channels.values().any(|channel| {
                            channel.kind.is_private()
                                && channel.recipients.len() == 1
                                && channel.recipients[0].id == user.id
                        })
                    {
                        continue;
                    }
                    if let Some(rank) =
                        score(user.display_name(), &term).or_else(|| score(&user.username, &term))
                    {
                        results.push(ResultRow {
                            target: Target::User(user.id),
                            title: user.display_name().into(),
                            subtitle: format!("@{}", user.username),
                            kind: RowKind::User(user.clone()),
                            score: rank + 10,
                            mentions: 0,
                            unread: false,
                        });
                    }
                }
            }
        }
        results.sort_by_key(|row| (row.score, row.title.to_lowercase()));
        results.truncate(100);
        self.quick_switcher.selected = if query == self.quick_switcher.query {
            previous
                .and_then(|target| results.iter().position(|row| row.target == target))
                .unwrap_or(0)
        } else {
            0
        };
        self.quick_switcher.query = query;
        self.quick_switcher.results = results;
    }
    fn activate_quick_result(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_quick_switcher(window, cx);
        match target {
            Target::Channel(id) => self.select(id, cx),
            Target::User(user_id) => self.command(Command::OpenDm { user_id }, cx),
            Target::Guild(guild) => {
                let channel = self
                    .state
                    .channels
                    .values()
                    .filter(|channel| {
                        channel.guild_id == Some(guild) && channel.kind.is_message_channel()
                    })
                    .filter(|channel| {
                        self.state.current_user.as_ref().is_some_and(|user| {
                            self.state
                                .permissions_for(channel.id, user.id)
                                .contains(fluxer_core::Permissions::VIEW_CHANNEL)
                        })
                    })
                    .min_by_key(|channel| (channel.kind.is_voice(), channel.position, channel.id))
                    .map(|channel| channel.id);
                if let Some(channel) = channel {
                    self.select(channel, cx);
                }
            }
        }
    }
    pub(super) fn quick_switcher_overlay(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.quick_switcher.open {
            return div().into_any_element();
        }
        self.rebuild_quick_results(cx);
        let count = self.quick_switcher.results.len();
        let visible = ((f32::from(window.viewport_size().height) - 220.) / 48.)
            .floor()
            .clamp(2., 8.) as usize;
        let first = self.quick_switcher.selected.saturating_sub(visible - 1);
        let caption = match query_mode(&self.quick_switcher.query).0 {
            Some('#') => "TEXT CHANNELS",
            Some('!') => "VOICE CHANNELS",
            Some('@') => "FRIENDS AND COMMUNITY MEMBERS",
            Some('*') => "COMMUNITIES",
            _ => {
                if self.quick_switcher.query.is_empty() {
                    "RECENT AND UNREAD CHANNELS"
                } else {
                    "CHANNELS, PEOPLE AND COMMUNITIES"
                }
            }
        };
        div().absolute().inset_0().occlude().bg(rgba(0x00000099)).flex().items_start().justify_center().pt(px((f32::from(window.viewport_size().height)*0.17).max(24.)))
            .on_mouse_down(MouseButton::Left,cx.listener(|this,_,window,cx|this.close_quick_switcher(window,cx)))
            .child(div().id("quick-switcher-dialog").w(px(560.)).max_w(relative(0.94)).rounded_lg().bg(rgb(PANEL)).border_1().border_color(rgb(0x4c435e)).shadow_lg()
                .on_mouse_down(MouseButton::Left,|_,_,cx|cx.stop_propagation())
                .capture_key_down(cx.listener(|this,event:&KeyDownEvent,window,cx| {
                    match event.keystroke.key.as_str(){
                        "escape"=>this.close_quick_switcher(window,cx),
                        "up"|"down"=>{let count=this.quick_switcher.results.len();if count>0 {this.quick_switcher.selected=if event.keystroke.key=="up"{this.quick_switcher.selected.checked_sub(1).unwrap_or(count-1)}else{(this.quick_switcher.selected+1)%count};this.quick_switcher.armed=None;cx.notify();}},
                        "enter"=>{if !event.is_held && !this.quick_switcher.input.read(cx).is_composing(){this.quick_switcher.armed=this.quick_switcher.results.get(this.quick_switcher.selected).map(|row|row.target);}},
                        "tab"=>{this.quick_switcher.input.focus_handle(cx).focus(window);},
                        _=>return,
                    }cx.stop_propagation();
                }))
                .capture_key_up(cx.listener(|this,event:&KeyUpEvent,window,cx|{if event.keystroke.key=="enter"{if let Some(target)=this.quick_switcher.armed.take(){this.activate_quick_result(target,window,cx);}cx.stop_propagation();}}))
                .on_scroll_wheel(cx.listener(|this,event:&ScrollWheelEvent,_,cx|{let count=this.quick_switcher.results.len();let delta=event.delta.pixel_delta(px(48.)).y;if count>0 && delta!=px(0.){this.quick_switcher.selected=if delta<px(0.){(this.quick_switcher.selected+1).min(count-1)}else{this.quick_switcher.selected.saturating_sub(1)};this.quick_switcher.armed=None;cx.notify();}cx.stop_propagation();}))
                .child(div().p_3().flex().items_center().gap_2().child(icon(Icon::Search,20.)).child(div().flex_1().child(self.quick_switcher.input.clone())).child(icon_button("close-quick-switcher",Icon::Close,"Close quick switcher").on_click(cx.listener(|this,_,window,cx|this.close_quick_switcher(window,cx)))))
                .child(div().px_4().pb_2().text_size(px(10.)).text_color(rgb(MUTED)).child(caption))
                .children(self.quick_switcher.results.iter().enumerate().skip(first).take(visible).map(|(index,row)|{
                    let target=row.target;
                    div().id(("quick-switcher-result",index)).mx_2().px_3().h(px(48.)).flex().items_center().gap_3().rounded_md().when(index==self.quick_switcher.selected,|v|v.bg(rgb(0x514065))).hover(|v|v.bg(rgb(0x3b3349)))
                        .child(match &row.kind{RowKind::User(user)=>crate::avatar_images::user_avatar(user,self.media_endpoint.as_deref(),30.),RowKind::Voice=>icon(Icon::Volume,25.).into_any_element(),RowKind::Guild=>icon(Icon::Chat,25.).into_any_element(),RowKind::Group=>icon(Icon::Chat,25.).into_any_element(),RowKind::Text=>div().w(px(25.)).text_size(px(23.)).text_color(rgb(MUTED)).child("#").into_any_element()})
                        .child(div().flex_1().min_w_0().child(div().text_size(px(14.)).when(row.unread,|v|v.font_weight(FontWeight::SEMIBOLD)).text_ellipsis().child(row.title.clone())).child(div().text_size(px(11.)).text_color(rgb(MUTED)).text_ellipsis().child(row.subtitle.clone())))
                        .when(row.mentions>0,|v|v.child(div().px_2().rounded_full().bg(rgb(0xb34768)).text_size(px(11.)).child(row.mentions.to_string())))
                        .on_click(cx.listener(move|this,_,window,cx|this.activate_quick_result(target,window,cx)))
                }))
                .when(count==0,|v|v.child(div().p_5().text_color(rgb(MUTED)).child("No matching channels or people")))
                .child(div().mt_2().px_4().py_3().border_t_1().border_color(rgb(0x40384d)).text_size(px(11.)).text_color(rgb(MUTED)).child("↑ ↓ to navigate · Enter to open · Esc to close    # text   ! voice   @ people   * communities"))
            ).into_any_element()
    }
}
#[cfg(test)]
mod tests {
    use super::{query_mode, score};
    #[test]
    fn source_prefix_modes_and_unicode_ranking() {
        assert_eq!(query_mode(" # café "), (Some('#'), "café".into()));
        assert_eq!(score("café", "CAFÉ"), Some(0));
        assert!(score("general", "gnr").is_some());
        assert!(score("general", "xyz").is_none());
        assert!(score("general", "gen").unwrap() < score("engineering", "gin").unwrap());
    }
}
