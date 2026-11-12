// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

impl Fluxer {
    pub(super) fn chat(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
        div().id("chat-file-drop-target")
            .can_drop(move |dragged,_,_|can_attach && dragged.is::<ExternalPaths>())
            .drag_over::<ExternalPaths>(move |style,_,_,_|if can_attach {style.bg(rgb(0x302744)).border_color(rgb(ACCENT))}else{style})
            .on_drop(cx.listener(|this,paths:&ExternalPaths,_,cx| {
                if let Some(channel)=this.state.selected_channel {this.stage_attachment_paths(channel,paths.paths().to_vec(),cx);}
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
                    .child(heading(title))
                    .child(
                        button("search", "Search").on_click(cx.listener(|this, _, window, cx| this.open_search(window,cx))),
                    )
                    .child(button("open-pins","Pins").on_click(cx.listener(|this,_,_,cx|this.open_pins(cx))))
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
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .when(messages.is_empty(), |v| {
                        v.child(muted("This conversation is ready. Send the first message."))
                    })
                    .children(messages.into_iter().map(|message| {
                        let id = message.id;
                        let pin_message=message.clone();
                        let pin_pending=self.search_ui.mutations.contains(&(message.channel_id,message.id));
                        let content = message.content.clone();
                        let copy_content = chat_helpers::display_text(&message);
                        let has_text = !copy_content.is_empty();
                        let own = self
                            .state
                            .current_user
                            .as_ref()
                            .is_some_and(|u| u.id == message.author.id);
                        div()
                            .id(("message", id.0))
                            .relative()
                            .when(self.search_ui.highlight==Some((message.channel_id,message.id)),|view|view.bg(rgb(0x37304f)).border_l_2().border_color(rgb(ACCENT)).pl_2())
                            .child(self.visible_message_marker(&message,cx))
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
                                            .child(muted(if message.edited_timestamp.is_some() {
                                                "edited".into()
                                            } else {
                                                message.timestamp.clone()
                                            })),
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
                                    .child(
                                        div()
                                            .flex()
                                            .gap_2()
                                            .flex_wrap()
                                            .child(
                                                button(("react", id.0), "Add reaction")
                                                    .on_click(cx.listener(
                                                    move |this, event: &ClickEvent, window, cx| {
                                                        this.open_emoji(
                                                            EmojiTarget::Reaction(id),
                                                            event.position(),
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                            )
                                            .when(self.can_pin_message(&message),|view|view.child(button(("pin-message",id.0),if pin_pending{"Updating…"}else if message.pinned{"Unpin"}else{"Pin"}).when(pin_pending,|view|view.opacity(0.5)).on_click(cx.listener(move|this,event:&ClickEvent,window,cx|{if !pin_pending{this.request_pin(pin_message.clone(),!pin_message.pinned,event.modifiers().shift,window,cx);}}))))
                                            .child(button(("reply", id.0), "Reply").on_click(
                                                cx.listener(move |this, _, window, cx| {
                                                    if let Some(message) = this
                                                        .state
                                                        .selected_channel
                                                        .and_then(|channel| {
                                                            this.state
                                                                .channel_messages(channel)
                                                                .find(|message| message.id == id)
                                                        })
                                                        .cloned()
                                                    {
                                                        this.replies.insert(
                                                            message.channel_id,
                                                            (message, false),
                                                        );
                                                        this.cancel_edit(cx);
                                                        this.composer
                                                            .focus_handle(cx)
                                                            .focus(window);
                                                        cx.notify();
                                                    }
                                                }),
                                            ))
                                            .child(button(("forward", id.0), "Forward").on_click(
                                                cx.listener(move |this, _, window, cx| {
                                                    this.forwarding = this
                                                        .state
                                                        .selected_channel
                                                        .and_then(|channel| {
                                                            this.state
                                                                .channel_messages(channel)
                                                                .find(|message| message.id == id)
                                                        })
                                                        .cloned();
                                                    this.forward_search.update(cx, |input, cx| {
                                                        input.set(String::new(), cx)
                                                    });
                                                    this.forward_search
                                                        .focus_handle(cx)
                                                        .focus(window);
                                                    this.emoji_target = None;
                                                    cx.notify();
                                                }),
                                            ))
                                            .child(button(("copy", id.0), "Copy").on_click(
                                                cx.listener(move |_, _, _, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(
                                                            copy_content.clone(),
                                                        ),
                                                    );
                                                }),
                                            ))
                                            .children(message.reactions.iter().enumerate().map(
                                                |(index, r)| {
                                                    let emoji =
                                                        chat_helpers::reaction_key(&r.emoji);
                                                    button(
                                                        SharedString::from(format!(
                                                            "reaction-chip-{id}-{index}"
                                                        )),
                                                        format!(
                                                            "{} {}",
                                                            if r.emoji.id.is_some() {
                                                                format!(":{}:", r.emoji.name)
                                                            } else {
                                                                r.emoji.name.clone()
                                                            },
                                                            r.count
                                                        ),
                                                    )
                                                    .when(r.me, |view| {
                                                        view.bg(rgb(0x56438b))
                                                            .border_1()
                                                            .border_color(rgb(ACCENT))
                                                    })
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
                                            ))
                                            .when(own, |v| {
                                                v.child(button(("edit", id.0), "Edit").on_click(
                                                    cx.listener(move |this, _, window, cx| {
                                                        if this.message_pending() {
                                                            return;
                                                        }
                                                        if this.editing.is_none() {
                                                            this.edit_draft = Some(
                                                                this.composer
                                                                    .read(cx)
                                                                    .content
                                                                    .to_string(),
                                                            );
                                                        }
                                                        this.editing = Some(id);
                                                        this.composer.update(cx, |v, cx| {
                                                            v.set(content.clone(), cx)
                                                        });
                                                        this.composer
                                                            .focus_handle(cx)
                                                            .focus(window);
                                                        cx.notify();
                                                    }),
                                                ))
                                                .child(button(("delete", id.0), "Delete").on_click(
                                                    cx.listener(move |this, _, _, cx| {
                                                        this.command(
                                                            Command::DeleteMessage {
                                                                message_id: id,
                                                            },
                                                            cx,
                                                        )
                                                    }),
                                                ))
                                            }),
                                    ),
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
                    .when(self.editing.is_none(), |view| view.child(self.attachment_strip(cx)))
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
                                            button(
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
                                            button("cancel-reply", "Cancel reply")
                                                .flex_shrink_0()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    if let Some(channel) =
                                                        this.state.selected_channel
                                                    {
                                                        this.replies.remove(&channel);
                                                    }
                                                    cx.notify();
                                                })),
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
                                .child(button("cancel-edit", "Cancel edit").on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.cancel_edit(cx);
                                        cx.notify();
                                    },
                                ))),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(self.attachment_button(cx))
                            .child(div().flex_1().min_w_0().child(self.composer.clone()))
                            .child(button("composer-emoji", "☺").on_click(cx.listener(
                                |this, event: &ClickEvent, window, cx| {
                                    this.open_emoji(
                                        EmojiTarget::Composer,
                                        event.position(),
                                        window,
                                        cx,
                                    )
                                },
                            )))
                            .child(
                                button(
                                    "send",
                                    if self.message_pending() {
                                        "Sending…"
                                    } else {
                                        "Send"
                                    },
                                )
                                .bg(rgb(0x6f56d7))
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.submit(&Submit, window, cx),
                                )),
                            ),
                    )
                    .when(self.pane != Pane::Call, |view| {
                        view.child(muted(
                            "Enter to send · Shift+Enter for a new line · Drop files or paste an image",
                        ))
                    }),
            )
            .into_any_element()
    }
}
