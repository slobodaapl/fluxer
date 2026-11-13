// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::controls::{Icon, icon, icon_button, quiet_button};
use crate::message_grouping::{
    COZY_AVATAR_SIZE, COZY_GUTTER, COZY_LINE_HEIGHT, COZY_SPACING_Y, message_layouts,
};

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
        let layouts = message_layouts(&messages);
        self.message_actions.row_order = messages.iter().map(|message| message.id).collect();
        let tab_entry = messages.last().map(|message| message.id);
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
            let handle = self
                .message_actions
                .row_focus
                .entry(*id)
                .or_insert_with(|| cx.focus_handle());
            *handle = handle.clone().tab_stop(Some(*id) == tab_entry);
        }
        self.message_content.retain_replies(selected, &visible);
        let mut retained: BTreeSet<_> = messages.iter().map(|m| (m.channel_id, m.id)).collect();
        retained.extend(self.search_ui.cached_text_keys());
        retained.extend(
            messages
                .iter()
                .filter_map(|m| chat_helpers::referenced_message(&self.state, m))
                .map(|m| (m.channel_id, m.id)),
        );
        self.message_content.cache.retain_visible(&retained);
        for message in messages
            .iter()
            .filter(|message| !pin_system_message::is_pin_system_message(message))
        {
            let document = self.message_document(message, message_markup::MessageContext::Standard);
            if let Some(input) = self.message_text.get(&message.id) {
                input.update(cx, |input, cx| input.update_message_document(document, cx));
            } else {
                let input = self.message_text_input(document, window, cx);
                self.message_text.insert(message.id, input);
            }
        }
        let pending_rows = self.pending_message_rows(window, cx);
        let conversation_empty = messages.is_empty() && pending_rows.is_empty();
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
                    .id("channel-header")
                    .when_some(self.state.selected_channel, |view, channel| crate::context_actions::trigger(view, crate::context_actions::Target::Channel(channel), cx))
                    .h(px(if self.pane == Pane::Call { 38. } else { 60. }))
                    .flex_shrink_0()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(PANEL))
                    .child(div().font_weight(FontWeight::SEMIBOLD).flex_1().min_w_0().child(crate::emoji_sprites::unicode_text(&title, 18., window.scale_factor() > 1., cx)))
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
                self.pane == Pane::Chat
                    && self.viewer.focused.is_some()
                    && self.available_streams.is_empty(),
                |view| {
                    view.child(div().px_4().py_1().child(
                        quiet_button("return-to-stream", "Return to stream").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.pane = Pane::Call;
                                cx.notify();
                            },
                        )),
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
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .py_1()
                            .when(self.viewer.focused.is_some(), |view| {
                                view.child(
                                    quiet_button("return-to-stream", "Return to stream").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.pane = Pane::Call;
                                            cx.notify();
                                        }),
                                    ),
                                )
                            })
                            .children(self.available_streams.iter().enumerate().map(
                                |(index, identity)| {
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
                                    div()
                                        .id(("available-stream-context", index))
                                        .map(|view| crate::context_actions::trigger(view, crate::context_actions::Target::Stream(identity.clone()), cx))
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .pl_2()
                                        .rounded_md()
                                        .bg(rgb(PANEL))
                                        .child(icon(Icon::ScreenShare, 14.))
                                        .child(
                                            div()
                                                .max_w(px(140.))
                                                .text_size(px(12.))
                                                .text_ellipsis()
                                                .child(name.clone()),
                                        )
                                        .child(if watching {
                                            icon_button(
                                                ("watch-stream", index),
                                                Icon::Stop,
                                                format!("Stop watching {name}'s screen"),
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.watch_stream(identity.clone(), false, cx)
                                            }))
                                            .into_any_element()
                                        } else {
                                            quiet_button(("watch-stream", index), "Watch")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.watch_stream(identity.clone(), true, cx)
                                                }))
                                                .into_any_element()
                                        })
                                },
                            )),
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
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .child(
                        div()
                            .id("timeline")
                            .track_scroll(&self.timeline)
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px_0()
                            .py_3()
                            .flex()
                            .flex_col()
                            .gap_0()
                            .when(conversation_empty, |v| {
                                v.child(muted(
                                    "This conversation is ready. Send the first message.",
                                ))
                            })
                            .children(messages.into_iter().zip(layouts).map(|(message, layout)| {
                                let id = message.id;
                                let is_pin = pin_system_message::is_pin_system_message(&message);
                                let has_text =
                                    !is_pin && !chat_helpers::display_text(&message).is_empty();
                                let pin_content = self.pin_system_message(&message, cx);
                                let reply_preview =
                                    self.message_reply_preview(&message, window, cx);
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
                                let timestamp_label = layout.short_time.clone();
                                let timestamp_tooltip = message.timestamp.clone();
                                let gutter = if is_pin {
                                    pin_system_message::gutter()
                                } else if layout.starts_group {
                                    div()
                                        .w(px(COZY_AVATAR_SIZE))
                                        .flex_shrink_0()
                                        .pt(px(2.))
                                        .child(rich_message::avatar(
                                            &message.author,
                                            self.media_endpoint.as_deref(),
                                            COZY_AVATAR_SIZE,
                                        ))
                                        .into_any_element()
                                } else {
                                    div()
                                        .id(("grouped-message-time", id.0))
                                        .w(px(COZY_AVATAR_SIZE))
                                        .h(px(COZY_LINE_HEIGHT))
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .justify_end()
                                        .text_size(px(11.))
                                        .text_color(rgb(MUTED))
                                        .invisible()
                                        .group_hover(group.clone(), |style| style.visible())
                                        .when(
                                            focused
                                                || self.message_actions.active_message()
                                                    == Some(id),
                                            |label| label.visible(),
                                        )
                                        .tooltip(move |_, cx| {
                                            cx.new(|_| {
                                                MessageTimestampTooltip(timestamp_tooltip.clone())
                                            })
                                            .into()
                                        })
                                        .child(timestamp_label)
                                        .into_any_element()
                                };
                                let row =
                                    div()
                                        .id(("message", id.0))
                                        .relative()
                                        .group(group)
                                        .track_focus(&row_focus)
                                        .px(px(16.))
                                        .py(px(COZY_SPACING_Y))
                                        .rounded_md()
                                        .hover(|row| row.bg(rgb(0x25222f)))
                                        .focus(|row| row.bg(rgb(0x2f293c)))
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            cx.listener(
                                                move |this, event: &MouseDownEvent, window, cx| {
                                                    cx.stop_propagation();
                                                    this.open_message_menu(
                                        context_message.clone(),
                                        event.position,
                                        message_actions::MessageMenuContext::Timeline,
                                        window,
                                        cx,
                                    );
                                                },
                                            ),
                                        )
                                        .on_key_down(cx.listener(
                                            move |this, event: &KeyDownEvent, window, cx| {
                                                if matches!(
                                                    event.keystroke.key.as_str(),
                                                    "up" | "down"
                                                ) && event.keystroke.modifiers
                                                    == Modifiers::default()
                                                    && this
                                                        .message_actions
                                                        .row_focus
                                                        .get(&id)
                                                        .is_some_and(|focus| {
                                                            focus.is_focused(window)
                                                        })
                                                {
                                                    this.move_message_focus(
                                                        id,
                                                        event.keystroke.key == "down",
                                                        window,
                                                        cx,
                                                    );
                                                    cx.stop_propagation();
                                                    return;
                                                }
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
                                        .when(
                                            self.search_ui.highlight
                                                == Some((message.channel_id, message.id)),
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
                                        .gap(px(COZY_GUTTER))
                                        .line_height(px(COZY_LINE_HEIGHT))
                                        .child(gutter)
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .flex_col()
                                                .gap_0()
                                                .when(layout.starts_group && !is_pin, |body| {
                                                    body.child(
                                            div()
                                                .h(px(COZY_LINE_HEIGHT))
                                                .flex()
                                                .gap_2()
                                                .items_center()
                                                .child(
                                                    div().font_weight(FontWeight::SEMIBOLD).child(
                                                        crate::emoji_sprites::unicode_text(message.author.display_name(), 14., window.scale_factor() > 1., cx),
                                                    ),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(12.))
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
                                                })
                                                .when_some(pin_content, |view, content| view.child(content))
                                                .when_some(reply_preview, |view, preview| {
                                                    view.child(preview)
                                                })
                                                .when(has_text, |view| {
                                                    view.child(
                                                        div()
                                                            .flex()
                                                            .items_end()
                                                            .gap_1()
                                                            .w_full()
                                                            .min_w_0()
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .min_w_0()
                                                                    .line_height(px(
                                                                        COZY_LINE_HEIGHT,
                                                                    ))
                                                                    .child(
                                                                        self.message_text
                                                                            .get(&id)
                                                                            .expect(
                                                                                "cached message",
                                                                            )
                                                                            .clone(),
                                                                    ),
                                                            )
                                                            .when(
                                                                !layout.starts_group
                                                                    && message
                                                                        .edited_timestamp
                                                                        .is_some(),
                                                                |body| {
                                                                    body.child(
                                                                        div()
                                                                            .pb(px(2.))
                                                                            .text_size(px(10.))
                                                                            .text_color(rgb(MUTED))
                                                                            .child("(edited)"),
                                                                    )
                                                                },
                                                            ),
                                                    )
                                                })
                                                .when(!is_pin, |view| {
                                                    view.child(rich_message::media(
                                                        &message,
                                                        rich_message::MediaMotion {
                                                            enabled: !(self.image_viewer.is_open() || self.playback.modal.is_open()),
                                                            autoplay: media_animation::gif_autoplay(&self.state.user_settings),
                                                            surface: "timeline",
 hide_downloads: crate::media_context_policy::hide_media_downloads(message.channel_id, crate::media_context_policy::MediaKind::Image, &self.state.channels),
                                                        },
                                                        &self.revealed_media,
                                                        &mut self.playback,
                                                        window,
                                                        cx,
                                                    ))
                                                })
                                                .when(!message.reactions.is_empty(), |view| {
                                                    view.child(
                                                        div().flex().gap_1().flex_wrap().children(
                                                            message
                                                                .reactions
                                                                .iter()
                                                                .enumerate()
                                                                .map(|(index, reaction)| {
                                                                    let emoji =
                                                                        chat_helpers::reaction_key(
                                                                            &reaction.emoji,
                                                                        );
                                                                    controls::quiet_button(
                                                        SharedString::from(format!(
                                                            "reaction-chip-{id}-{index}"
                                                        )),
                                                        "",
                                                    )
                                                    .flex()
                                                    .items_center()
                                                    .gap_1()
                                                    .child(if let Some(emoji_id) = reaction.emoji.id {
                                                        if let Some(endpoint) = self.media_endpoint.as_deref() {
                                                            img(SharedString::from(format!("{}/emojis/{emoji_id}.webp?size=48", endpoint.trim_end_matches('/'))))
                                                                .size(px(16.)).object_fit(ObjectFit::Contain).into_any_element()
                                                        } else {
                                                            div().size(px(16.)).into_any_element()
                                                        }
                                                    } else {
                                                        crate::emoji_sprites::unicode_art(&reaction.emoji.name, 16., window.scale_factor() > 1., cx)
                                                    })
                                                    .child(reaction.count.to_string())
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
                                                                }),
                                                        ),
                                                    )
                                                }),
                                        )
                                        // Paint the solid toolbar after text/media. Absolute
                                        // positioning alone does not put it above siblings;
                                        // selectable text used to paint through its backing.
                                        .child(toolbar);
                                // Keep exactly one scroll child per message. Dividers and
                                // inter-group spacing live INSIDE this wrapper, so history
                                // centering and roving keyboard indices remain unchanged.
                                div()
                                    .id(("timeline-entry", id.0))
                                    .w_full()
                                    .min_w_0()
                                    .flex_shrink_0()
                                    .flex()
                                    .flex_col()
                                    .when_some(layout.date_divider, |entry, label| {
                                        entry.child(
                                            div()
                                                .h(px(20.))
                                                .my(px(16.))
                                                .px(px(20.))
                                                .flex()
                                                .items_center()
                                                .child(div().flex_1().h(px(2.)).bg(rgb(0x403a4c)))
                                                .child(
                                                    div()
                                                        .px_3()
                                                        .text_size(px(12.))
                                                        .text_color(rgb(MUTED))
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .child(label),
                                                )
                                                .child(div().flex_1().h(px(2.)).bg(rgb(0x403a4c))),
                                        )
                                    })
                                    .when(layout.space_before > 0, |entry| {
                                        entry.child(
                                            div().h(px(layout.space_before as f32)).flex_shrink_0(),
                                        )
                                    })
                                    .child(row)
                            }))
                            .children(pending_rows),
                    )
                    .child(scrollbar::Scrollbar::new(
                        "chat-scrollbar",
                        self.timeline.clone(),
                    )),
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
                                            div()
                                                .text_color(rgb(MUTED))
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .child(crate::emoji_sprites::unicode_text(
                                                    &format!("Replying to {} · {}", message.author.display_name(), chat_helpers::preview(message)),
                                                    12., window.scale_factor() > 1., cx,
                                                )),
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

struct MessageTimestampTooltip(String);
impl Render for MessageTimestampTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(RAIL))
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .child(self.0.clone())
    }
}
