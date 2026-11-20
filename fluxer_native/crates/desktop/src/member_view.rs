// SPDX-License-Identifier: AGPL-3.0-or-later
//! Virtualized server-provided lazy member rows.
use super::*;
use fluxer_core::{Member, PresenceStatus};

impl Fluxer {
    pub(super) fn member_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(channel) = self.state.selected_channel else {
            return div().into_any_element();
        };
        let Some(guild) = self.state.channels.get(&channel).and_then(|c| c.guild_id) else {
            return div().into_any_element();
        };
        let permitted = self.state.current_user.as_ref().is_some_and(|user| {
            self.state.permissions_for(channel, user.id).contains(
                fluxer_core::Permissions::VIEW_CHANNEL
                    | fluxer_core::Permissions::VIEW_CHANNEL_MEMBERS,
            )
        });
        if !permitted {
            return div()
                .w(px(220.))
                .px_3()
                .py_4()
                .child("You cannot view members in this channel.")
                .into_any_element();
        }
        let identity = self
            .state
            .channels
            .get(&channel)
            .zip(self.state.guilds.get(&guild))
            .map(|(channel, guild)| fluxer_core::member_list_identity(guild, channel));
        if self.members_identity != identity {
            self.members_identity = identity.clone();
            self.members_request_generation = self.members_request_generation.wrapping_add(1);
            self.members_start = 0;
            self.members_scroll = UniformListScrollHandle::new();
            self.command(
                Command::SetMemberList {
                    channel_id: Some(channel),
                    start: 0,
                },
                cx,
            );
        }
        let list = self
            .state
            .member_lists
            .get(&(guild, channel))
            .filter(|list| Some(list.identity.as_str()) == identity.as_deref());
        let total = list
            .map(|list| {
                let grouped: u64 = list
                    .groups
                    .iter()
                    .filter_map(|group| group.get("count").and_then(serde_json::Value::as_u64))
                    .filter(|count| *count > 0)
                    .map(|count| count + 1)
                    .sum();
                grouped
                    .max(
                        list.rows
                            .keys()
                            .next_back()
                            .map_or(0, |last| u64::from(*last) + 1),
                    )
                    .min(100_001) as usize
            })
            .unwrap_or(0);
        let label = list
            .map(|l| format!("{} online", l.online_count))
            .unwrap_or_else(|| "Loading members…".into());
        let handle = self.members_scroll.clone();
        let scrollbar = handle.0.borrow().base_handle.clone();
        div()
            .id("member-panel")
            .w(px(220.))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(0x2b2d31))
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(12.))
                    .text_color(rgb(0xb5bac1))
                    .child(label)
                    .child(
                        crate::controls::quiet_button("close-members", "×").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.members_open = false;
                                this.command(
                                    Command::SetMemberList {
                                        channel_id: None,
                                        start: 0,
                                    },
                                    cx,
                                );
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list(
                            "member-list",
                            total,
                            cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                                // GPUI also asks for a single measurement row; only viewport ranges drive subscriptions.
                                if let Some(start) =
                                    requested_window(this.members_start, range.clone())
                                {
                                    this.members_start = start;
                                    this.members_request_generation =
                                        this.members_request_generation.wrapping_add(1);
                                    let generation = this.members_request_generation;
                                    cx.spawn(async move |entity, cx| {
                                        Timer::after(Duration::from_millis(100)).await;
                                        let _ = entity.update(cx, |this, cx| {
                                            if this.members_open
                                                && this.state.selected_channel == Some(channel)
                                                && this.members_request_generation == generation
                                            {
                                                this.command(
                                                    Command::SetMemberList {
                                                        channel_id: Some(channel),
                                                        start,
                                                    },
                                                    cx,
                                                );
                                            }
                                        });
                                    })
                                    .detach();
                                }
                                range
                                    .map(|index| this.member_row(guild, channel, index, window, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(handle)
                        .h_full()
                        .flex_1()
                        .min_h_0()
                        .w_full(),
                    )
                    .child(div().absolute().right_0().top_0().bottom_0().child(
                        crate::scrollbar::Scrollbar::new("member-scrollbar", scrollbar),
                    )),
            )
            .into_any_element()
    }
    fn member_row(
        &mut self,
        guild: Snowflake,
        channel: Snowflake,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let identity = self
            .state
            .channels
            .get(&channel)
            .zip(self.state.guilds.get(&guild))
            .map(|(channel, guild)| fluxer_core::member_list_identity(guild, channel));
        let item = self
            .state
            .member_lists
            .get(&(guild, channel))
            .filter(|list| Some(list.identity.as_str()) == identity.as_deref())
            .and_then(|list| list.rows.get(&(index as u32)))
            .cloned();
        if let Some(group) = item.as_ref().and_then(|item| item.get("group")) {
            let id = group
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let name = self
                .state
                .guilds
                .get(&guild)
                .and_then(|g| g.roles.iter().find(|r| r.id.to_string() == id))
                .map(|r| r.name.as_str())
                .unwrap_or(id);
            let count = group
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            return div()
                .h(px(44.))
                .px_3()
                .flex()
                .items_end()
                .pb_1()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(0x949ba4))
                .child(format!("{} — {}", name.to_uppercase(), count))
                .into_any_element();
        }
        let Some(value) = item.as_ref().and_then(|item| item.get("member")) else {
            return div()
                .h(px(44.))
                .px_3()
                .text_color(rgb(0x60636b))
                .child("· · ·")
                .into_any_element();
        };
        let Ok(row_member) = self.state.decode_api::<Member>(value.clone()) else {
            return div().h(px(44.)).into_any_element();
        };
        let member = self
            .state
            .members
            .get(&(guild, row_member.user.id))
            .cloned()
            .unwrap_or(row_member);
        let name = member
            .nick
            .clone()
            .unwrap_or_else(|| member.user.display_name().to_owned());
        let status = self
            .state
            .presences
            .get(&member.user.id)
            .map(|p| p.status)
            .unwrap_or_else(|| {
                match value
                    .pointer("/presence/status")
                    .and_then(serde_json::Value::as_str)
                {
                    Some("online") => PresenceStatus::Online,
                    Some("idle") => PresenceStatus::Idle,
                    Some("dnd") => PresenceStatus::Dnd,
                    _ => PresenceStatus::Offline,
                }
            });
        let color = match status {
            PresenceStatus::Online => 0x43b581,
            PresenceStatus::Idle => 0xfaa61a,
            PresenceStatus::Dnd => 0xf04747,
            _ => 0x747f8d,
        };
        let custom = if let Some(presence) = self.state.presences.get(&member.user.id) {
            presence
                .extra
                .get("custom_status")
                .and_then(|v| v.get("text"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        } else {
            value
                .pointer("/presence/custom_status/text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        }
        .to_owned();
        let user = member.user.clone();
        let id = user.id;
        let avatar = crate::avatar_images::member_avatar(
            &self.state,
            id,
            Some(guild),
            &name,
            self.media_endpoint.as_deref(),
            false,
            32.,
        );
        let avatar_target = self.profile_trigger(
            div()
                .id(SharedString::from(format!("member-avatar-{index}-{id}")))
                .size(px(32.))
                .child(avatar),
            user.clone(),
            Some(guild),
            false,
            cx,
        );
        let name_art =
            crate::emoji_sprites::unicode_text(&name, 14., window.scale_factor() > 1., cx);
        let name_target = self.profile_trigger(
            div()
                .id(SharedString::from(format!("member-name-{index}-{id}")))
                .child(name_art),
            user,
            Some(guild),
            false,
            cx,
        );
        let status_art =
            crate::emoji_sprites::unicode_text(&custom, 11., window.scale_factor() > 1., cx);
        crate::context_actions::trigger(
            div()
                .id(SharedString::from(format!("member-row-{index}-{id}")))
                .h(px(44.))
                .px_2()
                .rounded_md()
                .hover(|s| s.bg(rgb(0x393b42)))
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div().relative().size(px(32.)).child(avatar_target).child(
                        div()
                            .id(SharedString::from(format!("member-status-{index}-{id}")))
                            .absolute()
                            .right_0()
                            .bottom_0()
                            .size(px(10.))
                            .rounded_full()
                            .bg(rgb(color))
                            .on_click(|_, _, cx| cx.stop_propagation()),
                    ),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .items_start()
                        .child(name_target)
                        .when(!custom.is_empty(), |v| {
                            v.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(0x949ba4))
                                    .child(status_art),
                            )
                        }),
                ),
            crate::context_actions::Target::Member { guild, user: id },
            cx,
        )
        .into_any_element()
    }
}

