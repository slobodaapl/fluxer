// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared overlay geometry and focus traversal. Callers own drawing and defer
//! only once: GPUI does not support deferred children inside deferred drawing.
use gpui::{Bounds, FocusHandle, Pixels, Size, Window, point, px, size};

fn finite(value: Pixels) -> f32 {
    let value = f32::from(value);
    if value.is_finite() { value } else { 0. }
}

/// Bound a surface to the viewport, even while a window is being minimized or
/// resized. Margins shrink with tiny viewports instead of pushing content out.
pub(super) fn surface_size(
    viewport: Size<Pixels>,
    preferred: Size<Pixels>,
    margin: Pixels,
) -> Size<Pixels> {
    let available = |extent: Pixels, requested: Pixels| {
        let extent = finite(extent).max(0.);
        let margin = finite(margin).max(0.).min(extent / 2.);
        px(finite(requested)
            .max(0.)
            .min((extent - margin * 2.).max(0.)))
    };
    size(
        available(viewport.width, preferred.width),
        available(viewport.height, preferred.height),
    )
}

/// Prefer right-start beside the actual trigger, flip left when it fits, then
/// clamp. All returned bounds stay inside the current window.
pub(super) fn popover_bounds(
    anchor: Bounds<Pixels>,
    viewport: Size<Pixels>,
    preferred: Size<Pixels>,
    margin: Pixels,
    gap: Pixels,
) -> Bounds<Pixels> {
    let fitted = surface_size(viewport, preferred, margin);
    let vw = finite(viewport.width).max(0.);
    let vh = finite(viewport.height).max(0.);
    let width = finite(fitted.width);
    let height = finite(fitted.height);
    let mx = finite(margin).max(0.).min((vw - width).max(0.) / 2.);
    let my = finite(margin).max(0.).min((vh - height).max(0.) / 2.);
    let right = finite(anchor.origin.x) + finite(anchor.size.width).max(0.) + finite(gap).max(0.);
    let left = finite(anchor.origin.x) - width - finite(gap).max(0.);
    let x = if right + width <= vw - mx {
        right
    } else if left >= mx {
        left
    } else {
        right
    };
    Bounds::new(
        point(
            px(x.clamp(mx, (vw - width - mx).max(mx))),
            px(finite(anchor.origin.y).clamp(my, (vh - height - my).max(my))),
        ),
        fitted,
    )
}

pub(super) fn cycle_focus(window: &mut Window, handles: &[FocusHandle], backwards: bool) {
    if handles.is_empty() {
        return;
    }
    let current = handles.iter().position(|handle| handle.is_focused(window));
    let next = match (current, backwards) {
        (Some(index), true) => (index + handles.len() - 1) % handles.len(),
        (Some(index), false) => (index + 1) % handles.len(),
        (None, true) => handles.len() - 1,
        (None, false) => 0,
    };
    handles[next].focus(window);
}

impl super::Fluxer {
    /// A new account/instance can reuse numeric IDs. Drop private projected
    /// documents and callback scopes on the context boundary itself.
    pub(super) fn reset_conversation_surfaces(&mut self, cx: &mut gpui::Context<Self>) {
        self.conversation_scope.synchronize("");
        self.composer
            .update(cx, |input, cx| input.set(String::new(), cx));
        self.search
            .update(cx, |input, cx| input.set(String::new(), cx));
        self.emoji_search
            .update(cx, |input, cx| input.set(String::new(), cx));
        self.forward_search
            .update(cx, |input, cx| input.set(String::new(), cx));
        self.editing = None;
        self.edit_draft = None;
        self.pending_messages.clear();
        self.clear_attachments(cx);
        self.attachments.picker_open = false;
        crate::input::clear_code_highlight_cache();
        self.reset_context_menu(cx);
        self.context.kick_confirm = None;
        self.context.kick_previous = None;
        self.context.kick_armed = false;
        self.media_ui.reset();
        self.context.user_audio.clear();
        self.context.connection_audio.clear();
        self.replies.clear();
        self.outbox.reset();
        self.message_text.clear();
        self.clear_browser();
        self.message_actions.reset();
        self.message_content.reset();
        self.message_links.clear_prompt();
        self.image_viewer.reset();
        self.playback.reset(cx);
        self.revealed_media.clear();
        self.forwarding = None;
        self.emoji_target = None;
        self.quick_switcher.reset(cx);
        self.profile_ui.reset(cx);
        self.role_mention.update(cx, |popup, cx| popup.reset(cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchored_surfaces_flip_and_remain_inside_resized_windows() {
        for (width, height) in [
            (1280., 820.),
            (880., 600.),
            (180., 200.),
            (12., 9.),
            (0., 0.),
        ] {
            for (x, y) in [
                (30., 40.),
                (width - 3., height - 2.),
                (-40., -30.),
                (f32::NAN, f32::INFINITY),
            ] {
                let bounds = popover_bounds(
                    Bounds::new(point(px(x), px(y)), size(px(20.), px(22.))),
                    size(px(width), px(height)),
                    size(px(300.), px(520.)),
                    px(8.),
                    px(12.),
                );
                assert!(bounds.left() >= px(0.) && bounds.top() >= px(0.));
                assert!(bounds.right() <= px(width) && bounds.bottom() <= px(height));
            }
        }
        let bounds = popover_bounds(
            Bounds::new(point(px(790.), px(590.)), size(px(0.), px(0.))),
            size(px(800.), px(600.)),
            size(px(300.), px(520.)),
            px(8.),
            px(12.),
        );
        assert_eq!(bounds.left(), px(478.));
        assert_eq!(bounds.top(), px(72.));
    }
}
