// SPDX-License-Identifier: AGPL-3.0-or-later
//! Avatar/name activation stays separate from surrounding message/call actions.
use super::*;
use fluxer_core::User;
use std::{cell::Cell, rc::Rc};

pub(super) fn trigger(
    view: Stateful<Div>,
    user: User,
    guild: Option<Snowflake>,
    webhook: bool,
    cx: &mut Context<Fluxer>,
) -> Stateful<Div> {
    let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
    let painted = bounds.clone();
    let probe = std::env::var("FLUXER_PROFILE_DEMO").is_ok_and(|mode| mode == "hitboxes")
        && std::env::args().any(|arg| arg == "--demo");
    let probe_id = user.id;
    let click_user = user.clone();
    let click_bounds = bounds.clone();
    view.relative()
        .cursor_pointer()
        .focusable()
        .tab_stop(true)
        .child(
            canvas(
                move |bounds, _, _| {
                    if probe && bounds != painted.get() {
                        eprintln!(
                            "PROFILE_HITBOX {} {} {} {} {}",
                            probe_id,
                            f32::from(bounds.origin.x),
                            f32::from(bounds.origin.y),
                            f32::from(bounds.size.width),
                            f32::from(bounds.size.height)
                        );
                    }
                    painted.set(bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0()
            .size_full(),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            let b = click_bounds.get();
            this.open_user_profile(
                click_user.clone(),
                guild,
                webhook,
                point(b.right() + px(8.), b.top()),
                window,
                cx,
            );
        }))
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                cx.stop_propagation();
                if !event.is_held {
                    let b = bounds.get();
                    this.open_user_profile(
                        user.clone(),
                        guild,
                        webhook,
                        point(b.right() + px(8.), b.top()),
                        window,
                        cx,
                    );
                }
            }
        }))
        .on_key_up(|event: &KeyUpEvent, _, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                cx.stop_propagation();
            }
        })
}

impl Fluxer {
    pub(super) fn profile_user(&self, id: Snowflake, name: &str) -> User {
        self.state
            .users
            .get(&id)
            .cloned()
            .or_else(|| {
                self.state
                    .current_user
                    .as_ref()
                    .filter(|user| user.id == id)
                    .cloned()
            })
            .unwrap_or_else(|| User {
                id,
                username: name.into(),
                ..Default::default()
            })
    }
    pub(super) fn profile_trigger(
        &self,
        view: Stateful<Div>,
        user: User,
        guild: Option<Snowflake>,
        webhook: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        trigger(view, user, guild, webhook, cx)
    }
}
pub(super) fn message_is_webhook(message: &Message) -> bool {
    message
        .extra
        .get("webhook_id")
        .is_some_and(|id| !id.is_null())
}
