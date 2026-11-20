// SPDX-License-Identifier: AGPL-3.0-or-later
//! Compact profile surface. Display precedence follows features/user/models/Profile.ts;
//! geometry follows UserProfileSurfaceGeometry.ts. Remote bio remains inert text.
use fluxer_client::{
    DirectMessageStatus, ProfileSnapshot, ProfileStatus, RelationshipAction,
    RelationshipMutationStatus,
};
use fluxer_core::{Member, ProfileDetails, Role, Snowflake, User};
use gpui::{prelude::*, *};

#[derive(Clone)]
pub(super) struct ProfileOpen {
    pub user: User,
    pub guild_id: Option<Snowflake>,
    pub member: Option<Member>,
    pub roles: Vec<Role>,
    pub manageable_roles: Vec<Snowflake>,
    pub media_endpoint: Option<String>,
    pub community_name: Option<String>,
    pub anchor: Point<Pixels>,
    pub webhook: bool,
    pub can_message: bool,
    pub blocked: bool,
    pub friend_action: Option<RelationshipAction>,
    pub can_relationship: bool,
}

pub(super) struct ProfileUi {
    pub popup: Entity<ProfilePopup>,
}
impl ProfileUi {
    pub fn new(cx: &mut Context<super::Fluxer>) -> Self {
        Self {
            popup: cx.new(ProfilePopup::new),
        }
    }
    pub fn reset(&mut self, cx: &mut Context<super::Fluxer>) {
        self.popup.update(cx, |popup, cx| {
            popup.open = None;
            popup.snapshot = None;
            popup.previous_focus = None;
            popup.armed = false;
            popup.copied = false;
            cx.notify();
        });
    }
    pub fn is_open(&self, cx: &App) -> bool {
        self.popup.read(cx).is_open()
    }
}

