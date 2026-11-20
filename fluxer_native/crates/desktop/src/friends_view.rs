// SPDX-License-Identifier: AGPL-3.0-or-later
//! Friends and requests from the account's gateway relationship cache.
use super::*;
use crate::controls::quiet_button;
use fluxer_client::{DirectMessageStatus, RelationshipAction, RelationshipMutationStatus};
use fluxer_core::PresenceStatus;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    Online,
    All,
    Pending,
    Blocked,
}
pub(super) struct FriendsUi {
    filter: Filter,
    group_selecting: bool,
    group_target: Option<Snowflake>,
    group_recipients: std::collections::BTreeSet<Snowflake>,
    pub status: BTreeMap<Snowflake, RelationshipMutationStatus>,
    pub dm_status: BTreeMap<Snowflake, DirectMessageStatus>,
    pub confirm: Option<(Snowflake, RelationshipAction, String)>,
    pub group_confirm: Option<(Snowflake, Snowflake, String, bool, Snowflake)>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    selected: bool,
    armed: bool,
}
impl FriendsUi {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            filter: Filter::Online,
            group_selecting: false,
            group_target: None,
            group_recipients: std::collections::BTreeSet::new(),
            status: BTreeMap::new(),
            dm_status: BTreeMap::new(),
            confirm: None,
            group_confirm: None,
            focus: cx.focus_handle(),
            previous_focus: None,
            selected: false,
            armed: false,
        }
    }
}
impl Fluxer {
    pub(super) fn add_friends_to_group(&mut self, channel: Snowflake, cx: &mut Context<Self>) {
        self.friends_ui.group_target = Some(channel);
        self.friends_ui.group_selecting = true;
        self.friends_ui.group_recipients.clear();
        self.friends_ui.filter = Filter::All;
        self.pane = Pane::Friends;
        cx.notify();
    }
    pub(super) fn confirm_group_removal(
        &mut self,
        channel: Snowflake,
        user: Snowflake,
        name: String,
        leave: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(actor) = self.state.current_user.as_ref().map(|u| u.id) else {
            return;
        };
        self.friends_ui.group_confirm = Some((channel, user, name, leave, actor));
        self.friends_ui.previous_focus = window.focused(cx);
        self.friends_ui.selected = false;
        self.friends_ui.armed = false;
        window.focus(&self.friends_ui.focus);
        cx.notify();
    }
    fn relationship_action(
        &mut self,
        user: Snowflake,
        action: RelationshipAction,
        name: String,
        confirm: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.demo {
            return;
        }
        if confirm {
            self.friends_ui.confirm = Some((user, action, name));
            self.friends_ui.previous_focus = window.focused(cx);
            self.friends_ui.selected = false;
            self.friends_ui.armed = false;
            window.focus(&self.friends_ui.focus);
            cx.notify();
        } else {
            self.command(
                Command::ChangeRelationship {
                    user_id: user,
                    action,
                },
                cx,
            );
        }
    }
    fn close_relationship_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.friends_ui.confirm = None;
        self.friends_ui.group_confirm = None;
        self.friends_ui.armed = false;
        if let Some(focus) = self.friends_ui.previous_focus.take() {
            window.focus(&focus);
        }
        cx.notify();
    }
    fn accept_relationship_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((channel_id, user_id, _, _, actor)) = self.friends_ui.group_confirm.clone() {
            self.close_relationship_confirmation(window, cx);
            if self
                .state
                .current_user
                .as_ref()
                .is_some_and(|u| u.id == actor)
            {
                self.command(
                    Command::RemoveGroupRecipient {
                        channel_id,
                        user_id,
                    },
                    cx,
                );
            }
        } else if let Some((user, action, _)) = self.friends_ui.confirm.clone() {
            self.close_relationship_confirmation(window, cx);
            self.command(
                Command::ChangeRelationship {
                    user_id: user,
                    action,
                },
                cx,
            );
        }
    }
    pub(super) fn relationship_confirmation(&self, cx: &mut Context<Self>) -> AnyElement {
        let (title, description) =
            if let Some((_, _, name, leave, _)) = &self.friends_ui.group_confirm {
                if *leave {
                    ("Leave group", "Leave this group DM?".to_owned())
                } else {
                    (
                        "Remove member",
                        format!("Remove {} from this group DM?", self.streaming().name(name)),
                    )
                }
            } else if let Some((_, action, name)) = &self.friends_ui.confirm {
                let name = self.streaming().name(name);
                match action {
                    RelationshipAction::Block => (
                        "Block user",
                        format!("Block {name}? They will be added to your blocked list."),
                    ),
                    _ => (
                        "Remove relationship",
                        format!("Remove your relationship with {name}?"),
                    ),
                }
            } else {
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
                    .id("relationship-confirm-dialog")
                    .track_focus(&self.friends_ui.focus)
                    .w(px(420.))
                    .p_5()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => this.close_relationship_confirmation(window, cx),
                            "tab" => {
                                this.friends_ui.selected = !this.friends_ui.selected;
                                this.friends_ui.armed = false;
                                cx.notify();
                            }
                            "enter" | "space"
                                if !event.is_held && !event.keystroke.modifiers.modified() =>
                            {
                                this.friends_ui.armed = true
                            }
                            _ => return,
                        };
                        cx.stop_propagation();
                    }))
                    .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            let armed = std::mem::take(&mut this.friends_ui.armed);
                            if armed && !event.keystroke.modifiers.modified() {
                                if this.friends_ui.selected {
                                    this.accept_relationship_confirmation(window, cx)
                                } else {
                                    this.close_relationship_confirmation(window, cx)
                                }
                            }
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::BOLD)
                            .child(title),
                    )
                    .child(description)
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                quiet_button("relationship-cancel", "Cancel")
                                    .bg(rgb(if !self.friends_ui.selected {
                                        0x535064
                                    } else {
                                        0x383a40
                                    }))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_relationship_confirmation(window, cx)
                                    })),
                            )
                            .child(
                                quiet_button("relationship-confirm", title)
                                    .bg(rgb(if self.friends_ui.selected {
                                        0xe15059
                                    } else {
                                        0xc43b45
                                    }))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.accept_relationship_confirmation(window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
    pub(super) fn friends_view(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let filter = self.friends_ui.filter;
        let mut relationships: Vec<_> = self
            .state
            .relationships
            .values()
            .filter(|r| {
                relationship_visible(
                    filter,
                    r.kind,
                    self.state.presences.get(&r.id).map(|p| p.status),
                )
            })
            .cloned()
            .collect();
        relationships.sort_by_key(|r| {
            r.user
                .as_ref()
                .map(|u| u.display_name().to_lowercase())
                .unwrap_or_default()
        });
        let mut body = div()
            .id("friends-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .px_5();
        if relationships.is_empty() {
            body = body.child(
                div()
                    .py_8()
                    .text_color(rgb(MUTED))
                    .child("No people to show here."),
            );
        }
        for relationship in relationships {
            let Some(user) = relationship
                .user
                .clone()
                .or_else(|| self.state.users.get(&relationship.id).cloned())
            else {
                continue;
            };
            let id = user.id;
            let name = self.streaming().name(user.display_name()).into_owned();
            let status = self.friends_ui.status.get(&id).cloned();
            let pending = matches!(status, Some(RelationshipMutationStatus::Pending));
            let label = match relationship.kind {
                1 => self
                    .state
                    .presences
                    .get(&id)
                    .map(|p| match p.status {
                        PresenceStatus::Online => "Online",
                        PresenceStatus::Idle => "Idle",
                        PresenceStatus::Dnd => "Do Not Disturb",
                        _ => "Offline",
                    })
                    .unwrap_or("Offline"),
                2 => "Blocked",
                3 => "Incoming friend request",
                4 => "Outgoing friend request",
                _ => "",
            };
            let avatar = avatar_images::member_avatar(
                &self.state,
                id,
                None,
                &name,
                self.media_endpoint.as_deref(),
                false,
                36.,
            );
            let avatar_target = self.profile_trigger(
                div()
                    .id(SharedString::from(format!("friend-avatar-{id}")))
                    .size(px(36.))
                    .child(avatar),
                user.clone(),
                None,
                false,
                cx,
            );
            let name_art = emoji_sprites::unicode_text(&name, 14., window.scale_factor() > 1., cx);
            let name_target = self.profile_trigger(
                div()
                    .id(SharedString::from(format!("friend-name-{id}")))
                    .child(name_art),
                user,
                None,
                false,
                cx,
            );
            let profile = div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_3()
                .child(avatar_target)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .min_w_0()
                        .child(name_target)
                        .child(div().text_size(px(12.)).text_color(rgb(MUTED)).child(label)),
                );
            let mut actions = div().flex().items_center().gap_1();
            if relationship.kind == 1
                && !matches!(
                    self.friends_ui.dm_status.get(&id),
                    Some(DirectMessageStatus::Pending)
                )
            {
                actions = actions.child(
                    quiet_button(("friend-message", id.0), "Message").on_click(cx.listener(
                        move |this, _, _, cx| this.command(Command::OpenDm { user_id: id }, cx),
                    )),
                );
            }
            let already_member = self
                .friends_ui
                .group_target
                .and_then(|id| self.state.channels.get(&id))
                .is_some_and(|channel| channel.recipients.iter().any(|user| user.id == id));
            if relationship.kind == 1 && self.friends_ui.group_selecting && !already_member {
                let selected = self.friends_ui.group_recipients.contains(&id);
                actions = actions.child(
                    quiet_button(
                        ("group-select", id.0),
                        if selected { "Selected" } else { "Add to group" },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.friends_ui.group_recipients.remove(&id) {
                            this.friends_ui.group_recipients.insert(id);
                        }
                        cx.notify();
                    })),
                );
            }
            if relationship.kind == 3 && !pending && !self.demo {
                actions = actions.child(quiet_button(("friend-accept", id.0), "Accept").on_click(
                    cx.listener({
                        let name = name.clone();
                        move |this, _, window, cx| {
                            this.relationship_action(
                                id,
                                RelationshipAction::Accept,
                                name.clone(),
                                false,
                                window,
                                cx,
                            )
                        }
                    }),
                ));
            }
            let remove_label = match relationship.kind {
                1 => "Remove",
                2 => "Unblock",
                3 => "Decline",
                _ => "Cancel",
            };
            if !pending && !self.demo {
                actions = actions.child(
                    quiet_button(("friend-remove", id.0), remove_label).on_click(cx.listener({
                        let name = name.clone();
                        move |this, _, window, cx| {
                            this.relationship_action(
                                id,
                                RelationshipAction::Remove,
                                name.clone(),
                                relationship.kind == 1,
                                window,
                                cx,
                            )
                        }
                    })),
                );
                if relationship.kind == 1 || relationship.kind == 3 || relationship.kind == 4 {
                    actions =
                        actions.child(quiet_button(("friend-block", id.0), "Block").on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.relationship_action(
                                    id,
                                    RelationshipAction::Block,
                                    name.clone(),
                                    true,
                                    window,
                                    cx,
                                )
                            }),
                        ));
                }
            }
            let dm_detail = match self.friends_ui.dm_status.get(&id) {
                Some(DirectMessageStatus::Pending) => Some("Opening direct message…".to_owned()),
                Some(DirectMessageStatus::Failed(error)) => {
                    Some(format!("Couldn't open direct message: {error}"))
                }
                _ => None,
            };
            let detail = match status {
                Some(RelationshipMutationStatus::Pending) => Some("Updating…".to_owned()),
                Some(RelationshipMutationStatus::Failed(error)) => {
                    Some(format!("Couldn't update: {error}"))
                }
                _ => dm_detail,
            };
            body = body.child(
                div()
                    .border_b_1()
                    .border_color(rgb(0x3b3d45))
                    .py_3()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(profile)
                            .child(actions),
                    )
                    .when_some(detail, |v, detail| {
                        v.child(
                            div()
                                .pt_1()
                                .text_size(px(12.))
                                .text_color(rgb(0xf2b8b5))
                                .child(detail),
                        )
                    }),
            );
        }
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(60.))
                    .px_5()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(rgb(PANEL))
                    .child(div().font_weight(FontWeight::BOLD).child("Friends"))
                    .when(!self.demo, |row| {
                        row.child(
                            quiet_button(
                                "group-dm-toggle",
                                if self.friends_ui.group_selecting {
                                    "Cancel group"
                                } else {
                                    "New group DM"
                                },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(channel) = this.friends_ui.group_target.take() {
                                    this.friends_ui.group_selecting = false;
                                    this.friends_ui.group_recipients.clear();
                                    this.select(channel, cx);
                                    return;
                                }
                                this.friends_ui.group_selecting = !this.friends_ui.group_selecting;
                                this.friends_ui.group_recipients.clear();
                                if this.friends_ui.group_selecting {
                                    this.friends_ui.filter = Filter::All;
                                }
                                cx.notify();
                            })),
                        )
                    })
                    .when(self.friends_ui.group_selecting, |row| {
                        row.child(
                            quiet_button(
                                "group-dm-create",
                                format!(
                                    "{} ({})",
                                    if self.friends_ui.group_target.is_some() {
                                        "Add friends"
                                    } else {
                                        "Create group"
                                    },
                                    self.friends_ui.group_recipients.len()
                                ),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                let recipients =
                                    this.friends_ui.group_recipients.iter().copied().collect();
                                if let Some(channel_id) = this.friends_ui.group_target.take() {
                                    this.command(
                                        Command::AddGroupRecipients {
                                            channel_id,
                                            recipients,
                                        },
                                        cx,
                                    );
                                    this.select(channel_id, cx);
                                } else {
                                    this.command(Command::CreateGroupDm { recipients }, cx);
                                }
                                this.friends_ui.group_selecting = false;
                                this.friends_ui.group_recipients.clear();
                                cx.notify();
                            })),
                        )
                    })
                    .children(
                        [
                            (Filter::Online, "Online"),
                            (Filter::All, "All"),
                            (Filter::Pending, "Pending"),
                            (Filter::Blocked, "Blocked"),
                        ]
                        .into_iter()
                        .map(|(value, label)| {
                            quiet_button(
                                SharedString::from(format!("friends-filter-{label}")),
                                label,
                            )
                            .bg(rgb(if filter == value { 0x474851 } else { 0x2b2d31 }))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.friends_ui.filter = value;
                                    cx.notify();
                                },
                            ))
                        }),
                    ),
            )
            .child(body)
            .into_any_element()
    }
}

