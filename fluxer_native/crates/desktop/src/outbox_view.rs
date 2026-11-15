// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local echoes are created in the Enter handler, before the worker or network responds.
use super::*;
use crate::controls::{Icon, icon, icon_button};
use fluxer_client::{AttachmentStatus, PendingMessage, PendingMessageStatus};

#[derive(Default)]
pub(super) struct OutboxUi {
    pub channels: BTreeMap<Snowflake, Vec<PendingMessage>>,
    fixture_started: bool,
    awaiting: BTreeSet<(Snowflake, u64)>,
    text: BTreeMap<(Snowflake, u64), Entity<TextInput>>,
}

impl OutboxUi {
    pub fn reset(&mut self) {
        self.channels.clear();
        self.awaiting.clear();
        self.text.clear();
    }

    pub fn insert(&mut self, channel: Snowflake, message: PendingMessage) {
        self.awaiting.insert((channel, message.id));
        self.channels.entry(channel).or_default().push(message);
    }

    pub fn update(&mut self, channel: Snowflake, mut messages: Vec<PendingMessage>) {
        // An older progress snapshot may already be queued when Enter creates
        // another echo. Keep that echo until its own command has been observed.
        self.awaiting
            .retain(|(c, id)| *c != channel || !messages.iter().any(|message| message.id == *id));
        if let Some(previous) = self.channels.get(&channel) {
            messages.extend(
                previous
                    .iter()
                    .filter(|message| self.awaiting.contains(&(channel, message.id)))
                    .cloned(),
            );
        }
        messages.sort_by_key(|message| message.id);
        self.text
            .retain(|(c, id), _| *c != channel || messages.iter().any(|message| message.id == *id));
        if messages.is_empty() {
            self.channels.remove(&channel);
        } else {
            self.channels.insert(channel, messages);
        }
    }

    pub fn owns_attachment(&self, channel: Snowflake, id: u64) -> bool {
        self.channels.get(&channel).is_some_and(|messages| {
            messages
                .iter()
                .any(|message| message.attachments.iter().any(|stage| stage.id == id))
        })
    }
}

impl Fluxer {
    /// Persistent local-only rows expose the transient delivery layout for review.
    pub(super) fn maybe_demo_outbox(&mut self, cx: &mut Context<Self>) {
        if !self.demo
            || self.outbox.fixture_started
            || self.state.current_user.is_none()
            || !std::env::args().any(|arg| arg == "--demo-outbox")
        {
            return;
        }
        let Some(channel) = self.state.selected_channel else {
            return;
        };
        self.outbox.fixture_started = true;
        let rows = [
            ("This pending message should read just like a normal chat message, with enough horizontal room for its words and Fluxer emoji 👋🏽 🎉.", PendingMessageStatus::Sending),
            ("An attachment is uploading while the next draft remains available.", PendingMessageStatus::Sending),
            ("A failed message should keep its normal text layout and offer a small retry control.", PendingMessageStatus::Failed("Offline example: connection interrupted".into())),
        ].into_iter().enumerate().map(|(index, (content, status))| {
            let attachments = if index == 1 { vec![fluxer_client::StagedAttachment {
                id: 900_000, filename: "example-image.png".into(), size: 2048, mime_type: "image/png".into(), spoiler: false,
                status: AttachmentStatus::Uploading, progress: 0.42, error: None, preview: None,
            }] } else { vec![] };
            PendingMessage { id: fluxer_client::next_pending_message_id(), content: content.into(), attachments, status, reply: None }
        }).collect();
        self.outbox.update(channel, rows);
        self.timeline.scroll_to_bottom();
        cx.notify();
    }

