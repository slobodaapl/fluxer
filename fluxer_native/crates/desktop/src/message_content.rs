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
    mention_users: BTreeMap<(Snowflake, Snowflake), Vec<fluxer_core::User>>,
}
impl MessageContent {
    fn update_mentions(
        &mut self,
        channel: Snowflake,
        message: Snowflake,
        users: &[fluxer_core::User],
    ) {
        let key = (channel, message);
        if users.is_empty() {
            self.mention_users.remove(&key);
        } else {
            self.mention_users.insert(key, users.to_vec());
        }
    }
    fn mentioned_user(
        &self,
        channel: Snowflake,
        message: Snowflake,
        id: Snowflake,
    ) -> Option<fluxer_core::User> {
        self.mention_users
            .get(&(channel, message))
            .and_then(|users| users.iter().find(|user| user.id == id))
            .cloned()
    }
    pub fn reset(&mut self) {
        self.cache.clear();
        chat_helpers::clear_preview_cache();
        self.replies.clear();
        self.mention_users.clear();
    }
    pub fn retain_documents(&mut self, retained: &BTreeSet<(Snowflake, Snowflake)>) {
        self.cache.retain_visible(retained);
        self.mention_users.retain(|key, _| retained.contains(key));
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
        self.message_content
            .update_mentions(message.channel_id, message.id, &message.mentions);
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
        message: &Message,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextInput> {
        let channel = message.channel_id;
        let message_id = message.id;
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
                    let user = this
                        .state
                        .users
                        .get(&id)
                        .cloned()
                        .or_else(|| this.message_content.mentioned_user(channel, message_id, id))
                        .or_else(|| {
                            this.state
                                .messages
                                .values()
                                .flat_map(|messages| messages.values())
                                .flat_map(|message| message.mentions.iter())
                                .find(|user| user.id == id)
                                .cloned()
                        })
                        .or_else(|| {
                            this.state
                                .current_user
                                .as_ref()
                                .filter(|user| user.id == id)
                                .cloned()
                        });
                    if let Some(user) = user {
                        this.open_user_profile(
                            user,
                            guild,
                            false,
                            event
                                .anchor
                                .map(|bounds| point(bounds.right(), bounds.top()))
                                .unwrap_or_else(|| window.mouse_position()),
                            window,
                            cx,
                        );
                    }
                } else if let message_markup::MessageTarget::Role(id) = event.target {
                    this.open_role_mention(id, channel, event.anchor, window, cx);
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
            let input = self.message_text_input(document, &source, window, cx);
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

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn updated_message_replaces_mention_snapshot_without_replacing_input() {
        let mut content = MessageContent::default();
        let user = fluxer_core::User {
            id: Snowflake(3),
            username: "old".into(),
            ..Default::default()
        };
        content.update_mentions(Snowflake(1), Snowflake(2), &[user.clone()]);
        let mut updated = user;
        updated.username = "new".into();
        content.update_mentions(Snowflake(1), Snowflake(2), &[updated]);
        assert_eq!(
            content
                .mentioned_user(Snowflake(1), Snowflake(2), Snowflake(3))
                .unwrap()
                .username,
            "new"
        );
        content.update_mentions(Snowflake(1), Snowflake(2), &[]);
        assert!(
            content
                .mentioned_user(Snowflake(1), Snowflake(2), Snowflake(3))
                .is_none()
        );
    }
}