fn requested_window(current: u32, range: std::ops::Range<usize>) -> Option<u32> {
    if range.is_empty()
        || (range.start == 0 && range.len() == 1)
        || (range.start >= current as usize && range.end <= current as usize + 100)
    {
        return None;
    }
    let start = range.start.saturating_sub(30).min(99_901) as u32;
    (start != current).then_some(start)
}
impl Fluxer {
    pub(super) fn can_kick_member(&self, guild: Snowflake, user: Snowflake) -> bool {
        if self.demo {
            return false;
        }
        self.state
            .current_user
            .as_ref()
            .and_then(|current| {
                Some(
                    fluxer_core::moderation_capabilities(
                        self.state.guilds.get(&guild)?,
                        self.state.members.get(&(guild, current.id))?,
                        self.state.members.get(&(guild, user))?,
                    )
                    .kick,
                )
            })
            .unwrap_or(false)
    }
    fn close_kick_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context.kick_confirm = None;
        self.context.kick_armed = false;
        if let Some(focus) = self.context.kick_previous.take() {
            window.focus(&focus);
        }
        cx.notify();
    }
    fn accept_kick_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((guild_id, user_id, _)) = self.context.kick_confirm.clone() {
            self.close_kick_confirmation(window, cx);
            if self.can_kick_member(guild_id, user_id) {
                self.command(Command::KickMember { guild_id, user_id }, cx);
            } else {
                self.error = Some("You can no longer kick this member".into());
            }
        }
    }
    pub(super) fn kick_confirmation(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((_, _, name)) = &self.context.kick_confirm else {
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
                    .id("kick-confirm-dialog")
                    .track_focus(&self.context.kick_focus)
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
                            "escape" => this.close_kick_confirmation(window, cx),
                            "tab" => {
                                this.context.kick_selected = !this.context.kick_selected;
                                this.context.kick_armed = false;
                                cx.notify();
                            }
                            "enter" | "space"
                                if !event.is_held && !event.keystroke.modifiers.modified() =>
                            {
                                this.context.kick_armed = true
                            }
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            let armed = std::mem::take(&mut this.context.kick_armed);
                            if armed && !event.keystroke.modifiers.modified() {
                                if this.context.kick_selected {
                                    this.accept_kick_confirmation(window, cx)
                                } else {
                                    this.close_kick_confirmation(window, cx)
                                }
                            }
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::BOLD)
                            .child("Kick member"),
                    )
                    .child(format!("Kick {name} from this community?"))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                crate::controls::quiet_button("kick-cancel", "Cancel")
                                    .bg(rgb(if !self.context.kick_selected {
                                        0x535064
                                    } else {
                                        0x383a40
                                    }))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_kick_confirmation(window, cx)
                                    })),
                            )
                            .child(
                                crate::controls::quiet_button("kick-confirm", "Kick member")
                                    .bg(rgb(if self.context.kick_selected {
                                        0xe15059
                                    } else {
                                        0xc43b45
                                    }))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.accept_kick_confirmation(window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::requested_window;
    #[test]
    fn viewport_window_is_bounded_and_coalesces_measurements() {
        assert_eq!(requested_window(0, 0..1), None);
        assert_eq!(requested_window(0, 20..35), None);
        assert_eq!(requested_window(0, 95..110), Some(65));
        assert_eq!(requested_window(65, 95..110), None);
        assert_eq!(requested_window(65, 5..20), Some(0));
        assert_eq!(requested_window(0, 100_000..100_001), Some(99_901));
    }
}