    pub(super) fn send_optimistically(&mut self, content: String, cx: &mut Context<Self>) {
        let Some(channel) = self.state.selected_channel else {
            return;
        };
        let id = fluxer_client::next_pending_message_id();
        let reply = self.replies.get(&channel);
        let reply_to = reply.map(|(message, _)| message.id);
        let mention_author = reply.is_some_and(|(_, mention)| *mention);
        let command = Command::SendPendingMessage {
            id,
            channel_id: channel,
            content: content.clone(),
            reply_to,
            mention_author,
        };
        let result = self
            .controller
            .as_ref()
            .ok_or_else(|| "The native client worker is unavailable".to_string())
            .and_then(|controller| controller.send(command));
        if let Err(error) = result {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let attachments = self
            .attachments
            .channels
            .remove(&channel)
            .unwrap_or_default();
        let reference = reply_to.map(|message_id| fluxer_core::MessageReference {
            channel_id: channel,
            message_id: Some(message_id),
            guild_id: self.state.channels.get(&channel).and_then(|c| c.guild_id),
            kind: 0,
        });
        self.outbox.insert(
            channel,
            PendingMessage {
                id,
                content,
                attachments,
                status: PendingMessageStatus::Sending,
                reply: reference,
            },
        );
        self.replies.remove(&channel);
        self.state.drafts.remove(&channel);
        self.composer
            .update(cx, |input, cx| input.set(String::new(), cx));
        if self
            .search_ui
            .histories
            .get(&channel)
            .is_some_and(|history| !history.latest)
        {
            self.search_ui.return_to_latest_on_select(channel);
            self.search_ui.pending_center = None;
            self.search_ui.highlight = None;
            self.command(
                Command::ReturnToLatest {
                    channel_id: channel,
                },
                cx,
            );
        }
        self.timeline.scroll_to_bottom();
        self.error = None;
        cx.notify();
    }

    pub(super) fn pending_message_rows(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(channel) = self.state.selected_channel else {
            return vec![];
        };
        let Some(author) = self.state.current_user.clone() else {
            return vec![];
        };
        let messages = self
            .outbox
            .channels
            .get(&channel)
            .cloned()
            .unwrap_or_default();
        messages
            .into_iter()
            .filter(|pending| {
                // Gateway acceptance can arrive before the worker's completion
                // snapshot. An exact nonce hides the echo in either event order.
                let nonce = pending.id.to_string();
                !self.state.channel_messages(channel).any(|message| {
                    message.nonce.as_deref() == Some(nonce.as_str())
                        && message.author.id == author.id
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|pending| {
                let id = pending.id;
                let failed = matches!(pending.status, PendingMessageStatus::Failed(_));
                let message = Message {
                    id: Snowflake(id),
                    channel_id: channel,
                    author: author.clone(),
                    content: pending.content.clone(),
                    message_reference: pending.reply.clone(),
                    ..Default::default()
                };
                let text = if let Some(input) = self.outbox.text.get(&(channel, id)) {
                    input.clone()
                } else {
                    let document =
                        self.message_document(&message, message_markup::MessageContext::Standard);
                    let input = self.message_text_input(document, message.channel_id, window, cx);
                    self.outbox.text.insert((channel, id), input.clone());
                    input
                };
                let reply = self.message_reply_preview(&message, window, cx);
                let status = match &pending.status {
                    PendingMessageStatus::Failed(error) => format!("Couldn't send: {error}"),
                    PendingMessageStatus::Sending => {
                        if pending
                            .attachments
                            .iter()
                            .any(|stage| stage.status == AttachmentStatus::Preparing)
                        {
                            "Preparing attachments…".into()
                        } else if !pending.attachments.is_empty() {
                            let progress = pending
                                .attachments
                                .iter()
                                .map(|stage| stage.progress.clamp(0., 1.))
                                .sum::<f32>()
                                / pending.attachments.len() as f32;
                            if progress < 1. {
                                format!("Uploading… {:.0}%", progress * 100.)
                            } else {
                                "Sending…".into()
                            }
                        } else {
                            "Sending…".into()
                        }
                    }
                };
                div()
                    .id(("pending-message", id))
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .px(px(16.))
                    .py(px(message_grouping::COZY_SPACING_Y))
                    .line_height(px(message_grouping::COZY_LINE_HEIGHT))
                    .flex()
                    .gap(px(message_grouping::COZY_GUTTER))
                    .child(
                        div()
                            .w(px(message_grouping::COZY_AVATAR_SIZE))
                            .flex_shrink_0()
                            .child(rich_message::avatar(
                                &author,
                                self.media_endpoint.as_deref(),
                                message_grouping::COZY_AVATAR_SIZE,
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(
                                emoji_sprites::unicode_text(
                                    author.display_name(),
                                    14.,
                                    window.scale_factor() > 1.,
                                    cx,
                                ),
                            ))
                            .when_some(reply, |view, reply| view.child(reply))
                            .when(!pending.content.is_empty(), |view| {
                                view.child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .opacity(if failed { 1. } else { 0.65 })
                                        .child(text),
                                )
                            })
                            .when(!pending.attachments.is_empty(), |view| {
                                view.child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .children(pending.attachments.iter().map(|stage| {
                                            let image = if stage.spoiler {
                                                None
                                            } else {
                                                self.attachment_preview(channel, stage, cx)
                                            };
                                            div()
                                                .w(px(180.))
                                                .rounded_md()
                                                .overflow_hidden()
                                                .bg(rgb(PANEL))
                                                .border_1()
                                                .border_color(rgb(if failed {
                                                    0xa54c62
                                                } else {
                                                    0x41394d
                                                }))
                                                .flex()
                                                .flex_col()
                                                .when_some(image, |view, image| {
                                                    view.child(
                                                        img(image)
                                                            .w_full()
                                                            .h(px(100.))
                                                            .object_fit(ObjectFit::Contain),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .px_2()
                                                        .py_1()
                                                        .flex()
                                                        .items_center()
                                                        .gap_1()
                                                        .child(icon(
                                                            if stage.spoiler {
                                                                Icon::EyeOff
                                                            } else {
                                                                Icon::File
                                                            },
                                                            14.,
                                                        ))
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .text_size(px(12.))
                                                                .text_ellipsis()
                                                                .child(stage.filename.clone()),
                                                        ),
                                                )
                                                .when(!failed, |view| {
                                                    view.child(
                                                        div().h(px(2.)).bg(rgb(0x24202d)).child(
                                                            div()
                                                                .h_full()
                                                                .w(relative(
                                                                    stage.progress.clamp(0., 1.),
                                                                ))
                                                                .bg(rgb(ACCENT)),
                                                        ),
                                                    )
                                                })
                                        })),
                                )
                            })
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(px(11.))
                                            .text_color(rgb(if failed { 0xf18ca2 } else { MUTED }))
                                            .child(status),
                                    )
                                    .when(failed, |view| {
                                        view.child(
                                            icon_button(
                                                ("retry-pending", id),
                                                Icon::Retry,
                                                "Retry message",
                                            )
                                            .size(px(24.))
                                            .flex_shrink_0()
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    if this.slowmode_remaining() > 0 {
                                                        return;
                                                    }
                                                    if let Some(message) = this
                                                        .outbox
                                                        .channels
                                                        .get_mut(&channel)
                                                        .and_then(|messages| {
                                                            messages
                                                                .iter_mut()
                                                                .find(|message| message.id == id)
                                                        })
                                                    {
                                                        message.status =
                                                            PendingMessageStatus::Sending;
                                                    }
                                                    this.command(
                                                        Command::RetryPendingMessage {
                                                            channel_id: channel,
                                                            id,
                                                        },
                                                        cx,
                                                    );
                                                }),
                                            ),
                                        )
                                    })
                                    .child(
                                        icon_button(
                                            ("remove-pending", id),
                                            Icon::Close,
                                            if failed {
                                                "Remove failed message"
                                            } else {
                                                "Cancel sending message"
                                            },
                                        )
                                        .size(px(24.))
                                        .flex_shrink_0()
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.command(
                                                    Command::RemovePendingMessage {
                                                        channel_id: channel,
                                                        id,
                                                    },
                                                    cx,
                                                );
                                            }),
                                        ),
                                    ),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn pending(id: u64) -> PendingMessage {
        PendingMessage {
            id,
            content: "same content".into(),
            attachments: vec![],
            status: PendingMessageStatus::Sending,
            reply: None,
        }
    }

    #[test]
    fn older_worker_snapshot_cannot_remove_a_new_local_echo() {
        let channel = Snowflake(10);
        let mut ui = OutboxUi::default();
        ui.insert(channel, pending(1));
        ui.update(channel, vec![pending(1)]);
        ui.insert(channel, pending(2));
        ui.update(channel, vec![pending(1)]);
        assert_eq!(
            ui.channels[&channel]
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        ui.update(channel, vec![pending(1), pending(2)]);
        ui.update(channel, vec![pending(2)]);
        assert_eq!(ui.channels[&channel].len(), 1);
        assert_eq!(ui.channels[&channel][0].id, 2);
        ui.update(channel, vec![]);
        assert!(!ui.channels.contains_key(&channel));
    }
}
