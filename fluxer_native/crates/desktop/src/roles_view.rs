// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

pub(super) struct RolesUi {
    pub guild: Option<Snowflake>,
    role: Option<Snowflake>,
    original: Option<fluxer_core::Role>,
    name: Entity<TextInput>,
    color: Entity<TextInput>,
    permissions: Entity<TextInput>,
    hoist: bool,
    mentionable: bool,
    pending: bool,
    confirm: bool,
    error: Option<String>,
    focus: FocusHandle,
    previous: Option<FocusHandle>,
}
impl RolesUi {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            guild: None,
            role: None,
            original: None,
            name: cx.new(|cx| TextInput::new("Role name", cx)),
            color: cx.new(|cx| TextInput::new("Color (hex)", cx)),
            permissions: cx.new(|cx| TextInput::new("Permissions", cx)),
            hoist: false,
            mentionable: false,
            pending: false,
            confirm: false,
            error: None,
            focus: cx.focus_handle(),
            previous: None,
        }
    }
}
impl Fluxer {
    pub(super) fn can_manage_roles(&self, guild: Snowflake) -> bool {
        !self.demo
            && self.state.current_user.as_ref().is_some_and(|u| {
                self.state.members.contains_key(&(guild, u.id))
                    && self
                        .state
                        .guild_permissions_for(guild, u.id)
                        .contains(fluxer_core::Permissions::MANAGE_ROLES)
            })
    }
    pub(super) fn open_roles(
        &mut self,
        guild: Snowflake,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_manage_roles(guild) {
            return;
        }
        self.roles_ui.guild = Some(guild);
        self.roles_ui.previous = window.focused(cx);
        self.roles_ui.focus.focus(window);
        self.select_role(None, cx);
    }
    fn select_role(&mut self, id: Option<Snowflake>, cx: &mut Context<Self>) {
        if self.roles_ui.pending {
            return;
        }
        let role = self
            .roles_ui
            .guild
            .and_then(|g| self.state.guilds.get(&g))
            .and_then(|g| id.and_then(|id| g.roles.iter().find(|r| r.id == id)))
            .cloned();
        self.roles_ui.original = role.clone();
        self.roles_ui.role = role.as_ref().map(|r| r.id);
        self.roles_ui.hoist = role.as_ref().is_some_and(|r| r.hoist);
        self.roles_ui.mentionable = role.as_ref().is_some_and(|r| r.mentionable);
        self.roles_ui.confirm = false;
        self.roles_ui.error = None;
        self.roles_ui.name.update(cx, |v, cx| {
            v.set(role.as_ref().map_or(String::new(), |r| r.name.clone()), cx)
        });
        self.roles_ui.color.update(cx, |v, cx| {
            v.set(format!("{:06x}", role.as_ref().map_or(0, |r| r.color)), cx)
        });
        self.roles_ui.permissions.update(cx, |v, cx| {
            v.set(role.as_ref().map_or(0, |r| r.permissions.0).to_string(), cx)
        });
        cx.notify();
    }
    fn close_roles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.roles_ui.pending {
            return;
        }
        self.roles_ui.guild = None;
        if let Some(f) = self.roles_ui.previous.take() {
            f.focus(window);
        }
        cx.notify();
    }
    pub(super) fn roles_key(
        &mut self,
        e: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if e.keystroke.key == "escape" {
            self.close_roles(window, cx);
            cx.stop_propagation();
        }
    }
    fn submit_role(&mut self, delete: bool, cx: &mut Context<Self>) {
        let Some(guild_id) = self.roles_ui.guild else {
            return;
        };
        if self.roles_ui.pending {
            return;
        }
        if let Some(original) = &self.roles_ui.original
            && self
                .state
                .guilds
                .get(&guild_id)
                .and_then(|guild| guild.roles.iter().find(|role| role.id == original.id))
                != Some(original)
        {
            self.roles_ui.error =
                Some("This role changed. Select it again to reload before saving.".into());
            cx.notify();
            return;
        }
        let color = u32::from_str_radix(
            self.roles_ui
                .color
                .read(cx)
                .content
                .trim()
                .trim_start_matches('#'),
            16,
        );
        let permissions = self
            .roles_ui
            .permissions
            .read(cx)
            .content
            .trim()
            .parse::<u64>();
        let (Ok(color), Ok(permissions)) = (color, permissions) else {
            self.roles_ui.error = Some("Enter a hex color and a decimal permission value".into());
            cx.notify();
            return;
        };
        self.roles_ui.pending = true;
        self.roles_ui.error = None;
        let command = Command::ManageRole {
            guild_id,
            role_id: self.roles_ui.role,
            original: self.roles_ui.original.clone(),
            name: self.roles_ui.name.read(cx).content.to_string(),
            color,
            permissions,
            hoist: self.roles_ui.hoist,
            mentionable: self.roles_ui.mentionable,
            delete,
        };
        let result = self
            .controller
            .as_ref()
            .ok_or_else(|| "The native client worker is unavailable".to_owned())
            .and_then(|controller| controller.send(command));
        if let Err(error) = result {
            self.roles_ui.pending = false;
            self.roles_ui.error = Some(error);
        }
        cx.notify();
    }
    pub(super) fn role_result(
        &mut self,
        guild: Snowflake,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.roles_ui.guild != Some(guild) || !self.roles_ui.pending {
            return;
        }
        self.roles_ui.pending = false;
        self.roles_ui.error = error;
        self.roles_ui.confirm = false;
        cx.notify();
    }
    pub(super) fn roles_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(guild) = self.roles_ui.guild else {
            return div().into_any_element();
        };
        let mut roles = self
            .state
            .guilds
            .get(&guild)
            .map(|g| g.roles.clone())
            .unwrap_or_default();
        roles.sort_by(|a, b| b.position.cmp(&a.position).then(a.id.cmp(&b.id)));
        let mut list = div().flex().flex_col().gap_1();
        for role in roles {
            let id = role.id;
            list = list.child(
                controls::quiet_button(SharedString::from(format!("role-{id}")), role.name)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_role(Some(id), cx))),
            );
        }
        let pending = self.roles_ui.pending;
        let selected = self.roles_ui.role;
        let bits = self
            .roles_ui
            .permissions
            .read(cx)
            .content
            .trim()
            .parse::<u64>()
            .unwrap_or(0);
        let mut permission_list = div()
            .id("role-permissions")
            .max_h(px(150.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for (label, bit) in [
            ("Administrator", 3),
            ("Manage community", 5),
            ("Manage roles", 28),
            ("Manage channels", 4),
            ("Kick members", 1),
            ("Ban members", 2),
            ("Moderate members", 40),
            ("Manage nicknames", 27),
            ("Manage expressions", 30),
            ("Manage webhooks", 29),
            ("View audit log", 7),
            ("Create invites", 0),
            ("View channels", 10),
            ("Send messages", 11),
            ("Manage messages", 13),
            ("Read message history", 16),
            ("Mention everyone", 17),
            ("Attach files", 15),
            ("Embed links", 14),
            ("Add reactions", 6),
            ("Connect", 20),
            ("Speak", 21),
            ("Mute members", 22),
            ("Deafen members", 23),
            ("Move members", 24),
            ("Manage threads", 34),
            ("Create public threads", 35),
            ("Create private threads", 36),
            ("Send messages in threads", 38),
            ("Pin messages", 51),
            ("Bypass slowmode", 52),
        ] {
            let mask = 1u64 << bit;
            permission_list = permission_list.child(
                controls::quiet_button(
                    SharedString::from(format!("permission-{bit}")),
                    format!("{} {label}", if bits & mask != 0 { "☑" } else { "☐" }),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let current = this
                        .roles_ui
                        .permissions
                        .read(cx)
                        .content
                        .trim()
                        .parse::<u64>()
                        .unwrap_or(0);
                    this.roles_ui
                        .permissions
                        .update(cx, |input, cx| input.set((current ^ mask).to_string(), cx));
                    cx.notify();
                })),
            );
        }
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
                    .id("roles-dialog")
                    .track_focus(&self.roles_ui.focus)
                    .w(px(700.))
                    .max_h(px(650.))
                    .p_5()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::BOLD)
                            .child("Community Roles"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .child(
                                div()
                                    .id("role-list")
                                    .w(px(200.))
                                    .max_h(px(450.))
                                    .overflow_y_scroll()
                                    .child(
                                        controls::quiet_button("new-role", "＋ New role").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.select_role(None, cx)
                                            }),
                                        ),
                                    )
                                    .child(list),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(if selected.is_some() {
                                        "Edit role"
                                    } else {
                                        "Create role"
                                    })
                                    .child("Name")
                                    .child(self.roles_ui.name.clone())
                                    .child("Color (hex)")
                                    .child(self.roles_ui.color.clone())
                                    .child("Permissions (decimal bitset)")
                                    .child(self.roles_ui.permissions.clone())
                                    .child(permission_list)
                                    .when(selected.is_some(), |v| {
                                        v.child(
                                            controls::quiet_button(
                                                "role-hoist",
                                                if self.roles_ui.hoist {
                                                    "☑ Display separately"
                                                } else {
                                                    "☐ Display separately"
                                                },
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.roles_ui.hoist = !this.roles_ui.hoist;
                                                    cx.notify();
                                                }),
                                            ),
                                        )
                                    })
                                    .when(selected.is_some(), |v| {
                                        v.child(
                                            controls::quiet_button(
                                                "role-mentionable",
                                                if self.roles_ui.mentionable {
                                                    "☑ Allow mentions"
                                                } else {
                                                    "☐ Allow mentions"
                                                },
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.roles_ui.mentionable =
                                                        !this.roles_ui.mentionable;
                                                    cx.notify();
                                                }),
                                            ),
                                        )
                                    })
                                    .when_some(self.roles_ui.error.clone(), |v, e| {
                                        v.child(div().text_color(rgb(0xe15059)).child(e))
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .when(selected.is_some_and(|id| id != guild), |v| {
                                v.child(
                                    controls::quiet_button(
                                        "delete-role",
                                        if self.roles_ui.confirm {
                                            "Confirm delete"
                                        } else {
                                            "Delete role"
                                        },
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            if this.roles_ui.confirm {
                                                this.submit_role(true, cx);
                                            } else {
                                                this.roles_ui.confirm = true;
                                                cx.notify();
                                            }
                                        },
                                    )),
                                )
                            })
                            .child(controls::quiet_button("close-roles", "Close").on_click(
                                cx.listener(|this, _, window, cx| this.close_roles(window, cx)),
                            ))
                            .child(
                                controls::quiet_button(
                                    "save-role",
                                    if pending {
                                        "Please wait…"
                                    } else if selected.is_some() {
                                        "Save"
                                    } else {
                                        "Create"
                                    },
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_role(false, cx)),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }
}
