// SPDX-License-Identifier: AGPL-3.0-or-later
//! Self presence picker and persisted custom status editor.
use super::*;
use crate::controls::quiet_button;
use fluxer_client::RelationshipMutationStatus;

struct StatusTooltip;
impl Render for StatusTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(RAIL))
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .child("Set status")
    }
}

pub(super) struct StatusUi {
    open: bool,
    editor: bool,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    cancel_focus: FocusHandle,
    save_focus: FocusHandle,
    window: Option<AnyWindowHandle>,
    selected: usize,
    armed: bool,
    input: Entity<TextInput>,
    status: Option<RelationshipMutationStatus>,
}
impl StatusUi {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            open: false,
            editor: false,
            focus: cx.focus_handle(),
            previous_focus: None,
            cancel_focus: cx.focus_handle(),
            save_focus: cx.focus_handle(),
            window: None,
            selected: 0,
            armed: false,
            input: cx.new(|cx| TextInput::new("What's happening?", cx)),
            status: None,
        }
    }
    pub fn is_editor(&self) -> bool {
        self.open && self.editor
    }
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn reset(&mut self, cx: &mut Context<Fluxer>) {
        self.open = false;
        self.editor = false;
        self.previous_focus = None;
        self.window = None;
        self.status = None;
        self.armed = false;
        self.input
            .update(cx, |input, cx| input.set(String::new(), cx));
    }
}
impl Fluxer {
    pub(super) fn status_indicator(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = self
            .state
            .user_settings
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("online");
        let color = match status {
            "online" => 0x43b581,
            "idle" => 0xfaa61a,
            "dnd" => 0xf04747,
            _ => 0x747f8d,
        };
        button_base("self-status-picker")
            .size(px(20.))
            .p_0()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(10.)).rounded_full().bg(rgb(color)))
            .tooltip(|_, cx| cx.new(|_| StatusTooltip).into())
            .on_click(cx.listener(|this, _, window, cx| this.open_status(window, cx)))
            .into_any_element()
    }
    fn open_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status_ui.open {
            self.close_status(window, cx);
            return;
        }
        self.reset_context_menu(cx);
        self.dismiss_user_profile(window, cx);
        self.status_ui.previous_focus = window.focused(cx);
        self.status_ui.window = Some(window.window_handle());
        self.status_ui.open = true;
        self.status_ui.editor = false;
        self.status_ui.selected = 0;
        self.status_ui.armed = false;
        self.status_ui.status = None;
        window.focus(&self.status_ui.focus);
        cx.notify();
    }
    fn close_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status_ui.open = false;
        self.status_ui.editor = false;
        self.status_ui.window = None;
        self.status_ui.armed = false;
        if let Some(focus) = self.status_ui.previous_focus.take() {
            window.focus(&focus);
        }
        cx.notify();
    }
    fn select_status(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.status_ui.status,
            Some(RelationshipMutationStatus::Pending)
        ) {
            return;
        }
        if index < 4 {
            let status = ["online", "idle", "dnd", "invisible"][index];
            self.command(
                Command::SetPresenceStatus {
                    status: status.into(),
                },
                cx,
            );
        } else if index == 5 {
            self.command(Command::ClearCustomStatus, cx);
        } else {
            let text = self
                .state
                .user_settings
                .pointer("/custom_status/text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned();
            self.status_ui
                .input
                .update(cx, |input, cx| input.set(text, cx));
            self.status_ui.editor = true;
            self.status_ui.status = None;
            self.status_ui.input.read(cx).focus_handle(cx).focus(window);
            cx.notify();
        }
    }
    fn cancel_status_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status_ui.editor = false;
        self.status_ui.armed = false;
        window.focus(&self.status_ui.focus);
        cx.notify();
    }
    fn save_status_editor(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            self.status_ui.status,
            Some(RelationshipMutationStatus::Pending)
        ) {
            let text = self.status_ui.input.read(cx).content.to_string();
            self.command(Command::SetCustomStatus { text }, cx);
        }
    }
    pub(super) fn handle_presence_mutation(
        &mut self,
        status: RelationshipMutationStatus,
        cx: &mut Context<Self>,
    ) {
        if status == RelationshipMutationStatus::Complete {
            self.status_ui.open = false;
            self.status_ui.editor = false;
            if let (Some(window), Some(focus)) = (
                self.status_ui.window.take(),
                self.status_ui.previous_focus.take(),
            ) {
                let _ = window.update(cx, |_, window, _| window.focus(&focus));
            }
        }
        self.status_ui.status = Some(status);
        cx.notify();
    }
    pub(super) fn status_surface(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let pending = matches!(
            self.status_ui.status,
            Some(RelationshipMutationStatus::Pending)
        );
        let error = match &self.status_ui.status {
            Some(RelationshipMutationStatus::Failed(message)) => Some(message.clone()),
            _ => None,
        };
        if self.status_ui.editor {
            let editor = div()
                .id("custom-status-dialog")
                .w(px(420.))
                .p_5()
                .rounded_lg()
                .bg(rgb(PANEL))
                .shadow_lg()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(FontWeight::BOLD)
                        .child("Set a custom status"),
                )
                .child(self.status_ui.input.clone())
                .when(
                    matches!(
                        self.status_ui.status,
                        Some(RelationshipMutationStatus::Complete)
                    ),
                    |view| view.child(div().text_size(px(12.)).child("Saved")),
                )
                .when_some(error, |view, error| {
                    view.child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0xf2b8b5))
                            .child(error),
                    )
                })
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            quiet_button("status-cancel", "Cancel")
                                .track_focus(&self.status_ui.cancel_focus)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.cancel_status_editor(window, cx);
                                })),
                        )
                        .child(
                            quiet_button("status-save", if pending { "Saving…" } else { "Save" })
                                .track_focus(&self.status_ui.save_focus)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.save_status_editor(cx);
                                })),
                        ),
                );
            return div()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000099))
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => this.cancel_status_editor(window, cx),
                        "tab" => {
                            let input = this.status_ui.input.focus_handle(cx);
                            let current = if input.is_focused(window) {
                                0
                            } else if this.status_ui.cancel_focus.is_focused(window) {
                                1
                            } else {
                                2
                            };
                            let next = editor_tab_target(current, event.keystroke.modifiers.shift);
                            this.status_ui.armed = false;
                            match next {
                                0 => window.focus(&input),
                                1 => window.focus(&this.status_ui.cancel_focus),
                                _ => window.focus(&this.status_ui.save_focus),
                            }
                        }
                        "enter" if !event.keystroke.modifiers.modified() => {
                            if !event.is_held && !this.status_ui.input.read(cx).is_composing() {
                                this.status_ui.armed = true;
                            }
                            // Let the input method consume composition confirmation.
                            if this.status_ui.input.read(cx).is_composing() {
                                return;
                            }
                        }
                        _ => return,
                    }
                    cx.stop_propagation();
                }))
                .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                    if event.keystroke.key == "enter" {
                        let armed = std::mem::take(&mut this.status_ui.armed);
                        if armed
                            && !event.keystroke.modifiers.modified()
                            && !this.status_ui.input.read(cx).is_composing()
                        {
                            if this.status_ui.cancel_focus.is_focused(window) {
                                this.cancel_status_editor(window, cx);
                            } else {
                                this.save_status_editor(cx);
                            }
                        }
                        cx.stop_propagation();
                    }
                }))
                .child(editor)
                .into_any_element();
        }
        let selected = self.status_ui.selected;
        let current = self
            .state
            .user_settings
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("online")
            .to_owned();
        let custom = self
            .state
            .user_settings
            .pointer("/custom_status/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        let has_custom = self
            .state
            .user_settings
            .get("custom_status")
            .is_some_and(|value| !value.is_null());
        let menu_count = 5 + usize::from(has_custom);
        let custom_label = if custom.is_empty() {
            "Set a custom status".to_owned()
        } else {
            custom
        };
        let custom_art =
            emoji_sprites::unicode_text(&custom_label, 12., window.scale_factor() > 1., cx);
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(point(px(78.), window.viewport_size().height - px(65.)))
                .anchor(Corner::BottomLeft)
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .id("self-status-menu")
                        .track_focus(&self.status_ui.focus)
                        .occlude()
                        .w(px(220.))
                        .p_2()
                        .rounded_lg()
                        .bg(rgb(0x232428))
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .capture_key_down(cx.listener(
                            move |this, event: &KeyDownEvent, window, cx| {
                                match event.keystroke.key.as_str() {
                                    "escape" => this.close_status(window, cx),
                                    "up" => {
                                        this.status_ui.selected =
                                            (this.status_ui.selected + menu_count - 1) % menu_count;
                                        this.status_ui.armed = false;
                                        cx.notify();
                                    }
                                    "down" | "tab" => {
                                        this.status_ui.selected = if event.keystroke.key == "tab"
                                            && event.keystroke.modifiers.shift
                                        {
                                            (this.status_ui.selected + menu_count - 1) % menu_count
                                        } else {
                                            (this.status_ui.selected + 1) % menu_count
                                        };
                                        this.status_ui.armed = false;
                                        cx.notify();
                                    }
                                    "enter" | "space"
                                        if !event.is_held
                                            && !event.keystroke.modifiers.modified() =>
                                    {
                                        this.status_ui.armed = true
                                    }
                                    _ => return,
                                };
                                cx.stop_propagation();
                            },
                        ))
                        .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                let armed = std::mem::take(&mut this.status_ui.armed);
                                if armed && !event.keystroke.modifiers.modified() {
                                    this.select_status(this.status_ui.selected, window, cx);
                                }
                                cx.stop_propagation();
                            }
                        }))
                        .on_mouse_down_out(
                            cx.listener(|this, _, window, cx| this.close_status(window, cx)),
                        )
                        .children(
                            [
                                ("online", "Online", 0x43b581),
                                ("idle", "Idle", 0xfaa61a),
                                ("dnd", "Do Not Disturb", 0xf04747),
                                ("invisible", "Invisible", 0x747f8d),
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(index, (value, label, color))| {
                                let active = current == value;
                                div()
                                    .id(("self-status", index))
                                    .px_2()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(if selected == index {
                                        0x41424a
                                    } else {
                                        0x232428
                                    }))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().size(px(9.)).rounded_full().bg(rgb(color)))
                                    .child(label)
                                    .when(active, |v| v.child("✓"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.select_status(index, window, cx)
                                    }))
                            }),
                        )
                        .child(
                            div()
                                .id("self-custom-status")
                                .px_2()
                                .py_2()
                                .rounded_md()
                                .bg(rgb(if selected == 4 { 0x41424a } else { 0x232428 }))
                                .child(custom_art)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.select_status(4, window, cx)
                                })),
                        )
                        .when(has_custom, |view| {
                            view.child(
                                div()
                                    .id("self-clear-status")
                                    .px_2()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(if selected == 5 { 0x41424a } else { 0x232428 }))
                                    .child("Clear custom status")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.select_status(5, window, cx)
                                    })),
                            )
                        })
                        .when(pending, |v| {
                            v.child(div().px_2().text_size(px(11.)).child("Saving…"))
                        })
                        .when_some(error, |v, error| {
                            v.child(
                                div()
                                    .px_2()
                                    .text_size(px(11.))
                                    .text_color(rgb(0xf2b8b5))
                                    .child(error),
                            )
                        }),
                ),
        )
        .with_priority(10)
        .into_any_element()
    }
}

fn editor_tab_target(current: usize, reverse: bool) -> usize {
    if reverse {
        (current + 2) % 3
    } else {
        (current + 1) % 3
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn editor_focus_cycles_in_both_directions() {
        assert_eq!(
            (0..3)
                .map(|i| super::editor_tab_target(i, false))
                .collect::<Vec<_>>(),
            vec![1, 2, 0]
        );
        assert_eq!(
            (0..3)
                .map(|i| super::editor_tab_target(i, true))
                .collect::<Vec<_>>(),
            vec![2, 0, 1]
        );
    }
}
