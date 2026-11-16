// SPDX-License-Identifier: AGPL-3.0-or-later
//! Compact native role information. The member cache is a partial gateway view.
use super::*;
use fluxer_core::Role;

pub(super) struct RoleMention {
    role: Option<Role>,
    known: usize,
    guild: Option<Snowflake>,
    anchor: Bounds<Pixels>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
}
impl RoleMention {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            role: None,
            known: 0,
            guild: None,
            anchor: Bounds::default(),
            focus: cx.focus_handle(),
            previous_focus: None,
        }
    }
    pub fn is_open(&self) -> bool {
        self.role.is_some()
    }
    fn open(
        &mut self,
        role: Role,
        guild: Snowflake,
        known: usize,
        anchor: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.role.is_none() {
            self.previous_focus = window.focused(cx);
        }
        self.role = Some(role);
        self.guild = Some(guild);
        self.known = known;
        self.anchor = anchor;
        self.focus.focus(window);
        cx.notify();
    }
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.role = None;
        self.guild = None;
        self.previous_focus = None;
        cx.notify();
    }
    pub fn reconcile(&mut self, state: &ClientState, window: &mut Window, cx: &mut Context<Self>) {
        let Some((guild, current)) = self.guild.zip(self.role.as_ref()) else {
            return;
        };
        let role = state
            .guilds
            .get(&guild)
            .and_then(|guild| guild.roles.iter().find(|role| role.id == current.id));
        let Some(role) = role else {
            self.dismiss(window, cx);
            return;
        };
        let known = state
            .members
            .iter()
            .filter(|((g, _), member)| *g == guild && member.roles.contains(&role.id))
            .count();
        if self.role.as_ref() != Some(role) || self.known != known {
            self.role = Some(role.clone());
            self.known = known;
            cx.notify();
        }
    }
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.role = None;
        self.guild = None;
        if let Some(focus) = self.previous_focus.take() {
            focus.focus(window);
        }
        cx.notify();
    }
}
impl Render for RoleMention {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(role) = self.role.as_ref() else {
            return div().into_any_element();
        };
        let viewport = window.viewport_size();
        let fitted = crate::overlay::popover_bounds(
            self.anchor,
            viewport,
            size(px(260.), px(150.)),
            px(8.),
            px(12.),
        );
        let origin = fitted.origin;
        let width = f32::from(fitted.size.width);
        let height = f32::from(fitted.size.height);
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(origin)
                .anchor(Corner::TopLeft)
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .id("role-mention-popup")
                        .track_focus(&self.focus)
                        .occlude()
                        .w(px(width))
                        .max_h(px(height))
                        .overflow_y_scroll()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(0x4c4458))
                        .bg(rgb(0x232428))
                        .text_color(rgb(0xf2f3f5))
                        .shadow_lg()
                        .p_3()
                        .on_mouse_down_out(
                            cx.listener(|this, _, window, cx| this.dismiss(window, cx)),
                        )
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            match event.keystroke.key.as_str() {
                                "escape" => this.dismiss(window, cx),
                                "tab" => this.focus.focus(window),
                                _ => return,
                            }
                            cx.stop_propagation();
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().size(px(10.)).rounded_full().bg(rgb(
                                    if role.color == 0 {
                                        0x99aab5
                                    } else {
                                        role.color & 0xffffff
                                    },
                                )))
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(role.name.clone()),
                                ),
                        )
                        .when(role.mentionable, |view| {
                            view.child(
                                div()
                                    .pt_2()
                                    .text_size(px(12.))
                                    .text_color(rgb(0xb5bac1))
                                    .child("Anyone can mention this role"),
                            )
                        })
                        .child(
                            div()
                                .pt_2()
                                .text_size(px(12.))
                                .text_color(rgb(0xb5bac1))
                                .child(if self.known == 0 {
                                    "Member count unavailable".to_owned()
                                } else {
                                    format!(
                                        "At least {} {}",
                                        self.known,
                                        if self.known == 1 { "member" } else { "members" }
                                    )
                                }),
                        ),
                ),
        )
        .with_priority(9)
        .into_any_element()
    }
}
impl Fluxer {
    pub(super) fn open_role_mention(
        &mut self,
        id: Snowflake,
        channel: Snowflake,
        anchor: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(guild) = self
            .state
            .channels
            .get(&channel)
            .and_then(|channel| channel.guild_id)
        else {
            return;
        };
        let Some(role) = self
            .state
            .guilds
            .get(&guild)
            .and_then(|guild| guild.roles.iter().find(|role| role.id == id))
            .cloned()
        else {
            return;
        };
        let known = self
            .state
            .members
            .iter()
            .filter(|((g, _), member)| *g == guild && member.roles.contains(&id))
            .count();
        self.dismiss_user_profile(window, cx);
        self.role_mention.update(cx, |popup, cx| {
            popup.open(
                role,
                guild,
                known,
                anchor
                    .unwrap_or_else(|| Bounds::new(window.mouse_position(), size(px(0.), px(0.)))),
                window,
                cx,
            )
        });
    }
}