#[derive(Clone, Debug)]
pub(super) enum ProfileAction {
    Retry {
        user_id: Snowflake,
        guild_id: Option<Snowflake>,
    },
    Message {
        user_id: Snowflake,
    },
    Relationship {
        user_id: Snowflake,
        action: RelationshipAction,
    },
    SetRole {
        guild_id: Snowflake,
        user_id: Snowflake,
        role_id: Snowflake,
        add: bool,
    },
    Link(String),
    Closed,
}
pub(super) struct ProfilePopup {
    open: Option<ProfileOpen>,
    snapshot: Option<ProfileSnapshot>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    scroll: ScrollHandle,
    selected: usize,
    armed: bool,
    copied: bool,
    presenting_loading: bool,
    full: bool,
    trigger_bounds: Option<Bounds<Pixels>>,
    bio_links: Vec<String>,
    confirm_blocked_dm: bool,
    confirm_block: bool,
    role_picker: bool,
    role_pending: bool,
    role_error: Option<String>,
    dm_status: Option<DirectMessageStatus>,
    relationship_status: Option<RelationshipMutationStatus>,
}
impl EventEmitter<ProfileAction> for ProfilePopup {}
impl ProfilePopup {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            open: None,
            snapshot: None,
            focus: cx.focus_handle(),
            previous_focus: None,
            scroll: ScrollHandle::new(),
            selected: 0,
            armed: false,
            copied: false,
            presenting_loading: false,
            full: false,
            trigger_bounds: None,
            bio_links: Vec::new(),
            confirm_blocked_dm: false,
            confirm_block: false,
            role_picker: false,
            role_pending: false,
            role_error: None,
            dm_status: None,
            relationship_status: None,
        }
    }
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }
    pub fn current_scope(&self) -> Option<(Snowflake, Option<Snowflake>)> {
        self.open.as_ref().map(|open| (open.user.id, open.guild_id))
    }
    pub fn open(&mut self, config: ProfileOpen, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_open() {
            self.previous_focus = window.focused(cx);
        }
        self.open = Some(config);
        self.full = false;
        self.trigger_bounds = None;
        self.bio_links.clear();
        self.snapshot = None;
        self.presenting_loading = true;
        self.scroll = ScrollHandle::new();
        self.selected = 0;
        self.armed = false;
        self.copied = false;
        self.confirm_blocked_dm = false;
        self.confirm_block = false;
        self.role_picker = false;
        self.role_pending = false;
        self.role_error = None;
        self.dm_status = None;
        self.relationship_status = None;
        window.focus(&self.focus);
        cx.notify();
    }
    pub fn apply_role_result(
        &mut self,
        guild: Snowflake,
        user: Snowflake,
        role: Snowflake,
        add: bool,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.current_scope() != Some((user, Some(guild))) {
            return;
        }
        self.role_pending = false;
        self.role_error = error.clone();
        if error.is_none() {
            let update = |member: &mut Member| {
                member.roles.retain(|id| *id != role);
                if add {
                    member.roles.push(role);
                }
            };
            if let Some(member) = self.open.as_mut().and_then(|o| o.member.as_mut()) {
                update(member);
            }
            if let Some(member) = self
                .snapshot
                .as_mut()
                .and_then(|s| s.profile.as_mut())
                .and_then(|p| p.guild_member.as_mut())
            {
                update(member);
            }
        }
        cx.notify();
    }
    pub fn set_trigger_bounds(&mut self, bounds: Option<Bounds<Pixels>>) {
        self.trigger_bounds = bounds;
    }
    pub fn apply_relationship_status(
        &mut self,
        user: Snowflake,
        status: RelationshipMutationStatus,
        cx: &mut Context<Self>,
    ) {
        if self.open.as_ref().is_some_and(|open| open.user.id == user) {
            self.relationship_status = Some(status);
            cx.notify();
        }
    }
    pub fn apply_relationship_kind(
        &mut self,
        kind: Option<u8>,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(open) = &mut self.open {
            let old = (open.blocked, open.friend_action);
            (open.blocked, open.friend_action) =
                relationship_actions(kind, enabled && open.can_message);
            if old != (open.blocked, open.friend_action) {
                self.armed = false;
            }
            cx.notify();
        }
    }
    fn block_action(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.relationship_status,
            Some(RelationshipMutationStatus::Pending)
        ) {
            return;
        }
        if let Some(open) = &self.open {
            if !open.can_relationship {
                return;
            }
            if open.blocked {
                self.relationship_status = Some(RelationshipMutationStatus::Pending);
                cx.emit(ProfileAction::Relationship {
                    user_id: open.user.id,
                    action: RelationshipAction::Remove,
                });
            } else {
                self.confirm_block = true;
                self.selected = 0;
                self.armed = false;
                cx.notify();
            }
        }
    }
    fn confirm_primary(&mut self, cx: &mut Context<Self>) {
        if self.confirm_block {
            self.confirm_block = false;
            self.relationship_status = Some(RelationshipMutationStatus::Pending);
            if let Some(open) = &self.open {
                cx.emit(ProfileAction::Relationship {
                    user_id: open.user.id,
                    action: RelationshipAction::Block,
                });
            }
            cx.notify();
        } else {
            self.confirm_blocked_dm = false;
            self.dm_status = Some(DirectMessageStatus::Pending);
            if let Some(open) = &self.open {
                cx.emit(ProfileAction::Message {
                    user_id: open.user.id,
                });
            }
            cx.notify();
        }
    }
    fn cancel_confirmation(&mut self, cx: &mut Context<Self>) {
        self.confirm_blocked_dm = false;
        self.confirm_block = false;
        self.armed = false;
        cx.notify();
    }
    fn friend_action(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.relationship_status,
            Some(RelationshipMutationStatus::Pending)
        ) {
            return;
        }
        if let Some(open) = &self.open
            && let Some(action) = open.friend_action
        {
            self.relationship_status = Some(RelationshipMutationStatus::Pending);
            cx.emit(ProfileAction::Relationship {
                user_id: open.user.id,
                action,
            });
        }
    }
    pub fn apply_dm_status(
        &mut self,
        user: Snowflake,
        status: DirectMessageStatus,
        cx: &mut Context<Self>,
    ) {
        if self.open.as_ref().is_some_and(|open| open.user.id == user) {
            self.dm_status = Some(status);
            cx.notify();
        }
    }
    pub fn apply_snapshot(&mut self, snapshot: ProfileSnapshot, cx: &mut Context<Self>) {
        if self.open.as_ref().is_some_and(|open| {
            !open.webhook && open.user.id == snapshot.user_id && open.guild_id == snapshot.guild_id
        }) {
            if !matches!(snapshot.status, ProfileStatus::Failed(_)) {
                self.selected = 0;
            }
            self.armed = false;
            self.snapshot = Some(snapshot);
            cx.notify();
        }
    }
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.take().is_none() {
            return;
        }
        self.snapshot = None;
        if let Some(focus) = self.previous_focus.take() {
            window.focus(&focus);
        }
        cx.emit(ProfileAction::Closed);
        cx.notify();
    }
    fn failed(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| matches!(snapshot.status, ProfileStatus::Failed(_)))
    }
    fn activate(&mut self, retry: bool, cx: &mut Context<Self>) {
        let Some(open) = &self.open else {
            return;
        };
        if retry {
            cx.emit(ProfileAction::Retry {
                user_id: open.user.id,
                guild_id: open.guild_id,
            });
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(open.user.id.to_string()));
            self.copied = true;
            cx.notify();
        }
    }
    fn message(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.open else { return };
        if !open.can_message || matches!(self.dm_status, Some(DirectMessageStatus::Pending)) {
            return;
        }
        if open.blocked && !self.confirm_blocked_dm {
            self.confirm_blocked_dm = true;
            self.selected = 0;
            self.armed = false;
            cx.notify();
        } else {
            self.dm_status = Some(DirectMessageStatus::Pending);
            cx.emit(ProfileAction::Message {
                user_id: open.user.id,
            });
            cx.notify();
        }
    }
    fn action_count(&self) -> usize {
        1 + usize::from(!self.full && self.open.as_ref().is_some_and(|o| !o.webhook))
            + usize::from(self.failed())
            + usize::from(self.open.as_ref().is_some_and(|o| o.can_message))
            + usize::from(
                self.open
                    .as_ref()
                    .is_some_and(|o| o.friend_action.is_some()),
            )
            + usize::from(self.open.as_ref().is_some_and(|o| o.can_relationship))
    }
    fn role_options(&self) -> Vec<(Snowflake, bool)> {
        let Some(open) = &self.open else {
            return Vec::new();
        };
        let profile = self.snapshot.as_ref().and_then(|s| s.profile.as_ref());
        let ready = self
            .snapshot
            .as_ref()
            .is_some_and(|s| matches!(s.status, ProfileStatus::Ready));
        let member = if ready || profile.is_some() {
            profile.and_then(|p| p.guild_member.as_ref())
        } else {
            open.member.as_ref()
        };
        let Some(member) = member else {
            return Vec::new();
        };
        open.roles
            .iter()
            .filter(|role| open.manageable_roles.contains(&role.id))
            .map(|role| (role.id, !member.roles.contains(&role.id)))
            .collect()
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.presenting_loading && event.keystroke.key != "escape" {
            self.armed = false;
            cx.stop_propagation();
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                if self.confirm_blocked_dm || self.confirm_block {
                    self.cancel_confirmation(cx);
                } else {
                    self.dismiss(window, cx);
                }
            }
            "tab" => {
                let count = if self.confirm_blocked_dm || self.confirm_block {
                    2
                } else {
                    1 + usize::from(!self.full && self.open.as_ref().is_some_and(|o| !o.webhook))
                        + usize::from(self.failed())
                        + usize::from(self.open.as_ref().is_some_and(|o| o.can_message))
                        + usize::from(
                            self.open
                                .as_ref()
                                .is_some_and(|o| o.friend_action.is_some()),
                        )
                        + usize::from(self.open.as_ref().is_some_and(|o| o.can_relationship))
                        + self.bio_links.len()
                        + usize::from(!self.role_options().is_empty())
                        + if self.role_picker {
                            self.role_options().len()
                        } else {
                            0
                        }
                };
                self.selected = if event.keystroke.modifiers.shift {
                    (self.selected + count - 1) % count
                } else {
                    (self.selected + 1) % count
                };
                self.armed = false;
                cx.notify();
            }
            "enter" | "space" if !event.keystroke.modifiers.modified() => {
                if !event.is_held {
                    self.armed = true;
                }
            }
            "up" => {
                self.scroll
                    .set_offset(point(px(0.), self.scroll.offset().y + px(32.)));
                cx.notify();
            }
            "down" => {
                self.scroll
                    .set_offset(point(px(0.), self.scroll.offset().y - px(32.)));
                cx.notify();
            }
            _ => {}
        }
        cx.stop_propagation();
    }
    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.presenting_loading {
            self.armed = false;
            cx.stop_propagation();
            return;
        }
        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
            let armed = std::mem::take(&mut self.armed);
            if armed && !event.keystroke.modifiers.modified() {
                if self.confirm_blocked_dm || self.confirm_block {
                    if self.selected == 0 {
                        self.cancel_confirmation(cx);
                    } else {
                        self.confirm_primary(cx);
                    }
                } else if self.selected == 0 {
                    self.activate(false, cx);
                } else if !self.full
                    && self.open.as_ref().is_some_and(|o| !o.webhook)
                    && self.selected == 1
                {
                    self.full = true;
                    self.selected = 0;
                    self.scroll = ScrollHandle::new();
                    cx.notify();
                } else if self.failed()
                    && self.selected
                        == 1 + usize::from(
                            !self.full && self.open.as_ref().is_some_and(|o| !o.webhook),
                        )
                {
                    self.activate(true, cx);
                } else if self.open.as_ref().is_some_and(|o| o.can_message)
                    && self.selected
                        == 1 + usize::from(
                            !self.full && self.open.as_ref().is_some_and(|o| !o.webhook),
                        ) + usize::from(self.failed())
                {
                    self.message(cx);
                } else if self
                    .open
                    .as_ref()
                    .is_some_and(|o| o.friend_action.is_some())
                    && self.selected
                        == 1 + usize::from(
                            !self.full && self.open.as_ref().is_some_and(|o| !o.webhook),
                        ) + usize::from(self.failed())
                            + usize::from(self.open.as_ref().is_some_and(|o| o.can_message))
                {
                    self.friend_action(cx);
                } else {
                    let base = self.action_count();
                    if self.selected >= base {
                        let index = self.selected - base;
                        if let Some(url) = self.bio_links.get(index) {
                            cx.emit(ProfileAction::Link(url.clone()));
                        } else {
                            let role_index = index - self.bio_links.len();
                            let options = self.role_options();
                            if role_index == 0 && !options.is_empty() {
                                self.role_picker = !self.role_picker;
                                cx.notify();
                            } else if self.role_picker
                                && !self.role_pending
                                && let Some(&(role_id, add)) =
                                    role_index.checked_sub(1).and_then(|i| options.get(i))
                                && let Some(open) = &self.open
                                && let Some(guild_id) = open.guild_id
                            {
                                self.role_pending = true;
                                self.role_error = None;
                                cx.emit(ProfileAction::SetRole {
                                    guild_id,
                                    user_id: open.user.id,
                                    role_id,
                                    add,
                                });
                                cx.notify();
                            }
                        }
                    } else {
                        self.block_action(cx);
                    }
                }
            }
        }
        cx.stop_propagation();
    }
}

