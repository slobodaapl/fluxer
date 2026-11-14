// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared selectable conversation and preview documents, cached independently
//! of voice/video polling. Message commands continue to use the original body.
use super::*;
use crate::{
    controls::{Icon, icon, quiet_button},
    message_markup::{MessageCache, MessageContext, MessageDocument},
};

#[derive(Default)]
pub(super) struct MessageContent {
    pub cache: MessageCache,
    replies: BTreeMap<(Snowflake, Snowflake), Entity<TextInput>>,
}
impl MessageContent {
    pub fn reset(&mut self) {
        self.cache.clear();
        chat_helpers::clear_preview_cache();
        self.replies.clear();
    }
    pub fn retain_replies(&mut self, channel: Option<Snowflake>, visible: &BTreeSet<Snowflake>) {
        self.replies
            .retain(|(c, id), _| Some(*c) == channel && visible.contains(id));
    }
}
impl Fluxer {
    pub(super) fn message_document(
        &mut self,
        message: &Message,
        context: MessageContext,
    ) -> Arc<MessageDocument> {
        let mut rendering = message.clone();
        rendering.content = chat_helpers::display_text(message);
        self.message_content.cache.get(
            &rendering,
            &self.state,
            self.media_endpoint.as_deref(),
            context,
        )
    }
    pub(super) fn message_text_input(
        &self,
        document: Arc<MessageDocument>,
        channel: Snowflake,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextInput> {
        let input = cx.new(|cx| TextInput::read_only_document(document, cx));
        cx.subscribe_in(
            &input,
            window,
            move |this, _, event: &input::MessageActivated, window, cx| {
                if let message_markup::MessageTarget::User(id) = event.target {
                    let guild = this
                        .state
                        .channels
                        .get(&channel)
                        .and_then(|channel| channel.guild_id);
                    let user = this.profile_user(id, &id.to_string());
                    this.open_user_profile(user, guild, false, window.mouse_position(), window, cx);
                } else {
                    this.activate_message_target(&event.target, window, cx);
                }
            },
        )
        .detach();
        input
    }
    pub(super) fn message_reply_preview(
        &mut self,
        message: &Message,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if crate::pin_system_message::is_pin_system_message(message) {
            return None;
        }
        let reference = message.message_reference.as_ref()?;
        if reference.kind == 1 {
            return Some(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child(icon(Icon::Forward, 14.))
                    .child("Forwarded message")
                    .into_any_element(),
            );
        }
        let Some(mut source) = chat_helpers::referenced_message(&self.state, message) else {
            return Some(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child(icon(Icon::Reply, 14.))
                    .child("Original message unavailable")
                    .into_any_element(),
            );
        };
        if chat_helpers::display_text(&source).is_empty() {
            source.content = if source.attachments.is_empty() {
                "[Message]"
            } else {
                "[Attachment]"
            }
            .into();
        }
        let document = self.message_document(&source, MessageContext::InlineReply);
        let key = (message.channel_id, message.id);
        let input = if let Some(input) = self.message_content.replies.get(&key) {
            input.update(cx, |input, cx| input.update_message_document(document, cx));
            input.clone()
        } else {
            let input = self.message_text_input(document, source.channel_id, window, cx);
            self.message_content.replies.insert(key, input.clone());
            input
        };
        let name = source.author.display_name().to_owned();
        Some(
            div()
                .flex()
                .items_center()
                .gap_1()
                .min_w_0()
                .h(px(22.))
                .overflow_hidden()
                .child(icon(Icon::Reply, 14.))
                .child(
                    quiet_button(("reply-source", message.id.0), "")
                        .child(crate::emoji_sprites::unicode_text(
                            &name,
                            12.,
                            window.scale_factor() > 1.,
                            cx,
                        ))
                        .py_0()
                        .h(px(20.))
                        .line_height(px(18.))
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .max_w(px(140.))
                        .overflow_hidden()
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.jump_message(&source, cx)),
                        ),
                )
                .child(div().flex_1().min_w_0().child(input))
                .into_any_element(),
        )
    }
}
