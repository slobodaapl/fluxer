// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::controls::{Icon, icon, icon_button, quiet_button};
use crate::message_grouping::{
    COZY_AVATAR_SIZE, COZY_GUTTER, COZY_LINE_HEIGHT, COZY_SPACING_Y, message_layouts,
};

impl Fluxer {
    fn sticker_message(&self, message: &Message) -> AnyElement {
        let stickers = message
            .extra
            .get("stickers")
            .and_then(serde_json::Value::as_array);
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(stickers.into_iter().flatten().filter_map(|sticker| {
                let id = sticker.get("id")?.as_str()?;
                let endpoint = self.media_endpoint.as_deref()?;
                let animated = sticker
                    .get("animated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                    && !self.interface.reduced_motion();
                Some(
                    img(SharedString::from(format!(
                        "{}/stickers/{}.webp?size=320&animated={}",
                        endpoint.trim_end_matches('/'),
                        id,
                        animated
                    )))
                    .w(px(160.))
                    .h(px(160.))
                    .object_fit(ObjectFit::Contain),
                )
            }))
            .into_any_element()
    }

    fn sticker_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let channel_id = self.state.selected_channel;
        let stickers = self
            .state
            .selected_channel
            .and_then(|id| self.state.channels.get(&id))
            .and_then(|channel| channel.guild_id)
            .and_then(|id| self.state.guilds.get(&id))
            .and_then(|guild| guild.extra.get("stickers"))
            .and_then(serde_json::Value::as_array);
        let choices: Vec<_> = stickers
            .into_iter()
            .flatten()
            .filter_map(|sticker| {
                let channel_id = channel_id?;
                let id = sticker.get("id")?.as_str()?.parse::<Snowflake>().ok()?;
                let name = sticker
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Sticker")
                    .to_owned();
                let endpoint = self.media_endpoint.as_deref()?;
                let url = format!(
                    "{}/stickers/{}.webp?size=160&animated=false",
                    endpoint.trim_end_matches('/'),
                    id
                );
                Some(
                    quiet_button(("sticker-choice", id.0), name)
                        .child(
                            img(SharedString::from(url))
                                .w(px(64.))
                                .h(px(64.))
                                .object_fit(ObjectFit::Contain),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sticker_picker_open = false;
                            this.command(
                                Command::SendSticker {
                                    channel_id,
                                    sticker_id: id,
                                },
                                cx,
                            );
                            cx.notify();
                        })),
                )
            })
            .collect();
        let empty = choices.is_empty();
        div()
            .id("sticker-picker")
            .p_2()
            .bg(rgb(PANEL))
            .max_h(px(240.))
            .overflow_y_scroll()
            .flex()
            .flex_wrap()
            .gap_2()
            .when(empty, |view| view.child("No community stickers available"))
            .children(choices)
            .into_any_element()
    }

    pub(super) fn chat(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let channel = self
            .state
            .selected_channel
            .and_then(|id| self.state.channels.get(&id));
        if let Some(channel) = channel {
            let gate = self.state.mature_content_gate(channel);
            if gate != fluxer_core::MatureContentGate::None {
                let (title, description) = match gate {
                    fluxer_core::MatureContentGate::AccountRestricted => (
                        "Mature content unavailable",
                        "This mature content is not available to your account.",
                    ),
                    fluxer_core::MatureContentGate::Unavailable => (
                        "Content unavailable",
                        "The content warning scope is unavailable. Try again when the channel information has loaded.",
                    ),
                    _ => (
                        "Content warning",
                        "This channel may contain mature or sensitive content. Proceed only if you wish to view it.",
                    ),
                };
                let mut view = div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .text_color(rgb(TEXT))
                    .child(div().text_size(px(20.)).child(title))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(MUTED))
                            .child(description),
                    );
                if let Some(error) = &self.interface.server_error {
                    view = view.child(div().text_size(px(12.)).child(error.clone()));
                }
                if let fluxer_core::MatureContentGate::Consent { field, id } = gate {
                    view = view.child(
                        div()
                            .id("mature-content-proceed")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(ACCENT))
                            .cursor_pointer()
                            .child("Proceed")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.save_display_setting(
                                    fluxer_core::DisplaySetting::MatureConsent { field, id },
                                    cx,
                                );
                            })),
                    );
                }
                return view.into_any_element();
            }
        }
        if channel
            .is_some_and(|channel| matches!(channel.kind, ChannelType::Forum | ChannelType::Media))
        {
            return self.forum_view(cx);
        }
        let has_guild_members =
            channel.is_some_and(|channel| channel.guild_id.is_some() && !channel.kind.is_thread());
        let voice_channel = channel
            .filter(|channel| channel.kind.is_voice())
            .map(|channel| channel.id);
        let title = channel
            .map(|c| format!("{} {}", if c.kind.is_voice() { "◉" } else { "#" }, c.name))
            .unwrap_or("Choose a channel".into());
        let selected = self.state.selected_channel;
        let thread_parent = channel
            .filter(|channel| channel.kind.is_thread())
            .and_then(|channel| channel.parent_id)
            .and_then(|id| self.state.channels.get(&id));
        let thread_parent = thread_parent
            .filter(|parent| {
                parent.kind.standalone_thread_type().is_some()
                    || matches!(parent.kind, ChannelType::Forum | ChannelType::Media)
            })
            .map(|parent| (parent.id, parent.name.clone()));
        let private_call = channel
            .filter(|channel| {
                !self.demo
                    && channel.guild_id.is_none()
                    && matches!(channel.kind, ChannelType::Dm | ChannelType::GroupDm)
            })
            .map(|channel| channel.id);
        let group_add = channel
            .filter(|channel| {
                !self.demo && channel.kind == ChannelType::GroupDm && channel.guild_id.is_none()
            })
            .map(|channel| channel.id);
        let archived = channel.is_some_and(|channel| channel.thread_archived());
        let locked = channel.is_some_and(|channel| channel.thread_locked());
        let membership_action = channel
            .filter(|channel| {
                !self.demo
                    && channel.kind.is_thread()
                    && channel.kind != ChannelType::PrivateThread
                    && !channel.thread_archived()
                    && self.managing_thread.is_none()
            })
            .zip(self.state.current_user.as_ref())
            .filter(|(channel, user)| {
                let permissions = self.state.permissions_for(channel.id, user.id);
                self.state
                    .thread_guilds
                    .contains(&channel.guild_id.unwrap_or_default())
                    && permissions.contains(
                        fluxer_core::Permissions::VIEW_CHANNEL
                            | fluxer_core::Permissions::READ_MESSAGE_HISTORY,
                    )
                    && (!channel.thread_locked()
                        || permissions.contains(fluxer_core::Permissions::MANAGE_THREADS)
                        || channel
                            .extra
                            .get("member")
                            .is_some_and(|value| !value.is_null()))
            })
            .map(|(channel, _)| {
                (
                    channel.id,
                    channel
                        .extra
                        .get("member")
                        .is_none_or(|value| value.is_null()),
                )
            });
        let manage_thread = channel
            .filter(|channel| !self.demo && channel.kind.is_thread())
            .filter(|channel| {
                channel
                    .guild_id
                    .and_then(|id| self.state.guilds.get(&id))
                    .zip(self.state.current_user.as_ref())
                    .is_some_and(|(guild, user)| guild.owner_id == user.id)
            })
            .map(|channel| channel.id)
            .filter(|_| self.managing_thread.is_none());
        let can_browse_archived = channel
            .filter(|channel| !self.demo && channel.kind.standalone_thread_type().is_some())
            .zip(self.state.current_user.as_ref())
            .filter(|(channel, user)| {
                channel
                    .guild_id
                    .is_some_and(|guild| self.state.thread_guilds.contains(&guild))
                    && self.state.permissions_for(channel.id, user.id).contains(
                        fluxer_core::Permissions::VIEW_CHANNEL
                            | fluxer_core::Permissions::READ_MESSAGE_HISTORY,
                    )
            })
            .map(|(channel, _)| channel.id);
        let can_create_thread = selected.filter(|parent| {
            !self.demo
                && (self.state.public_thread_type(*parent).is_some()
                    || self.state.private_thread_allowed(*parent))
        });
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
        self.message_content.retain_documents(&retained);
        for message in messages
            .iter()
            .filter(|message| !pin_system_message::is_pin_system_message(message))
        {
            let document = self.message_document(message, message_markup::MessageContext::Standard);
            let document = if fluxer_core::compact_messages(&self.state.user_settings)
                && !document.text.is_empty()
            {
                Arc::new(document.with_author_name(
                    message,
                    &self.streaming().name(message.author.display_name()),
                ))
            } else {
                document
            };
            if let Some(input) = self.message_text.get(&message.id) {
                input.update(cx, |input, cx| {
                    input.set_emoji_reduced_motion(self.interface.reduced_motion());
                    input.update_message_document(document, cx);
                });
            } else {
                let input = self.message_text_input(
                    document,
                    message,
                    Some(self.timeline.clone()),
                    window,
                    cx,
                );
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
                    .child(div().font_weight(FontWeight::SEMIBOLD).min_w_0().child(crate::emoji_sprites::unicode_text(&title, 18., window.scale_factor() > 1., cx)))
                    .when_some(self.state.selected_channel.and_then(|id| self.state.channels.get(&id)).and_then(|c| c.topic.clone()).filter(|t| !t.is_empty()), |view, topic| view.child(div().flex_1().min_w_0().px_3().child(topic)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when_some(private_call, |view, channel| view.child(
                                quiet_button("start-private-call", "Start voice call").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.command(Command::StartPrivateCall { channel_id: channel }, cx);
                                        this.pane = Pane::Call;
                                        cx.notify();
                                    }),
                                ),
                            ))
                            .when_some(group_add, |view, channel| view.child(
                                quiet_button("group-add-friends", "Add friends").on_click(
                                    cx.listener(move |this, _, _, cx| this.add_friends_to_group(channel, cx)),
                                ),
                            ))
                            .when_some(thread_parent, |view, (parent, name)| view.child(
                                icon_button("thread-parent", Icon::Back, format!("Back to #{name}")).on_click(
                                    cx.listener(move |this, _, _, cx| this.select(parent, cx)),
                                ),
                            ))
                            .when_some(membership_action, |view, (thread, join)| view.child(
                                quiet_button("thread-membership", if join { "Join thread" } else { "Leave thread" })
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_thread_membership(thread, join, cx))),
                            ))
                            .when_some(manage_thread, |view, thread| view
                                .child(quiet_button("archive-thread", if archived { "Unarchive thread" } else { "Archive thread" })
                                    .on_click(cx.listener(move |this, _, _, cx| this.manage_thread(thread, Some(!archived), None, cx))))
                                .child(quiet_button("lock-thread", if locked { "Unlock thread" } else { "Lock thread" })
                                    .on_click(cx.listener(move |this, _, _, cx| this.manage_thread(thread, if archived { Some(false) } else { None }, Some(!locked), cx))))
                            )
                            .when_some(can_browse_archived, |view, parent| view.child(
                                quiet_button("browse-archived-threads", "Archived threads").on_click(
                                    cx.listener(move |this, _, _, cx| this.load_archived_threads(parent, None, cx)),
                                ),
                            ))
                            .when_some(can_create_thread, |view, parent| view.child(
                                quiet_button("new-thread", "New thread").on_click(
                                    cx.listener(move |this, _, window, cx| this.open_thread_form(parent, window, cx)),
                                ),
                            ))
                            .when(has_guild_members || group_add.is_some(), |view| view.child(
                                quiet_button("members-toggle", "Members").on_click(cx.listener(|this, _, _, cx| {
                                    this.members_open = !this.members_open;
                                    this.members_request_generation = this.members_request_generation.wrapping_add(1);
                                    this.members_start = 0;
                                    this.members_identity = if this.members_open {
                                        this.state.selected_channel.and_then(|id| this.state.channels.get(&id)).and_then(|channel| channel.guild_id.and_then(|guild| this.state.guilds.get(&guild).map(|guild| fluxer_core::member_list_identity(guild,channel))))
                                    } else { None };
                                    this.members_scroll = UniformListScrollHandle::new();
                                    this.command(Command::SetMemberList { channel_id: if this.members_open { this.state.selected_channel } else { None }, start: 0 }, cx);
                                    cx.notify();
                                }))
                            ))
                            .child(self.inbox_button(cx))
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
            .when_some(can_browse_archived, |view, parent| view.child(self.render_archived_threads(parent, cx)))
            .when(self.thread_form.is_some() && self.thread_form == can_create_thread, |view| {
                view.child(self.render_thread_form(cx))
            })
            .when(archived, |view| {
                view.child(div().px_5().py_2().text_color(rgb(MUTED)).child("Archived thread"))
            })
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
            .when(self.pane != Pane::Call && !self.available_cameras.is_empty(), |view| {
                view.child(self.camera_strip(cx))
            })
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
                                } else if fluxer_core::compact_messages(&self.state.user_settings) {
                                    div().w(px(40.)).flex_shrink_0().text_size(px(11.)).text_color(rgb(MUTED)).child(timestamp_label.clone()).into_any_element()
                                } else if layout.starts_group {
                                    div().w(px(COZY_AVATAR_SIZE)).flex_shrink_0().pt(px(2.))
                                        .child(self.profile_trigger(div().id(("profile-avatar",id.0)).size(px(COZY_AVATAR_SIZE)).child(rich_message::avatar(&message.author,self.media_endpoint.as_deref(),COZY_AVATAR_SIZE)),message.author.clone(),self.state.channels.get(&message.channel_id).and_then(|channel|channel.guild_id),crate::profile_triggers::message_is_webhook(&message),cx))
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
                                        .py(px(if fluxer_core::compact_messages(&self.state.user_settings) { 0. } else { COZY_SPACING_Y }))
                                        .text_size(px(fluxer_core::message_font_size(&self.state.user_settings)))
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
                                        .line_height(px(fluxer_core::message_font_size(&self.state.user_settings) * 1.375))
                                        .child(gutter)
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .flex_col()
                                                .gap_0()
                                                .when((layout.starts_group || fluxer_core::compact_messages(&self.state.user_settings)) && (!fluxer_core::compact_messages(&self.state.user_settings) || !has_text) && !is_pin, |body| {
                                                    body.child(
                                            div()
                                                .h(px(COZY_LINE_HEIGHT))
                                                .flex()
                                                .gap_2()
                                                .items_center()
                                                .child(
                                                    div().id(("profile-name",id.0)).font_weight(FontWeight::SEMIBOLD).child(
                                                        crate::emoji_sprites::unicode_text(&self.streaming().name(message.author.display_name()), 14., window.scale_factor() > 1., cx),
                                                    ).map(|view|self.profile_trigger(view,message.author.clone(),self.state.channels.get(&message.channel_id).and_then(|channel|channel.guild_id),crate::profile_triggers::message_is_webhook(&message),cx)),
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
                                                            .items_start()
                                                            .gap_1()
                                                            .w_full()
                                                            .min_w_0()
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .min_w_0()
                                                                    .line_height(px(fluxer_core::message_font_size(&self.state.user_settings) * 1.375))
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
                                                            autoplay: media_animation::effective_gif_autoplay(&self.state.user_settings, self.interface.reduced_motion()),
                                                            surface: "timeline",
                                                            redaction: streamer_mode::Redaction { effects: self.streaming(), invite: self.message_links.invite_endpoint() },
 hide_downloads: crate::media_context_policy::hide_media_downloads(message.channel_id, crate::media_context_policy::MediaKind::Image, &self.state.channels),
                                                        },
                                                        &self.revealed_media,
                                                        &mut self.playback,
                                                        window,
                                                        cx,
                                                    ))
                                                })
                                                .child(self.sticker_message(&message))
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
                                    .when(layout.space_before > 0 && !fluxer_core::compact_messages(&self.state.user_settings), |entry| {
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
                    .when(self.gif_picker.open, |view| {
                        view.child(div().flex().justify_end().child(self.gif_picker_panel(cx)))
                    })
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
                                                    &format!("Replying to {} · {}", self.streaming().name(message.author.display_name()), self.streamer_preview(message)),
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
                    .when(self.sticker_picker_open, |view| view.child(self.sticker_picker(cx)))
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
                            .child(quiet_button("composer-sticker", "Stickers").on_click(cx.listener(|this, _, _, cx| { this.sticker_picker_open = !this.sticker_picker_open; if this.sticker_picker_open { this.gif_picker.reset(); this.emoji_target = None; } cx.notify(); })))
                            .child(div().flex_1().min_w_0().child(self.composer.clone()))
                            .when(self.gif_picker_available(), |view| {
                                view.child(quiet_button("composer-gif", "GIF").on_click(cx.listener(
                                    |this, _, window, cx| this.toggle_gif_picker(window, cx),
                                )))
                            })
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
                                    if self.slowmode_remaining() > 0 {
                                        "Slowmode active"
                                    } else if self.message_pending() {
                                        "Sending…"
                                    } else {
                                        "Send message"
                                    },
                                )
                                .when(self.message_pending() || self.slowmode_remaining() > 0, |button| button.opacity(0.5))
                                .text_color(rgb(ACCENT))
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.send_from_composer(window, cx),
                                )),
                            ),
                    )
                    .when(self.pane != Pane::Call, |view| {
                        view.child(
                            div()
                                .px_2()
                                .text_size(px(10.))
                                .text_color(rgb(MUTED))
                                .child(if self.slowmode_remaining() > 0 { format!("Slowmode · {}:{:02} remaining",self.slowmode_remaining()/60,self.slowmode_remaining()%60) } else { "Enter to send · Shift+Enter for a new line".into() }),
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

impl Fluxer {
    fn load_more_forum(&mut self, channel_id: Snowflake, offset: usize, cx: &mut Context<Self>) {
        let Some(forum) = self.forum.as_mut() else {
            return;
        };
        if forum.loading_more || !forum.has_more || forum.next_offset != offset {
            return;
        }
        forum.loading_more = true;
        forum.error = None;
        self.command(
            Command::LoadForum {
                channel_id,
                creation_order: self.forum_creation_order,
                offset,
                tags: self.forum_filter_tags.clone(),
                match_all: self.forum_match_all,
            },
            cx,
        );
    }

    fn reload_forum(&mut self, channel_id: Snowflake, cx: &mut Context<Self>) {
        self.forum = None;
        self.command(
            Command::LoadForum {
                channel_id,
                creation_order: self.forum_creation_order,
                offset: 0,
                tags: self.forum_filter_tags.clone(),
                match_all: self.forum_match_all,
            },
            cx,
        );
    }

    fn forum_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(channel_id) = self.state.selected_channel else {
            return div().into_any_element();
        };
        let title = self.channel_name(channel_id);
        let can_post = !self.demo
            && self.state.channels.get(&channel_id).is_some_and(|channel| {
                matches!(channel.kind, ChannelType::Forum | ChannelType::Media)
                    && channel
                        .guild_id
                        .is_some_and(|guild| self.state.thread_guilds.contains(&guild))
                    && self.state.current_user.as_ref().is_some_and(|user| {
                        self.state.permissions_for(channel_id, user.id).contains(
                            fluxer_core::Permissions::VIEW_CHANNEL
                                | fluxer_core::Permissions::CREATE_PUBLIC_THREADS,
                        )
                    })
            });
        let mut list = div()
            .id("forum-posts")
            .flex_1()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_2();
        if let Some(ForumResults {
            posts,
            has_more,
            next_offset,
            loading_more,
            error,
        }) = &self.forum
        {
            if let Some(error) = error {
                list = list.child(div().child(error.clone()));
            }
            if posts.is_empty() && error.is_none() {
                list = list.child(div().child("No posts found."));
            }
            for (post, preview) in posts {
                let id = post.id;
                let text = preview
                    .as_ref()
                    .map(|message| message.content.clone())
                    .unwrap_or_default();
                list = list.child(
                    div()
                        .id(("forum-post", id.0))
                        .p_3()
                        .bg(rgb(0x303136))
                        .rounded_md()
                        .cursor_pointer()
                        .focusable()
                        .tab_stop(true)
                        .focus(|style| style.bg(rgb(0x484054)))
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space")
                                && !event.keystroke.modifiers.modified()
                            {
                                cx.stop_propagation();
                                if !event.is_held {
                                    this.select(id, cx);
                                }
                            }
                        }))
                        .child(div().child(post.display_name()))
                        .when(!text.is_empty(), |view| {
                            view.child(div().text_sm().text_color(rgb(0xb5bac1)).child(text))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.select(id, cx))),
                );
            }
            if *has_more {
                let offset = *next_offset;
                let loading = *loading_more;
                list = list.child(
                    quiet_button("forum-more", if loading { "Loading…" } else { "Load more" })
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space")
                                && !event.keystroke.modifiers.modified()
                            {
                                cx.stop_propagation();
                                if !event.is_held {
                                    this.load_more_forum(channel_id, offset, cx);
                                }
                            }
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.load_more_forum(channel_id, offset, cx);
                        })),
                );
            }
        } else {
            list = list.child(div().child("Loading posts…"));
        }
        let mut filters = div().p_2().flex().flex_wrap().gap_2();
        if let Some(tags) = self
            .state
            .channels
            .get(&channel_id)
            .and_then(|channel| channel.extra.get("available_tags"))
            .and_then(serde_json::Value::as_array)
        {
            for (index, tag) in tags.iter().enumerate() {
                let Some(id) = tag.get("id").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let id = id.to_owned();
                let name = tag
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Tag");
                let label = format!(
                    "{} {}",
                    if self.forum_filter_tags.contains(&id) {
                        "☑"
                    } else {
                        "☐"
                    },
                    name
                );
                filters = filters.child(quiet_button(("forum-filter-tag", index), label).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if this.forum_filter_tags.contains(&id) {
                            this.forum_filter_tags.retain(|tag| tag != &id);
                        } else {
                            this.forum_filter_tags.push(id.clone());
                        }
                        this.reload_forum(channel_id, cx);
                    }),
                ));
            }
            if !tags.is_empty() {
                filters = filters.child(
                    quiet_button(
                        "forum-filter-mode",
                        if self.forum_match_all {
                            "Match all tags"
                        } else {
                            "Match any tag"
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.forum_match_all = !this.forum_match_all;
                        this.reload_forum(channel_id, cx);
                    })),
                );
                filters = filters.child(quiet_button("forum-filter-reset", "Clear tags").on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.forum_filter_tags.clear();
                        this.reload_forum(channel_id, cx);
                    }),
                ));
            }
        }
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(div().p_4().child(format!("# {title}")))
            .child(filters)
            .child(
                div().p_2().child(
                    quiet_button(
                        "forum-sort",
                        if self.forum_creation_order {
                            "Sort: newest"
                        } else {
                            "Sort: recent activity"
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.forum_creation_order = !this.forum_creation_order;
                        this.forum = None;
                        this.command(
                            Command::LoadForum {
                                channel_id,
                                creation_order: this.forum_creation_order,
                                offset: 0,
                                tags: this.forum_filter_tags.clone(),
                                match_all: this.forum_match_all,
                            },
                            cx,
                        );
                    })),
                ),
            )
            .when(can_post && self.thread_form != Some(channel_id), |view| {
                view.child(
                    quiet_button("new-forum-post", "New post").on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.forum_tags.clear();
                            this.forum_body
                                .update(cx, |input, cx| input.set(String::new(), cx));
                            this.open_thread_form(channel_id, window, cx);
                        },
                    )),
                )
            })
            .when(can_post && self.thread_form == Some(channel_id), |view| {
                view.child(self.render_forum_form(channel_id, cx))
            })
            .child(list)
            .into_any_element()
    }
}