fn label(text: &str, size: f32, cx: &mut App) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .min_w_0()
        .max_w_full()
        .children(
            text.split_inclusive(char::is_whitespace)
                .map(|word| crate::emoji_sprites::unicode_text(word, size, false, cx)),
        )
        .into_any_element()
}
// AccentColorUtils.ts: effective profile accent, sampled avatar color,
// source default-avatar palette, then the application's default accent.
fn profile_accent(
    user: &User,
    global: Option<&ProfileDetails>,
    member: Option<&ProfileDetails>,
) -> u32 {
    let explicit = member
        .and_then(|p| p.accent_color)
        .or_else(|| global.and_then(|p| p.accent_color));
    if let Some(color) = explicit.or_else(|| {
        user.extra
            .get("avatar_color")
            .and_then(serde_json::Value::as_i64)
    }) {
        return color as u32 & 0xffffff;
    }
    if user.avatar.is_none() {
        return [0x4641d9, 0xf0b100, 0x00bba7, 0x2b7fff, 0xad46ff, 0x6a7282]
            [(user.id.get() % 6) as usize];
    }
    0x4641d9
}
fn text_override<'a>(global: Option<&'a str>, member: Option<&'a str>) -> &'a str {
    member.or(global).unwrap_or("")
}
fn placement(anchor: Point<Pixels>, viewport: Size<Pixels>) -> (Point<Pixels>, f32, f32) {
    let bounds = crate::overlay::popover_bounds(
        Bounds::new(anchor, size(px(0.), px(0.))),
        viewport,
        size(px(300.), px(520.)),
        px(8.),
        px(12.),
    );
    (
        bounds.origin,
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    )
}

