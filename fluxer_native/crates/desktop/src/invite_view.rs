// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use fluxer_client::InviteStatus;
use fluxer_protocol::InvitePreview;

#[derive(Default)]
enum InviteStep {
    #[default]
    Input,
    Loading,
    Preview(Box<InvitePreview>),
    Joining,
    Opening,
    Created(String),
    CreateLoading,
    ManageLoading(Snowflake),
    ManageError(Snowflake, String),
    ManageList(Snowflake, Vec<fluxer_protocol::CommunityInvite>),
    ConfirmRevoke(Snowflake, Vec<fluxer_protocol::CommunityInvite>, String),
    Revoking(Snowflake, Vec<fluxer_protocol::CommunityInvite>),
    CreateError(String),
    Error(String),
}

pub(super) struct InviteView {
    input: Entity<TextInput>,
    request_id: u64,
    pub open: bool,
    previous: Option<FocusHandle>,
    step: InviteStep,
    restore_focus: bool,
    dialog_focus: FocusHandle,
    focus_step: bool,
    selected: usize,
}

impl InviteView {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        let input = cx.new(|cx| TextInput::new("Invite link or code", cx));
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        Self {
            input,
            request_id: 0,
            open: false,
            previous: None,
            step: InviteStep::Input,
            restore_focus: false,
            dialog_focus: cx.focus_handle(),
            focus_step: false,
            selected: 0,
        }
    }

    pub fn set_hide_invites(&mut self, hide: bool, cx: &mut Context<Fluxer>) {
        self.input.update(cx, |input, cx| {
            if hide != input.secret {
                input.secret = hide;
                input.reset_context_menu(cx);
                cx.notify();
            }
        });
    }
    pub fn reset(&mut self, cx: &mut Context<Fluxer>) {
        self.open = false;
        self.step = InviteStep::Input;
        self.previous = None;
        self.restore_focus = false;
        self.focus_step = false;
        self.input
            .update(cx, |input, cx| input.set(String::new(), cx));
    }

    fn joining(&self) -> bool {
        matches!(
            self.step,
            InviteStep::Joining | InviteStep::Opening | InviteStep::Revoking(..)
        )
    }
}

impl Fluxer {
    pub(super) fn can_open_linked_invite(&self) -> bool {
        !self.invite_ui.open
            && self.community_ui.dialog.is_none()
            && self.context.kick_confirm.is_none()
            && self.context.channel_delete_confirm.is_none()
            && !self.status_ui.is_open()
            && !self.quick_switcher.open
            && !self.inbox_ui.open
            && !self.message_actions.is_blocking()
            && !self.message_links.is_open()
            && !self.image_viewer.is_open()
            && !self.playback.modal.is_open()
    }

    pub(super) fn open_linked_invite(&mut self, code: &str, cx: &mut Context<Self>) {
        self.selected_guild = None;
        self.pane = Pane::Friends;
        self.invite_ui.reset(cx);
        self.invite_ui.previous = Some(self.focus.clone());
        self.invite_ui.open = true;
        self.invite_ui
            .input
            .update(cx, |input, cx| input.set(code.to_owned(), cx));
        self.invite_action(cx);
    }

    pub(super) fn create_community_invite(
        &mut self,
        channel_id: Snowflake,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invite_ui.reset(cx);
        self.invite_ui.previous = window.focused(cx);
        self.invite_ui.open = true;
        self.invite_ui.request_id = self.invite_ui.request_id.wrapping_add(1);
        self.invite_ui.step = InviteStep::CreateLoading;
        self.invite_ui.dialog_focus.focus(window);
        if let Err(message) = self
            .controller
            .as_ref()
            .ok_or("The native client worker is unavailable".to_owned())
            .and_then(|controller| {
                controller.send(Command::CreateInvite {
                    request_id: self.invite_ui.request_id,
                    channel_id,
                })
            })
        {
            self.invite_ui.step = InviteStep::CreateError(message);
        }
        cx.notify();
    }
    pub(super) fn manage_community_invites(
        &mut self,
        guild_id: Snowflake,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invite_ui.reset(cx);
        self.invite_ui.previous = window.focused(cx);
        self.invite_ui.open = true;
        self.invite_ui.dialog_focus.focus(window);
        self.load_community_invites(guild_id, cx);
    }

