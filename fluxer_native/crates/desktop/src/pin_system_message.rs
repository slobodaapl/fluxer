// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source: PinSystemMessage.tsx and SystemMessage.tsx. A pin announcement is a
//! system event; its reference supplies a navigation ID, never a reply preview.
use super::*;
use crate::controls::{Icon, icon};

pub(super) fn is_pin_system_message(message: &Message) -> bool {
    message.kind == 6 // MessageTypes.CHANNEL_PINNED_MESSAGE
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PinAction {
    Jump {
        channel: Snowflake,
        message: Snowflake,
    },
    Pins {
        channel: Snowflake,
    },
}

fn jump_action(message: &Message) -> Option<PinAction> {
    if !is_pin_system_message(message) {
        return None;
    }
    // The source intentionally jumps within the announcement's channel, even
    // when a legacy reference carries a different channel/guild or no body.
    message
        .message_reference
        .as_ref()?
        .message_id
        .map(|id| PinAction::Jump {
            channel: message.channel_id,
            message: id,
        })
}

fn pins_action(message: &Message) -> Option<PinAction> {
    is_pin_system_message(message).then_some(PinAction::Pins {
        channel: message.channel_id,
    })
}

fn author_name(state: &ClientState, message: &Message) -> String {
    let user = state
        .users
        .get(&message.author.id)
        .unwrap_or(&message.author);
    state
        .channels
        .get(&message.channel_id)
        .and_then(|channel| channel.guild_id)
        .and_then(|guild| state.members.get(&(guild, user.id)))
        .and_then(|member| member.nick.as_deref())
        .filter(|nick| !nick.is_empty())
        .unwrap_or_else(|| user.display_name())
        .to_owned()
}

impl Fluxer {
    fn activate_pin_action(&mut self, action: PinAction, cx: &mut Context<Self>) {
        match action {
            PinAction::Jump { channel, message } => {
                // Existing navigation owns history fetching, missing/deleted
                // targets, permission errors, centering, and acknowledgment.
                self.follow_message_channel(channel, None, Some(message), cx);
            }
            PinAction::Pins { channel } => self.open_pins_for(channel, cx),
        }
    }

    pub(super) fn pin_system_message(
        &self,
        message: &Message,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !is_pin_system_message(message) || !self.state.channels.contains_key(&message.channel_id)
        {
            return None;
        }
        let jump = jump_action(message);
        let pins = pins_action(message);
        let author =
            emoji_sprites::unicode_text(&author_name(&self.state, message), 14., false, cx);
        let link = |suffix: &'static str, label: &'static str, action: Option<PinAction>| {
            div()
                .id((suffix, message.id.0))
                .focusable()
                .tab_stop(true)
                .cursor_pointer()
                .text_color(rgb(0xa999ff))
                .hover(|style| style.underline())
                .focus(|style| style.underline().bg(rgb(0x3b3549)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some(action) = action {
                        this.activate_pin_action(action, cx);
                    }
                }))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space")
                        && !event.keystroke.modifiers.modified()
                    {
                        cx.stop_propagation();
                        if !event.is_held
                            && let Some(action) = action
                        {
                            this.activate_pin_action(action, cx);
                        }
                    }
                }))
                .child(label)
        };
        Some(
            div()
                .id(("pin-system-message", message.id.0))
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(px(4.))
                .text_color(rgb(MUTED))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT))
                        .child(author),
                )
                .child("pinned")
                .child(link("pin-message-link", "a message", jump))
                .child("to this channel. See")
                // Keep punctuation attached when the links wrap onto a new line.
                .child(
                    div()
                        .flex()
                        .child(link("pin-all-link", "all pinned messages", pins))
                        .child("."),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .child(message_actions::message_time(&message.timestamp)),
                )
                .into_any_element(),
        )
    }
}

pub(super) fn gutter() -> AnyElement {
    div()
        .w(px(crate::message_grouping::COZY_AVATAR_SIZE))
        .h(px(crate::message_grouping::COZY_LINE_HEIGHT))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(MUTED))
        .child(icon(Icon::Pin, 16.))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{PinAction, author_name, is_pin_system_message, jump_action, pins_action};
    use fluxer_core::{ClientState, Message, Snowflake};

    fn announcement() -> Message {
        serde_json::from_value(serde_json::json!({
            "id":"30", "channel_id":"20", "author":{"id":"10","username":"Original"},
            "type":6, "content":"", "message_reference":{"channel_id":"99","guild_id":"98","message_id":"1"},
            "referenced_message":null
        })).unwrap()
    }

    #[test]
    fn system_type_routes_independently_of_pinned_flag_and_reference() {
        let mut message = announcement();
        assert!(is_pin_system_message(&message));
        message.message_reference = None;
        assert!(is_pin_system_message(&message));
        for kind in [0, 19, 7, 99] {
            message.kind = kind;
            message.pinned = true;
            assert!(!is_pin_system_message(&message));
            assert_eq!(jump_action(&message), None);
            assert_eq!(pins_action(&message), None);
        }
    }

    #[test]
    fn historical_or_deleted_target_uses_reference_id_in_announcement_channel() {
        let message = announcement();
        assert_eq!(
            jump_action(&message),
            Some(PinAction::Jump {
                channel: 20.into(),
                message: 1.into()
            })
        );
        assert_eq!(
            pins_action(&message),
            Some(PinAction::Pins { channel: 20.into() })
        );
        // Neither current selection nor cached reference bodies define links.
        let mut state = ClientState::default();
        state.selected_channel = Some(999.into());
        assert!(state.channel_messages(20.into()).next().is_none());
        assert_eq!(
            jump_action(&message),
            Some(PinAction::Jump {
                channel: 20.into(),
                message: 1.into()
            })
        );
    }

    #[test]
    fn missing_reference_is_noop_but_all_pins_remains_available() {
        let mut message = announcement();
        message.message_reference.as_mut().unwrap().message_id = None;
        assert_eq!(jump_action(&message), None);
        assert_eq!(
            pins_action(&message),
            Some(PinAction::Pins { channel: 20.into() })
        );
        message.message_reference = None;
        assert_eq!(jump_action(&message), None);
        assert!(pins_action(&message).is_some());
    }

    #[test]
    fn author_uses_live_user_and_member_nickname_with_historical_fallback() {
        let message = announcement();
        let mut state = ClientState::demo();
        assert_eq!(author_name(&state, &message), "Original");
        state.users.insert(
            message.author.id,
            fluxer_core::User {
                id: message.author.id,
                username: "Updated".into(),
                ..Default::default()
            },
        );
        assert_eq!(author_name(&state, &message), "Updated");
        let channel = state.channels.get(&Snowflake(11)).unwrap().clone();
        state.channels.insert(message.channel_id, channel);
        state.members.insert(
            (1.into(), message.author.id),
            fluxer_core::Member {
                nick: Some("Community name".into()),
                ..Default::default()
            },
        );
        assert_eq!(author_name(&state, &message), "Community name");
    }
}