impl ProfilePopup {
    fn surface(&self, content: AnyElement, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(open) = &self.open else {
            return div().into_any_element();
        };
        let (origin, width, height) = if let Some(trigger) = self.trigger_bounds {
            let fitted = crate::overlay::popover_bounds(
                trigger,
                window.viewport_size(),
                size(px(300.), px(520.)),
                px(8.),
                px(12.),
            );
            (
                fitted.origin,
                f32::from(fitted.size.width),
                f32::from(fitted.size.height),
            )
        } else {
            placement(open.anchor, window.viewport_size())
        };
        if self.full {
            let viewport = window.viewport_size();
            let fitted =
                crate::overlay::surface_size(viewport, size(px(600.), viewport.height), px(16.));
            let width = f32::from(fitted.width);
            let height = f32::from(fitted.height);
            return deferred(
                div()
                    .absolute()
                    .inset_0()
                    .bg(rgba(0x00000099))
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.dismiss(window, cx)),
                    )
                    .child(
                        div()
                            .id("native-full-profile")
                            .track_focus(&self.focus)
                            .occlude()
                            .w(px(width))
                            .max_h(px(height))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .rounded_lg()
                            .bg(rgb(0x232428))
                            .text_color(rgb(0xf2f3f5))
                            .shadow_lg()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .capture_key_down(cx.listener(Self::key_down))
                            .capture_key_up(cx.listener(Self::key_up))
                            .child(content),
                    ),
            )
            .with_priority(9)
            .into_any_element();
        }
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(origin)
                .anchor(Corner::TopLeft)
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .id("native-profile-popup")
                        .track_focus(&self.focus)
                        .occlude()
                        .w(px(width))
                        .max_h(px(height))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(0x4c4458))
                        .bg(rgb(0x232428))
                        .text_color(rgb(0xf2f3f5))
                        .shadow_lg()
                        .on_mouse_down_out(
                            cx.listener(|this, _, window, cx| this.dismiss(window, cx)),
                        )
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .capture_key_down(cx.listener(Self::key_down))
                        .capture_key_up(cx.listener(Self::key_up))
                        .child(content),
                ),
        )
        .with_priority(9)
        .into_any_element()
    }
    fn loading_surface(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        // Match the classic popout skeleton: reserve the banner/avatar geometry,
        // without briefly presenting a different accent, avatar or partial bio.
        self.surface(
            div()
                .w_full()
                .flex()
                .flex_col()
                .child(
                    div().relative().h(px(105.)).bg(rgb(0x35363c)).child(
                        div()
                            .absolute()
                            .left(px(10.))
                            .top(px(55.))
                            .size(px(88.))
                            .rounded_full()
                            .border_4()
                            .border_color(rgb(0x232428))
                            .bg(rgb(0x44464e)),
                    ),
                )
                .child(
                    div()
                        .pt(px(52.))
                        .pb_6()
                        .px_3()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().w(px(145.)).h(px(18.)).rounded_md().bg(rgb(0x35363c)))
                        .child(div().w(px(95.)).h(px(12.)).rounded_md().bg(rgb(0x35363c)))
                        .child(
                            div()
                                .mt_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(12.))
                                .text_color(rgb(0xb5bac1))
                                .child(
                                    crate::controls::icon(crate::controls::Icon::Refresh, 14.)
                                        .with_animation(
                                            "profile-loading",
                                            Animation::new(std::time::Duration::from_millis(900))
                                                .repeat(),
                                            |icon, delta| {
                                                icon.with_transformation(Transformation::rotate(
                                                    percentage(delta),
                                                ))
                                            },
                                        ),
                                )
                                .child("Loading profile…"),
                        ),
                )
                .into_any_element(),
            window,
            cx,
        )
    }
}