    fn load_community_invites(&mut self, guild_id: Snowflake, cx: &mut Context<Self>) {
        self.invite_ui.request_id = self.invite_ui.request_id.wrapping_add(1);
        self.invite_ui.selected = 0;
        self.invite_ui.step = InviteStep::ManageLoading(guild_id);
        if let Err(message) = self
            .controller
            .as_ref()
            .ok_or("The native client worker is unavailable".to_owned())
            .and_then(|controller| {
                controller.send(Command::ListCommunityInvites {
                    request_id: self.invite_ui.request_id,
                    guild_id,
                })
            })
        {
            self.invite_ui.step = InviteStep::ManageError(guild_id, message);
        }
        cx.notify();
    }

    fn revoke_community_invite(&mut self, cx: &mut Context<Self>) {
        let InviteStep::ConfirmRevoke(guild, invites, code) = &self.invite_ui.step else {
            return;
        };
        let (guild, invites, code) = (*guild, invites.clone(), code.clone());
        self.invite_ui.request_id = self.invite_ui.request_id.wrapping_add(1);
        self.invite_ui.step = InviteStep::Revoking(guild, invites);
        if let Err(message) = self
            .controller
            .as_ref()
            .ok_or("The native client worker is unavailable".to_owned())
            .and_then(|controller| {
                controller.send(Command::RevokeCommunityInvite {
                    request_id: self.invite_ui.request_id,
                    guild_id: guild,
                    code,
                })
            })
        {
            self.invite_ui.step = InviteStep::ManageError(guild, message);
        }
        cx.notify();
    }