fn relationship_visible(filter: Filter, kind: u8, presence: Option<PresenceStatus>) -> bool {
    match filter {
        Filter::Online => {
            kind == 1
                && presence.is_some_and(|status| {
                    matches!(
                        status,
                        PresenceStatus::Online | PresenceStatus::Idle | PresenceStatus::Dnd
                    )
                })
        }
        Filter::All => kind == 1,
        Filter::Pending => kind == 3 || kind == 4,
        Filter::Blocked => kind == 2,
    }
}
#[cfg(test)]
mod tests {
    use super::{Filter, relationship_visible};
    use fluxer_core::PresenceStatus;
    #[test]
    fn relationship_filters_use_real_kind_and_presence() {
        assert!(relationship_visible(
            Filter::Online,
            1,
            Some(PresenceStatus::Online)
        ));
        assert!(!relationship_visible(Filter::Online, 1, None));
        assert!(!relationship_visible(
            Filter::Online,
            1,
            Some(PresenceStatus::Invisible)
        ));
        assert!(!relationship_visible(
            Filter::Online,
            2,
            Some(PresenceStatus::Online)
        ));
        assert!(relationship_visible(Filter::Pending, 3, None));
        assert!(relationship_visible(Filter::Pending, 4, None));
        assert!(relationship_visible(Filter::Blocked, 2, None));
        assert!(!relationship_visible(Filter::All, 2, None));
    }
}