impl Render for ProfilePopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(open) = &self.open else {
            return div().into_any_element();
        };
        if self.confirm_blocked_dm || self.confirm_block {
            let is_block = self.confirm_block;
            let title = if is_block { "Block user" } else { "Open DM" };
            let name = open.user.display_name().to_owned();
            return div()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000099))
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("blocked-dm-confirm")
                        .track_focus(&self.focus)
                        .w(px(420.))
                        .p_5()
                        .rounded_lg()
                        .bg(rgb(0x232428))
                        .text_color(rgb(0xf2f3f5))
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .capture_key_down(cx.listener(Self::key_down))
                        .capture_key_up(cx.listener(Self::key_up))
                        .child(
                            div()
                                .text_size(px(20.))
                                .font_weight(FontWeight::BOLD)
                                .child(title),
                        )
                        .child(if is_block {
                            format!("Block {name}? They will be added to your blocked list.")
                        } else {
                            format!(
                                "{name} is blocked. Are you sure you want to open a direct message?"
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_3()
                                .child(
                                    div()
                                        .id("blocked-dm-cancel")
                                        .cursor_pointer()
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .bg(rgb(if self.selected == 0 {
                                            0x535064
                                        } else {
                                            0x383a40
                                        }))
                                        .child("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cancel_confirmation(cx);
                                        })),
                                )
                                .child(
                                    div()
                                        .id("blocked-dm-accept")
                                        .cursor_pointer()
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .bg(rgb(if self.selected == 1 {
                                            0x7289ef
                                        } else {
                                            0x5865f2
                                        }))
                                        .child(title)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.confirm_primary(cx)),
                                        ),
                                ),
                        ),
                )
                .into_any_element();
        }
        let profile = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.profile.as_ref());
        let user = profile.map(|profile| &profile.user).unwrap_or(&open.user);
        // A completed response's membership is authoritative, even when absent.
        let ready = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| matches!(snapshot.status, ProfileStatus::Ready));
        let member = if ready || profile.is_some() {
            profile.and_then(|profile| profile.guild_member.as_ref())
        } else {
            open.member.as_ref()
        };
        let global = profile.map(|profile| &profile.user_profile);
        let guild_profile = if member.is_some() {
            profile.and_then(|profile| profile.guild_member_profile.as_ref())
        } else {
            None
        };
        let name = member
            .and_then(|member| member.nick.as_deref())
            .unwrap_or(user.display_name());
        let pronouns = text_override(
            global.and_then(|p| p.pronouns.as_deref()),
            guild_profile.and_then(|p| p.pronouns.as_deref()),
        );
        let bio = text_override(
            global.and_then(|p| p.bio.as_deref()),
            guild_profile.and_then(|p| p.bio.as_deref()),
        );
        let flags = member.and_then(|member| member.profile_flags).unwrap_or(0);
        let banner = if flags & 2 != 0 {
            None
        } else {
            guild_profile
                .and_then(|p| p.banner.as_deref())
                .or_else(|| global.and_then(|p| p.banner.as_deref()))
        };
        let accent_color = profile_accent(user, global, guild_profile);
        let banner_url = banner.and_then(|hash| {
            crate::avatar_images::profile_banner_url(
                user.id,
                open.guild_id
                    .filter(|_| guild_profile.and_then(|p| p.banner.as_ref()).is_some()),
                hash,
                open.media_endpoint.as_deref(),
            )
        });
        let avatar_url = crate::avatar_images::profile_avatar_url(
            user,
            member,
            open.guild_id,
            open.media_endpoint.as_deref(),
        );
        // Use the same URL/resource keys as Img, sharing its in-flight work and
        // decoded cache. Errors are settled too: show the normal accent/default
        // avatar instead of keeping a failed image behind an endless spinner.
        let mut assets_pending = false;
        for url in [banner_url.as_ref(), avatar_url.as_ref()]
            .into_iter()
            .flatten()
        {
            assets_pending |= window
                .use_asset::<ImgResourceLoader>(&Resource::Uri(url.clone().into()), cx)
                .is_none();
        }
        let metadata_pending = !open.webhook
            && self
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| matches!(snapshot.status, ProfileStatus::Loading));
        self.presenting_loading = metadata_pending || assets_pending;
        if self.presenting_loading {
            return self.loading_surface(window, cx);
        }
        let avatar = crate::avatar_images::profile_avatar(
            user,
            member,
            open.guild_id,
            open.media_endpoint.as_deref(),
            80.,
        );
        let mut content = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .pb_3()
            .pt(px(34.))
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(label(name, 20., cx)),
            );
        if name != user.display_name() {
            content = content.child(div().text_size(px(14.)).child(label(
                user.display_name(),
                14.,
                cx,
            )));
        }
        content = content.child(
            div()
                .text_size(px(13.))
                .text_color(rgb(0xb5bac1))
                .child(label(&user.username, 13., cx)),
        );
        if !pronouns.is_empty() {
            content = content.child(div().text_size(px(12.)).child(label(pronouns, 12., cx)));
        }
        if user.bot {
            content = content.child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(0xbcb9ff))
                    .child("BOT"),
            );
        }
        self.bio_links.clear();
        if !bio.is_empty() {
            let message = fluxer_core::Message {
                content: bio.to_owned(),
                ..Default::default()
            };
            let document = crate::message_markup::document(
                &message,
                &Default::default(),
                open.media_endpoint.as_deref(),
                crate::message_markup::MessageContext::InlinePreview,
            );
            let mut biography = div().mt_2().text_size(px(13.)).flex().flex_wrap();
            for (index, span) in document.spans.iter().enumerate() {
                let text = &document.text[span.range.clone()];
                if span.spoilers.is_empty()
                    && !span.style.code
                    && let Some(crate::message_markup::MessageTarget::Link(url)) = &span.target
                {
                    let url = url.clone();
                    let selected = self.selected == self.action_count() + self.bio_links.len();
                    self.bio_links.push(url.clone());
                    biography = biography.child(
                        div()
                            .id(("profile-bio-link", index))
                            .cursor_pointer()
                            .when(selected, |view| view.bg(rgb(0x535064)))
                            .text_color(rgb(0x83bdf5))
                            .child(label(text, 13., cx))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(ProfileAction::Link(url.clone()));
                            })),
                    );
                    continue;
                }
                let shown = if span.spoilers.is_empty() {
                    text
                } else {
                    "▰▰▰"
                };
                biography = biography.child(label(shown, 13., cx));
            }
            content = content.child(biography);
        }
        if !open.webhook && user.id.get() != 0 {
            // packages/snowflake/src/Snowflake.ts: Fluxer uses the 2015 epoch.
            let millis = (user.id.get() >> 22) as i64 + 1_420_070_400_000;
            if let Some(created) = chrono::DateTime::from_timestamp_millis(millis) {
                content = content
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .child("FLUXER MEMBER SINCE"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0xb5bac1))
                            .child(created.format("%b %-d, %Y").to_string()),
                    );
            }
        }
        if let Some(member) = member {
            if let Some(joined) = member
                .extra
                .get("joined_at")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            {
                content = content
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .child(
                                open.community_name
                                    .as_ref()
                                    .map(|name| format!("JOINED {name}"))
                                    .unwrap_or_else(|| "COMMUNITY MEMBER SINCE".into()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0xb5bac1))
                            .child(joined.format("%b %-d, %Y").to_string()),
                    );
            }
            let mut roles: Vec<_> = open
                .roles
                .iter()
                .filter(|role| member.roles.contains(&role.id))
                .collect();
            roles.sort_by_key(|role| std::cmp::Reverse(role.position));
            if !open.manageable_roles.is_empty() {
                let guild_id = open.guild_id.unwrap();
                let user_id = open.user.id;
                content = content.child(
                    div()
                        .id("profile-manage-roles")
                        .when(
                            self.selected == self.action_count() + self.bio_links.len(),
                            |view| view.bg(rgb(0x535064)),
                        )
                        .mt_2()
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(rgb(0x9baaff))
                        .child(if self.role_picker {
                            "Close roles"
                        } else {
                            "Manage roles"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.role_picker = !this.role_picker;
                            cx.notify();
                        })),
                );
                if self.role_picker {
                    for (index, role) in open
                        .roles
                        .iter()
                        .filter(|r| open.manageable_roles.contains(&r.id))
                        .enumerate()
                    {
                        let role_id = role.id;
                        let add = !member.roles.contains(&role_id);
                        content = content.child(
                            div()
                                .id(SharedString::from(format!("profile-role-{role_id}")))
                                .when(
                                    self.selected
                                        == self.action_count() + self.bio_links.len() + 1 + index,
                                    |view| view.bg(rgb(0x535064)),
                                )
                                .px_2()
                                .py_1()
                                .text_size(px(12.))
                                .cursor_pointer()
                                .child(format!("{} {}", if add { "○" } else { "●" }, role.name))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.role_pending {
                                        this.role_pending = true;
                                        this.role_error = None;
                                        cx.emit(ProfileAction::SetRole {
                                            guild_id,
                                            user_id,
                                            role_id,
                                            add,
                                        });
                                        cx.notify();
                                    }
                                })),
                        );
                    }
                }
                if let Some(error) = &self.role_error {
                    content = content.child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0xf38ba8))
                            .child(format!("Couldn't update role: {error}")),
                    );
                }
            }
            if !roles.is_empty() {
                content = content
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .child("ROLES"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(roles.into_iter().map(|role| {
                                div()
                                    .rounded_md()
                                    .px_2()
                                    .py_1()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .bg(rgb(0x383a40))
                                    .child(div().size(px(6.)).rounded_full().flex_shrink_0().bg(
                                        rgb(if role.color == 0 {
                                            0x99aab5
                                        } else {
                                            role.color & 0xffffff
                                        }),
                                    ))
                                    .text_size(px(12.))
                                    .child(label(&role.name, 12., cx))
                            })),
                    );
            }
        }
        if open.webhook {
            content = content.child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(0xb5bac1))
                    .child("Webhook identity"),
            );
        } else {
            match self.snapshot.as_ref().map(|snapshot| &snapshot.status) {
                Some(ProfileStatus::Ready) => {
                    if profile.is_some_and(|profile| profile.profile_limited) {
                        content = content.child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(0xb5bac1))
                                .child("This profile has limited information."),
                        );
                    }
                }
                Some(ProfileStatus::Failed(error)) => {
                    content = content
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(0xf2b8b5))
                                .child(format!("Couldn't load profile: {error}")),
                        )
                        .child(
                            div()
                                .id("profile-retry")
                                .cursor_pointer()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(
                                    if self.selected == 1 + usize::from(!self.full && !open.webhook)
                                    {
                                        0x535064
                                    } else {
                                        0x383a40
                                    },
                                ))
                                .child("Retry")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.activate(true, cx);
                                })),
                        );
                }
                _ => {
                    content = content.child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0xb5bac1))
                            .child("Loading profile…"),
                    );
                }
            }
        }
        if !self.full && !open.webhook {
            content = content.child(
                div()
                    .id("profile-view-full")
                    .cursor_pointer()
                    .mt_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_size(px(12.))
                    .bg(rgb(if self.selected == 1 {
                        0x535064
                    } else {
                        0x383a40
                    }))
                    .child("View full profile")
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.full = true;
                        this.selected = 0;
                        this.scroll = ScrollHandle::new();
                        cx.notify();
                    })),
            );
        }
        if open.can_message {
            content = content.child(
                div()
                    .id("profile-message")
                    .cursor_pointer()
                    .mt_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(
                        if self.selected
                            == 1 + usize::from(
                                !self.full && self.open.as_ref().is_some_and(|o| !o.webhook),
                            ) + usize::from(self.failed())
                        {
                            0x7289ef
                        } else {
                            0x5865f2
                        },
                    ))
                    .text_size(px(12.))
                    .child(if open.blocked { "Open DM" } else { "Message" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.message(cx);
                    })),
            );
        }
        if let Some(action) = open.friend_action {
            let pending = matches!(
                self.relationship_status,
                Some(RelationshipMutationStatus::Pending)
            );
            content = content.child(
                div()
                    .id("profile-friend-action")
                    .cursor_pointer()
                    .mt_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(
                        if self.selected
                            == 1 + usize::from(
                                !self.full && self.open.as_ref().is_some_and(|o| !o.webhook),
                            ) + usize::from(self.failed())
                                + usize::from(open.can_message)
                        {
                            0x535064
                        } else {
                            0x383a40
                        },
                    ))
                    .text_size(px(12.))
                    .child(if pending {
                        "Updating…"
                    } else if action == RelationshipAction::Send {
                        "Add Friend"
                    } else {
                        "Accept Friend Request"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.friend_action(cx);
                    })),
            );
        }
        if open.can_relationship {
            content = content.child(
                div()
                    .id("profile-block-action")
                    .cursor_pointer()
                    .mt_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_size(px(12.))
                    .text_color(rgb(0xf2b8b5))
                    .bg(rgb(
                        if self.selected
                            == 1 + usize::from(
                                !self.full && self.open.as_ref().is_some_and(|o| !o.webhook),
                            ) + usize::from(self.failed())
                                + usize::from(open.can_message)
                                + usize::from(open.friend_action.is_some())
                        {
                            0x535064
                        } else {
                            0x232428
                        },
                    ))
                    .child(if open.blocked { "Unblock" } else { "Block" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.block_action(cx);
                    })),
            );
        }
        if let Some(RelationshipMutationStatus::Failed(message)) = &self.relationship_status {
            content = content.child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(0xf2b8b5))
                    .child(format!("Couldn't update relationship: {message}")),
            );
        }
        if let Some(status) = &self.dm_status {
            match status {
                DirectMessageStatus::Pending => {
                    content =
                        content.child(div().text_size(px(12.)).child("Opening direct message…"))
                }
                DirectMessageStatus::Failed(message) => {
                    content = content.child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0xf2b8b5))
                            .child(format!("Couldn't open direct message: {message}")),
                    )
                }
                DirectMessageStatus::Complete => {}
            }
        }
        content = content.child(
            div()
                .id("profile-copy-id")
                .cursor_pointer()
                .mt_2()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(if self.selected == 0 {
                    0x535064
                } else {
                    0x383a40
                }))
                .text_size(px(12.))
                .child(if self.copied {
                    "Copied User ID"
                } else {
                    "Copy User ID"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.activate(false, cx);
                })),
        );
        let header = div()
            .relative()
            .h(px(105.))
            .flex_shrink_0()
            .bg(rgb(accent_color))
            .when_some(banner_url, |view, url| {
                view.child(
                    div()
                        .absolute()
                        .inset_0()
                        .overflow_hidden()
                        .child(img(url).size_full().object_fit(ObjectFit::Cover)),
                )
            })
            .child(
                div()
                    .absolute()
                    .left(px(10.))
                    .top(px(55.))
                    .size(px(88.))
                    .rounded_full()
                    .bg(rgb(0x232428))
                    .p_1()
                    .child(avatar),
            );
        self.surface(
            div().child(header).child(content).into_any_element(),
            window,
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{placement, profile_accent, text_override};
    use fluxer_core::{ProfileDetails, User};
    use gpui::{point, px, size};
    #[test]
    fn accent_uses_profile_then_avatar_then_source_defaults_including_black() {
        let mut user = User {
            id: 1.into(),
            ..Default::default()
        };
        assert_eq!(profile_accent(&user, None, None), 0xf0b100);
        user.extra
            .insert("avatar_color".into(), serde_json::json!(0x112233));
        assert_eq!(profile_accent(&user, None, None), 0x112233);
        let global = ProfileDetails {
            accent_color: Some(0x445566),
            ..Default::default()
        };
        let member = ProfileDetails {
            accent_color: Some(0),
            ..Default::default()
        };
        assert_eq!(profile_accent(&user, Some(&global), None), 0x445566);
        assert_eq!(profile_accent(&user, Some(&global), Some(&member)), 0);
    }
    #[test]
    fn member_empty_text_overrides_global() {
        assert_eq!(text_override(Some("global"), Some("")), "");
        assert_eq!(text_override(Some("global"), None), "global");
    }
    #[test]
    fn popup_flips_and_fits_short_windows() {
        let (origin, width, height) =
            placement(point(px(790.), px(590.)), size(px(800.), px(600.)));
        assert!(origin.x + px(width) <= px(792.));
        assert!(origin.y + px(height) <= px(592.));
        let (_, width, height) = placement(point(px(50.), px(50.)), size(px(180.), px(200.)));
        assert_eq!((width, height), (164., 184.));
    }
}

fn relationship_actions(kind: Option<u8>, enabled: bool) -> (bool, Option<RelationshipAction>) {
    let action = if enabled {
        match kind {
            None => Some(RelationshipAction::Send),
            Some(3) => Some(RelationshipAction::Accept),
            _ => None,
        }
    } else {
        None
    };
    (kind == Some(2), action)
}

#[cfg(test)]
mod relationship_reconciliation_tests {
    use super::{RelationshipAction, relationship_actions};
    #[test]
    fn gateway_relationship_kinds_update_profile_actions() {
        assert_eq!(
            relationship_actions(None, true),
            (false, Some(RelationshipAction::Send))
        );
        assert_eq!(
            relationship_actions(Some(3), true),
            (false, Some(RelationshipAction::Accept))
        );
        for kind in [1, 4] {
            assert_eq!(relationship_actions(Some(kind), true), (false, None));
        }
        assert_eq!(relationship_actions(Some(2), true), (true, None));
        assert_eq!(relationship_actions(None, false), (false, None));
    }
}
