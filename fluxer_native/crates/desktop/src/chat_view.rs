// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::controls::{Icon, icon_button, quiet_button};

impl Fluxer {
    pub(super) fn chat(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let channel = self
            .state
            .selected_channel
            .and_then(|id| self.state.channels.get(&id));
        let voice_channel = channel
            .filter(|channel| channel.kind.is_voice())
            .map(|channel| channel.id);
        let title = channel
            .map(|c| format!("{} {}", if c.kind.is_voice() { "◉" } else { "#" }, c.name))
            .unwrap_or("Choose a channel".into());
        let selected = self.state.selected_channel;
        let messages = self.visible_messages();
        let visible: BTreeSet<_> = messages.iter().map(|message| message.id).collect();
        self.message_text.retain(|id, _| visible.contains(id));
        self.message_actions
            .row_focus
            .retain(|id, _| visible.contains(id));
        self.message_actions
            .row_bounds
            .borrow_mut()
            .retain(|id, _| visible.contains(id));
        for id in &visible {
            self.message_actions
                .row_focus
                .entry(*id)
                .or_insert_with(|| cx.focus_handle());
        }
        for message in &messages {
            let text = chat_helpers::display_text(message);
            if let Some(input) = self.message_text.get(&message.id) {
                input.update(cx, |input, cx| input.update_read_only(&text, cx));
            } else {
                self.message_text
                    .insert(message.id, cx.new(|cx| TextInput::read_only(&text, cx)));
            }
        }
        let can_attach = self.attachment_block_reason().is_none();
        div()
            .id("chat-file-drop-target")
            .can_drop(move |dragged, _, _| can_attach && dragged.is::<ExternalPaths>())
            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                if can_attach {
                    style.bg(rgb(0x302744)).border_color(rgb(ACCENT))
                } else {
                    style
                }
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(channel) = this.state.selected_channel {
                    this.stage_attachment_paths(channel, paths.paths().to_vec(), cx);
                }
            }))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .flex()
            .flex_col()
            .when(self.pane == Pane::Call, |view| view.overflow_hidden())
            .child(
                div()
                    .h(px(if self.pane == Pane::Call { 38. } else { 60. }))
                    .flex_shrink_0()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(PANEL))
                    .child(heading(title).flex_1().min_w_0())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                icon_button("search", Icon::Search, "Search messages · Ctrl+F")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_search(window, cx)
                                    })),
                            )
                            .child(
                                icon_button("open-pins", Icon::Pin, "Pinned messages")
                                    .on_click(cx.listener(|this, _, _, cx| this.open_pins(cx))),
                            ),
                    ),
            )
            .when(
                self.pane == Pane::Chat && self.viewer.focused.is_some(),
                |view| {
                    view.child(div().px_5().py_2().bg(rgb(0x30283f)).child(
                        button("return-to-stream", "Return to stream viewer").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.pane = Pane::Call;
                                cx.notify();
                            }),
                        ),
                    ))
                },
            )
            .when_some(
                voice_channel.filter(|_| self.pane != Pane::Call),
                |view, channel| view.child(self.voice_participant_pane(channel, cx)),
            )
            .when(
                self.pane != Pane::Call && !self.available_streams.is_empty(),
                |v| {
                    v.child(
                        div().flex().flex_wrap().gap_3().p_3().children(
                            self.available_streams
                                .iter()
                                .enumerate()
                                .map(|(index, identity)| {
                                    let watching = self.viewer.desired.contains(identity);
                                    let name = self
                                        .participants
                                        .get(identity)
                                        .map(|(name, _)| name.clone())
                                        .filter(|name| !name.is_empty())
                                        .or_else(|| {
                                            self.state.voice.channel_id.and_then(|channel| {
                                                self.state
                                                    .voice_participants(channel)
                                                    .into_iter()
                                                    .find(|participant| {
                                                        participant.livekit_identity.as_ref()
                                                            == Some(identity)
                                                    })
                                                    .map(|participant| participant.display_name)
                                            })
                                        })
                                        .unwrap_or_else(|| identity.clone());
                                    let identity = identity.clone();
                                    button(
                                        ("watch-stream", index),
                                        format!(
                                            "{} {name}'s screen",
                                            if watching { "Stop watching" } else { "Watch" }
                                        ),
                                    )
                                    .when(watching, |v| v.bg(rgb(0x6650bd)))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.watch_stream(identity.clone(), !watching, cx)
                                        },
                                    ))
                                }),
                        ),
                    )
                },
            )
            .when(voice_channel.is_some(), |view| {
                view.child(
                    div()
                        .px_5()
                        .pt_3()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(MUTED))
                        .child("CHANNEL CHAT"),
                )
            })
            .child(self.history_banner(cx))
            .child(
                div()
                    .id("timeline")
                    .track_scroll(&self.timeline)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(messages.is_empty(), |v| {
                        v.child(muted("This conversation is ready. Send the first message."))
                    })
                    .children(messages.into_iter().map(|message| {
                        let id = message.id;
                        let has_text = !chat_helpers::display_text(&message).is_empty();
                        let row_focus = self
                            .message_actions
                            .row_focus
                            .get(&id)
                            .expect("visible message focus")
                            .clone();
                        let focused = row_focus.contains_focused(window, cx);
                        let toolbar = self.message_toolbar(&message, focused, cx);
                        let context_message = message.clone();
                        let keyboard_message = message.clone();
                        let group = SharedString::from(format!("message-{id}"));
                        div()
                            .id(("message", id.0))
                            .relative()
                            .group(group)
                            .track_focus(&row_focus)
                            .px_2()
                            .py_2()
                            .rounded_md()
                            .hover(|row| row.bg(rgb(0x25222f)))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.open_message_menu(
                                        context_message.clone(),
                                        event.position,
                                        message_actions::MessageMenuContext::Timeline,
                                        window,
                                        cx,
                                    );
                                }),
                            )
                            .on_key_down(cx.listener(
                                move |this, event: &KeyDownEvent, window, cx| {
                                    if event.keystroke.key == "f10"
                                        && event.keystroke.modifiers.shift
                                    {
                                        this.open_message_menu(
                                            keyboard_message.clone(),
                                            this.message_menu_anchor(id),
                                            message_actions::MessageMenuContext::Timeline,
                                            window,
                                            cx,
                                        );
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                            .child(self.message_bounds_marker(id))
                            .child(toolbar)
                            .when(
                                self.search_ui.highlight == Some((message.channel_id, message.id)),
                                |view| {
                                    view.bg(rgb(0x37304f))
                                        .border_l_2()
                                        .border_color(rgb(ACCENT))
                                        .pl_2()
                                },
                            )
                            .child(self.visible_message_marker(&message, cx))
                            .w_full()
                            .min_w_0()
                            .flex_shrink_0()
                            .flex()
                            .gap_3()
                            .child(rich_message::avatar(
                                &message.author,
                                self.media_endpoint.as_deref(),
                                36.,
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
                                            .flex()
                                            .gap_3()
                                            .items_center()
                                            .child(
                                                div().font_weight(FontWeight::SEMIBOLD).child(
                                                    message.author.display_name().to_string(),
                                                ),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(rgb(MUTED))
                                                    .child(message_actions::message_time(
                                                        &message.timestamp,
                                                    )),
                                            )
                                            .when(message.edited_timestamp.is_some(), |row| {
                                                row.child(
                                                    div()
                                                        .text_size(px(10.))
                                                        .text_color(rgb(MUTED))
                                                        .child("(edited)"),
                                                )
                                            }),
                                    )
                                    .when_some(
                                        chat_helpers::reference_summary(&self.state, &message),
                                        |view, summary| {
                                            view.child(muted(summary).w_full().min_w_0())
                                        },
                                    )
                                    .when(has_text, |view| {
                                        view.child(
                                            self.message_text
                                                .get(&id)
                                                .expect("cached message")
                                                .clone(),
                                        )
                                    })
                                    .child(rich_message::media(&message))
                                    .when(!message.reactions.is_empty(), |view| {
                                        view.child(div().flex().gap_1().flex_wrap().children(
                                            message.reactions.iter().enumerate().map(
                                                |(index, reaction)| {
                                                    let emoji =
                                                        chat_helpers::reaction_key(&reaction.emoji);
                                                    controls::quiet_button(
                                                        SharedString::from(format!(
                                                            "reaction-chip-{id}-{index}"
                                                        )),
                                                        format!(
                                                            "{} {}",
                                                            if reaction.emoji.id.is_some() {
                                                                format!(":{}:", reaction.emoji.name)
                                                            } else {
                                                                reaction.emoji.name.clone()
                                                            },
                                                            reaction.count
                                                        ),
                                                    )
                                                    .h(px(24.))
                                                    .px_2()
                                                    .py_0()
                                                    .text_size(px(12.))
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(rgb(if reaction.me {
                                                        ACCENT
                                                    } else {
                                                        0x413b4c
                                                    }))
                                                    .bg(rgb(if reaction.me {
                                                        0x3c3256
                                                    } else {
                                                        0x2d2939
                                                    }))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.command(
                                                            Command::React {
                                                                message_id: id,
                                                                emoji: emoji.clone(),
                                                            },
                                                            cx,
                                                        )
                                                    }))
                                                },
                                            ),
                                        ))
                                    }),
                            )
                    })),
            )
            .when(self.forwarding.is_some(), |view| {
                view.child(self.forward_panel(cx))
            })
            .child(
                div()
                    .p(if self.pane == Pane::Call {
                        px(8.)
                    } else {
                        px(16.)
                    })
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(self.editing.is_none(), |view| {
                        view.child(self.attachment_strip(cx))
                    })
                    .when(self.editing.is_none(), |view| {
                        view.when_some(
                            selected.and_then(|channel| self.replies.get(&channel)),
                            |view, (message, mention)| {
                                let mention = *mention;
                                view.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            muted(format!(
                                                "Replying to {} · {}",
                                                message.author.display_name(),
                                                chat_helpers::preview(message)
                                            ))
                                            .flex_1()
                                            .min_w_0(),
                                        )
                                        .child(
                                            quiet_button(
                                                "reply-mention",
                                                if mention { "Mention on" } else { "Mention off" },
                                            )
                                            .flex_shrink_0()
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    if let Some((_, mention)) = this
                                                        .state
                                                        .selected_channel
                                                        .and_then(|channel| {
                                                            this.replies.get_mut(&channel)
                                                        })
                                                    {
                                                        *mention = !*mention;
                                                    }
                                                    cx.notify();
                                                }),
                                            ),
                                        )
                                        .child(
                                            icon_button(
                                                "cancel-reply",
                                                Icon::Close,
                                                "Cancel reply",
                                            )
                                            .flex_shrink_0()
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    if let Some(channel) =
                                                        this.state.selected_channel
                                                    {
                                                        this.replies.remove(&channel);
                                                    }
                                                    cx.notify();
                                                }),
                                            ),
                                        ),
                                )
                            },
                        )
                    })
                    .when(self.editing.is_some(), |v| {
                        v.child(
                            div()
                                .flex()
                                .gap_3()
                                .child(muted("Editing message · press Enter to save"))
                                .child(quiet_button("cancel-edit", "Cancel edit").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.cancel_edit(cx);
                                        cx.notify();
                                    }),
                                )),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .p_2()
                            .rounded_lg()
                            .bg(rgb(0x302b3c))
                            .border_1()
                            .border_color(rgb(0x40394d))
                            .child(self.attachment_button(cx))
                            .child(div().flex_1().min_w_0().child(self.composer.clone()))
                            .child(
                                icon_button("composer-emoji", Icon::React, "Choose emoji")
                                    .on_click(cx.listener(
                                        |this, event: &ClickEvent, window, cx| {
                                            this.open_emoji(
                                                EmojiTarget::Composer,
                                                event.position(),
                                                window,
                                                cx,
                                            )
                                        },
                                    )),
                            )
                            .child(
                                icon_button(
                                    "send",
                                    Icon::Send,
                                    if self.message_pending() {
                                        "Sending…"
                                    } else {
                                        "Send message"
                                    },
                                )
                                .when(self.message_pending(), |button| button.opacity(0.5))
                                .text_color(rgb(ACCENT))
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.submit(&Submit, window, cx),
                                )),
                            ),
                    )
                    .when(self.pane != Pane::Call, |view| {
                        view.child(
                            div()
                                .px_2()
                                .text_size(px(10.))
                                .text_color(rgb(MUTED))
                                .child("Enter to send · Shift+Enter for a new line"),
                        )
                    }),
            )
            .into_any_element()
    }
}
