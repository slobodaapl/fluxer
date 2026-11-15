// SPDX-License-Identifier: AGPL-3.0-or-later
//! Server-provided lazy member rows; pages never use message authors as a roster.
use super::*;
use crate::controls::quiet_button;
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
        let list = self.state.member_lists.get(&(guild, channel));
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(list) = list {
            for (index, item) in list
                .rows
                .range(self.members_start..=self.members_start.saturating_add(99))
            {
                if let Some(group) = item.get("group") {
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
                    rows.push(
                        div()
                            .px_3()
                            .pt_4()
                            .pb_1()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0x949ba4))
                            .child(format!("{} — {}", name.to_uppercase(), count))
                            .into_any_element(),
                    );
                } else if let Some(value) = item.get("member") {
                    if let Ok(row_member) = self.state.decode_api::<Member>(value.clone()) {
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
                        let custom =
                            if let Some(presence) = self.state.presences.get(&member.user.id) {
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
                        rows.push(
                            self.profile_trigger(
                                div()
                                    .id(SharedString::from(format!("member-{index}-{id}")))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .hover(|s| s.bg(rgb(0x393b42)))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div().relative().size(px(32.)).child(avatar).child(
                                            div()
                                                .absolute()
                                                .right_0()
                                                .bottom_0()
                                                .size(px(10.))
                                                .rounded_full()
                                                .bg(rgb(color)),
                                        ),
                                    )
                                    .child(div().min_w_0().flex().flex_col().child(name).when(
                                        !custom.is_empty(),
                                        |v| {
                                            v.child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(rgb(0x949ba4))
                                                    .child(custom),
                                            )
                                        },
                                    )),
                                user,
                                Some(guild),
                                false,
                                cx,
                            )
                            .into_any_element(),
                        );
                    }
                }
            }
        }
        let has_next = list.is_some_and(|l| {
            u64::from(self.members_start + 100)
                < l.member_count
                    + l.rows.values().filter(|r| r.get("group").is_some()).count() as u64
        });
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
                    .text_size(px(12.))
                    .text_color(rgb(0xb5bac1))
                    .child(
                        list.map(|l| format!("{} online", l.online_count))
                            .unwrap_or_else(|| "Loading members…".into()),
                    ),
            )
            .child(
                div()
                    .id("member-list-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .px_2()
                    .py_2()
                    .when(self.members_start > 0, |v| {
                        v.child(
                            quiet_button("members-prev", "Previous").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.members_start = this.members_start.saturating_sub(100);
                                    this.command(
                                        Command::SetMemberList {
                                            channel_id: this.state.selected_channel,
                                            start: this.members_start,
                                        },
                                        cx,
                                    );
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .when(has_next, |v| {
                        v.child(quiet_button("members-next", "Next").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.members_start += 100;
                                this.command(
                                    Command::SetMemberList {
                                        channel_id: this.state.selected_channel,
                                        start: this.members_start,
                                    },
                                    cx,
                                );
                                cx.notify();
                            },
                        )))
                    }),
            )
            .into_any_element()
    }
}