    pub(super) fn invite_button(&self, cx: &mut Context<Self>) -> AnyElement {
        button("join-by-invite", "Join")
            .size(px(44.))
            .p_0()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(12.))
            .rounded_full()
            .on_click(cx.listener(|this, _, window, cx| {
                if this.status_ui.is_open()
                    || this.quick_switcher.open
                    || this.inbox_ui.open
                    || this.message_actions.is_blocking()
                {
                    return;
                }
                this.invite_ui.reset(cx);
                this.invite_ui.previous = window.focused(cx);
                this.invite_ui.open = true;
                this.invite_ui.input.focus_handle(cx).focus(window);
                cx.notify();
            }))
            .into_any_element()
    }

    fn close_invite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.invite_ui.joining() {
            return;
        }
        self.command(
            Command::CancelInvite {
                request_id: self.invite_ui.request_id,
            },
            cx,
        );
        if let Some(focus) = self.invite_ui.previous.take() {
            focus.focus(window);
        } else {
            self.composer.focus_handle(cx).focus(window);
        }
        self.invite_ui.reset(cx);
        cx.notify();
    }

    fn invite_action(&mut self, cx: &mut Context<Self>) {
        let changes = match self.invite_ui.step {
            InviteStep::Input | InviteStep::Error(_) => {
                let input = self.invite_ui.input.read(cx).content.to_string();
                if input.trim().is_empty() {
                    return;
                }
                self.invite_ui.request_id = self.invite_ui.request_id.wrapping_add(1);
                (
                    Command::PreviewInvite {
                        request_id: self.invite_ui.request_id,
                        input,
                    },
                    InviteStep::Loading,
                )
            }
            InviteStep::Preview(_) => (
                Command::AcceptInvite {
                    request_id: self.invite_ui.request_id,
                },
                InviteStep::Joining,
            ),
            _ => return,
        };
        self.invite_ui.focus_step = true;
        self.invite_ui.step = match self
            .controller
            .as_ref()
            .ok_or("The native client worker is unavailable".to_owned())
            .and_then(|controller| controller.send(changes.0))
        {
            Ok(()) => changes.1,
            Err(message) => InviteStep::Error(message),
        };
        cx.notify();
    }

    pub(super) fn invite_status(
        &mut self,
        request_id: u64,
        status: InviteStatus,
        cx: &mut Context<Self>,
    ) {
        if !self.invite_ui.open || self.invite_ui.request_id != request_id {
            return;
        }
        self.invite_ui.focus_step = true;
        let managing = match &self.invite_ui.step {
            InviteStep::ManageLoading(guild) | InviteStep::Revoking(guild, _) => Some(*guild),
            _ => None,
        };
        let creating = matches!(self.invite_ui.step, InviteStep::CreateLoading);
        if matches!(&status, InviteStatus::Revoked(_)) {
            self.invite_ui.selected = 0;
        }
        self.invite_ui.step = match status {
            InviteStatus::Listed(invites) => {
                InviteStep::ManageList(managing.unwrap_or_default(), invites)
            }
            InviteStatus::Revoked(code) => match &self.invite_ui.step {
                InviteStep::Revoking(guild, invites) => InviteStep::ManageList(
                    *guild,
                    invites
                        .iter()
                        .filter(|invite| invite.code != code)
                        .cloned()
                        .collect(),
                ),
                _ => return,
            },
            InviteStatus::Failed(message) if managing.is_some() => {
                InviteStep::ManageError(managing.unwrap(), message)
            }
            InviteStatus::Created(url) => InviteStep::Created(url),
            InviteStatus::Preview(preview) => InviteStep::Preview(preview),
            InviteStatus::Opening => InviteStep::Opening,
            InviteStatus::Failed(message) if creating => InviteStep::CreateError(message),
            InviteStatus::Failed(message) => InviteStep::Error(message),
            InviteStatus::Joined => {
                self.status = "Invite accepted".into();
                self.finish_invite(cx)
            }
            InviteStatus::Opened { guild_id } => {
                self.selected_guild = guild_id;
                self.pane = Pane::Chat;
                if let Some(channel) = self.state.selected_channel {
                    self.quick_switcher.visit(channel);
                    self.reset_ack_context(channel);
                    self.search_ui.return_to_latest_on_select(channel);
                }
                self.finish_invite(cx)
            }
        };
        cx.notify();
    }

    fn finish_invite(&mut self, cx: &mut Context<Self>) -> InviteStep {
        let previous = self.invite_ui.previous.take();
        self.invite_ui.reset(cx);
        self.invite_ui.previous = previous;
        self.invite_ui.restore_focus = true;
        InviteStep::Input
    }

    pub(super) fn invite_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" if matches!(self.invite_ui.step, InviteStep::ConfirmRevoke(..)) => {
                if let InviteStep::ConfirmRevoke(guild, invites, _) = &self.invite_ui.step {
                    self.invite_ui.step = InviteStep::ManageList(*guild, invites.clone());
                    cx.notify();
                }
            }
            "escape" => self.close_invite(window, cx),
            "up" | "down" if matches!(self.invite_ui.step, InviteStep::ManageList(..)) => {
                if let InviteStep::ManageList(_, invites) = &self.invite_ui.step
                    && !invites.is_empty()
                {
                    self.invite_ui.selected = if event.keystroke.key == "up" {
                        (self.invite_ui.selected + invites.len() - 1) % invites.len()
                    } else {
                        (self.invite_ui.selected + 1) % invites.len()
                    };
                    cx.notify();
                }
            }
            "enter"
                if !event.is_held && matches!(self.invite_ui.step, InviteStep::ManageList(..)) =>
            {
                if let InviteStep::ManageList(guild, invites) = &self.invite_ui.step
                    && let Some(invite) = invites.get(self.invite_ui.selected)
                {
                    self.invite_ui.step =
                        InviteStep::ConfirmRevoke(*guild, invites.clone(), invite.code.clone());
                    cx.notify();
                }
            }
            "enter"
                if !event.is_held
                    && matches!(self.invite_ui.step, InviteStep::ConfirmRevoke(..)) =>
            {
                self.revoke_community_invite(cx)
            }
            "enter"
                if !event.is_held && matches!(self.invite_ui.step, InviteStep::ManageError(..)) =>
            {
                if let InviteStep::ManageError(guild, _) = self.invite_ui.step {
                    self.load_community_invites(guild, cx);
                }
            }
            "tab" => {
                if matches!(
                    self.invite_ui.step,
                    InviteStep::Input | InviteStep::Loading | InviteStep::Error(_)
                ) {
                    self.invite_ui.input.focus_handle(cx).focus(window);
                } else {
                    self.invite_ui.dialog_focus.focus(window);
                }
            }
            "enter" if !event.is_held && !self.invite_ui.input.read(cx).is_composing() => {
                self.invite_action(cx)
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(super) fn invite_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if std::mem::take(&mut self.invite_ui.restore_focus) {
            if self.state.selected_channel.is_some() {
                self.composer.focus_handle(cx).focus(window);
            } else if let Some(previous) = self.invite_ui.previous.take() {
                previous.focus(window);
            }
        }
        if !self.invite_ui.open {
            return div().into_any_element();
        }
        if std::mem::take(&mut self.invite_ui.focus_step) {
            if matches!(
                self.invite_ui.step,
                InviteStep::Input | InviteStep::Loading | InviteStep::Error(_)
            ) {
                self.invite_ui.input.focus_handle(cx).focus(window);
            } else {
                self.invite_ui.dialog_focus.focus(window);
            }
        }
        let streaming = self.streaming();
        let step = &self.invite_ui.step;
        let width = px(440.);
        let mut card = div()
            .id("invite-dialog")
            .track_focus(&self.invite_ui.dialog_focus)
            .w(width)
            .max_w(relative(0.94))
            .p_5()
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(0x4c435e))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(heading(
                        if matches!(
                            step,
                            InviteStep::ManageLoading(_)
                                | InviteStep::ManageError(..)
                                | InviteStep::ManageList(..)
                                | InviteStep::ConfirmRevoke(..)
                                | InviteStep::Revoking(..)
                        ) {
                            "Invites"
                        } else if matches!(
                            step,
                            InviteStep::CreateLoading
                                | InviteStep::Created(_)
                                | InviteStep::CreateError(_)
                        ) {
                            "Invite people"
                        } else {
                            "Join by invite"
                        },
                    ))
                    .when(!self.invite_ui.joining(), |card| {
                        card.child(
                            controls::icon_button("close-invite", controls::Icon::Close, "Close")
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.close_invite(window, cx)
                                    }),
                                ),
                        )
                    }),
            );

        match step {
            InviteStep::ManageLoading(_) => {
                card = card.child("Loading invites…");
            }
            InviteStep::ManageError(guild, message) => {
                let guild = *guild;
                card = card.child(message.clone()).child(
                    button("retry-community-invites", "Try again").on_click(
                        cx.listener(move |this, _, _, cx| this.load_community_invites(guild, cx)),
                    ),
                );
            }
            InviteStep::ManageList(guild, invites) => {
                let guild = *guild;
                let invites = invites.clone();
                card = card.child("View all invites for this community. To create an invite, use a channel’s invite button. Use the arrow keys to select an invite and Enter to revoke it.");
                if invites.is_empty() {
                    card = card.child("No active invites.");
                }
                let mut list_view = div()
                    .id("community-invites-list")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2();
                for (index, invite) in invites.iter().enumerate() {
                    let code = invite.code.clone();
                    let list = invites.clone();
                    let details = format!(
                        "{} · #{} · by {} · {} uses · expires {}",
                        if streaming.hide_invites {
                            "Invite code hidden"
                        } else {
                            &code
                        },
                        invite.channel.name.as_deref().unwrap_or("unknown"),
                        invite
                            .inviter
                            .as_ref()
                            .map(|user| streaming.name(&user.username))
                            .unwrap_or(std::borrow::Cow::Borrowed("unknown")),
                        invite.uses,
                        invite.expires_at.as_deref().unwrap_or("never")
                    );
                    list_view = list_view.child(
                        div()
                            .flex()
                            .justify_between()
                            .gap_2()
                            .child(if index == self.invite_ui.selected {
                                format!("› {details}")
                            } else {
                                details
                            })
                            .child(
                                button(
                                    SharedString::from(format!("revoke-invite-{index}")),
                                    "Revoke",
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.invite_ui.step = InviteStep::ConfirmRevoke(
                                            guild,
                                            list.clone(),
                                            code.clone(),
                                        );
                                        cx.notify();
                                    },
                                )),
                            ),
                    );
                }
                card = card.child(list_view);
            }
            InviteStep::ConfirmRevoke(guild, invites, code) => {
                let (guild, invites) = (*guild, invites.clone());
                card = card
                    .child(if streaming.hide_invites {
                        "Revoke this invite? People will no longer be able to use it.".to_owned()
                    } else {
                        format!("Revoke invite {code}? People will no longer be able to use it.")
                    })
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                button("cancel-revoke-invite", "Cancel").on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.invite_ui.step =
                                            InviteStep::ManageList(guild, invites.clone());
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(button("confirm-revoke-invite", "Revoke invite").on_click(
                                cx.listener(|this, _, _, cx| this.revoke_community_invite(cx)),
                            )),
                    );
            }
            InviteStep::Revoking(..) => {
                card = card.child("Revoking invite…");
            }
            InviteStep::CreateLoading => {
                card = card.child("Creating invite…");
            }
            InviteStep::CreateError(message) => {
                card = card.child(message.clone());
            }
            InviteStep::Created(url) => {
                let url = url.clone();
                card = card
                    .child("This invite expires in 7 days and has unlimited uses.")
                    .child(div().child(if streaming.hide_invites {
                        streamer_mode::HIDDEN_INVITE.to_owned()
                    } else {
                        url.clone()
                    }))
                    .when(!streaming.hide_invites, |card| {
                        card.child(button("copy-created-invite", "Copy invite").on_click(
                            cx.listener(move |this, _, _, cx| {
                                if !this.streaming().hide_invites {
                                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                                    this.status = "Invite copied".into();
                                    cx.notify();
                                }
                            }),
                        ))
                    });
            }
            InviteStep::Input | InviteStep::Loading | InviteStep::Error(_) => {
                let loading = matches!(step, InviteStep::Loading);
                card = card.child(self.invite_ui.input.clone());
                if let InviteStep::Error(message) = step {
                    card = card.child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(0xffa9b5))
                            .child(message.clone()),
                    );
                }
                card = card.child(
                    div().flex().justify_end().child(
                        button(
                            "preview-invite",
                            if loading {
                                "Loading…"
                            } else {
                                "Preview invite"
                            },
                        )
                        .when(
                            !loading && !self.invite_ui.input.read(cx).content.trim().is_empty(),
                            |button| {
                                button
                                    .bg(rgb(ACCENT))
                                    .on_click(cx.listener(|this, _, _, cx| this.invite_action(cx)))
                            },
                        ),
                    ),
                );
            }
            InviteStep::Preview(preview) => {
                let (title, detail, action) = match &preview.guild {
                    Some(guild) => (
                        guild.name.clone(),
                        format!(
                            "{} members · {} online",
                            preview.member_count,
                            preview.presence_count.unwrap_or_default()
                        ),
                        if self.state.members.contains_key(&(
                            guild.id,
                            self.state
                                .current_user
                                .as_ref()
                                .map(|user| user.id)
                                .unwrap_or_default(),
                        )) {
                            "Open community"
                        } else {
                            "Join community"
                        },
                    ),
                    None => (
                        preview
                            .channel
                            .name
                            .clone()
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| {
                                let names = preview
                                    .channel
                                    .recipients
                                    .iter()
                                    .map(|recipient| {
                                        streaming.name(&recipient.username).into_owned()
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                if names.is_empty() {
                                    "Group DM".into()
                                } else {
                                    names
                                }
                            }),
                        format!("{} members", preview.member_count),
                        "Join group DM",
                    ),
                };
                card = card
                    .child(
                        div()
                            .p_3()
                            .rounded_md()
                            .bg(rgb(RAIL))
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(muted(detail))
                            .when_some(
                                preview
                                    .channel
                                    .name
                                    .clone()
                                    .filter(|_| preview.guild.is_some()),
                                |view, name| view.child(muted(format!("#{name}"))),
                            )
                            .when(preview.temporary, |view| {
                                view.child(muted("Temporary membership"))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .gap_2()
                            .child(button("edit-invite", "Back").on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.invite_ui.step = InviteStep::Input;
                                    this.invite_ui.input.focus_handle(cx).focus(window);
                                    cx.notify();
                                },
                            )))
                            .child(
                                button("accept-invite", action)
                                    .bg(rgb(ACCENT))
                                    .on_click(cx.listener(|this, _, _, cx| this.invite_action(cx))),
                            ),
                    );
            }
            InviteStep::Joining => card = card.child(muted("Accepting invite…")),
            InviteStep::Opening => card = card.child(muted("Waiting for the destination to sync…")),
        }
        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(rgba(0x00000099))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_invite(window, cx)),
            )
            .child(card)
            .into_any_element()
    }
}
