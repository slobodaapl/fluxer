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
    expiry_picker: Entity<Dropdown>,
    duration_picker: Entity<Dropdown>,
    emoji_focus: FocusHandle,
    emoji_clear_focus: FocusHandle,
    emoji_id: Option<String>,
    emoji_name: Option<String>,
    window: Option<AnyWindowHandle>,
    selected: usize,
    armed: bool,
    input: Entity<TextInput>,
    status: Option<RelationshipMutationStatus>,
}
impl StatusUi {
    pub fn new(window: &mut Window, cx: &mut Context<Fluxer>) -> Self {
        let options = || {
            EXPIRY_LABELS
                .iter()
                .enumerate()
                .map(|(i, label)| (i.to_string(), (*label).to_owned()))
                .collect()
        };
        let expiry_picker =
            cx.new(|cx| Dropdown::new("Clear after", options(), Some("7".into()), window, cx));
        let duration_picker =
            cx.new(|cx| Dropdown::new("Status duration", options(), Some("0".into()), window, cx));
        expiry_picker.update(cx, |picker, _| picker.set_popup_priority(11));
        duration_picker.update(cx, |picker, _| picker.set_popup_priority(11));
        Self {
            open: false,
            editor: false,
            focus: cx.focus_handle(),
            previous_focus: None,
            cancel_focus: cx.focus_handle(),
            save_focus: cx.focus_handle(),
            expiry_picker,
            duration_picker,
            emoji_focus: cx.focus_handle(),
            emoji_clear_focus: cx.focus_handle(),
            emoji_id: None,
            emoji_name: None,
            window: None,
            selected: 0,
            armed: false,
            input: cx.new(|cx| TextInput::new("What's happening?", cx)),
            status: None,
        }
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
        self.expiry_picker
            .update(cx, |picker, cx| picker.dismiss(cx));
        self.duration_picker
            .update(cx, |picker, cx| picker.dismiss(cx));
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
        self.status_ui
            .expiry_picker
            .update(cx, |picker, cx| picker.dismiss(cx));
        self.status_ui.duration_picker.update(cx, |picker, cx| {
            picker.dismiss(cx);
            picker.set_selected("0", cx);
        });
        self.status_ui.open = true;
        self.status_ui.editor = false;
        self.status_ui.selected = 0;
        self.status_ui.armed = false;
        self.status_ui.status = None;
        window.focus(&self.status_ui.focus);
        cx.notify();
    }
    fn close_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status_ui
            .expiry_picker
            .update(cx, |picker, cx| picker.dismiss(cx));
        self.status_ui
            .duration_picker
            .update(cx, |picker, cx| picker.dismiss(cx));
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
                    duration_minutes: EXPIRY_MINUTES
                        [picker_index(&self.status_ui.duration_picker, cx)],
                },
                cx,
            );
        } else if index == 4 {
            self.status_ui
                .duration_picker
                .update(cx, |picker, cx| picker.show(window, cx));
        } else if index == 6 {
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
            let custom = self.state.user_settings.get("custom_status");
            self.status_ui.emoji_id = custom
                .and_then(|v| v.get("emoji_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            self.status_ui.emoji_name = custom
                .and_then(|v| v.get("emoji_name"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let expiry = custom
                .and_then(|v| v.get("expires_at"))
                .and_then(serde_json::Value::as_str)
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|date| {
                    expiry_index(
                        (date.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds(),
                    )
                })
                .unwrap_or(if custom.is_some_and(|v| !v.is_null()) {
                    0
                } else {
                    7
                });
            self.status_ui.expiry_picker.update(cx, |picker, cx| {
                picker.dismiss(cx);
                picker.set_selected(&expiry.to_string(), cx);
            });
            self.status_ui.editor = true;
            self.status_ui.status = None;
            self.status_ui.input.read(cx).focus_handle(cx).focus(window);
            cx.notify();
        }
    }
    fn cancel_status_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status_ui
            .expiry_picker
            .update(cx, |picker, cx| picker.dismiss(cx));
        self.status_ui.editor = false;
        self.status_ui.armed = false;
        window.focus(&self.status_ui.focus);
        cx.notify();
    }
    pub(super) fn close_emoji_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.emoji_target.take();
        if target == Some(EmojiTarget::CustomStatus) {
            window.focus(&self.status_ui.emoji_focus);
        } else {
            self.composer.focus_handle(cx).focus(window);
        }
        cx.notify();
    }
    fn open_status_emoji(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let size = window.viewport_size();
        self.open_emoji(
            EmojiTarget::CustomStatus,
            point(size.width / 2., size.height / 2.),
            window,
            cx,
        );
    }
    pub(super) fn set_status_emoji(
        &mut self,
        choice: &chat_helpers::EmojiChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.status_ui.emoji_id = choice.id.map(|id| id.to_string());
        self.status_ui.emoji_name = Some(if choice.id.is_some() {
            choice.name.clone()
        } else {
            choice.reaction.clone()
        });
        window.focus(&self.status_ui.emoji_focus);
        cx.notify();
    }
    fn save_status_editor(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            self.status_ui.status,
            Some(RelationshipMutationStatus::Pending)
        ) {
            let text = self.status_ui.input.read(cx).content.to_string();
            let expires_at =
                EXPIRY_MINUTES[picker_index(&self.status_ui.expiry_picker, cx)].map(|minutes| {
                    (chrono::Utc::now() + chrono::Duration::minutes(minutes))
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                });
            self.command(
                Command::SetCustomStatus {
                    text,
                    emoji_id: self.status_ui.emoji_id.clone(),
                    emoji_name: self.status_ui.emoji_name.clone(),
                    expires_at,
                },
                cx,
            );
        }
    }
    // Enter resolves to the app Submit action before raw key dispatch. Arm here
    // and save on release, so the input's action path follows the same modal flow.
    pub(super) fn arm_status_submit(&mut self, window: &Window, cx: &Context<Self>) {
        if self.status_ui.editor
            && !self
                .status_ui
                .expiry_picker
                .read(cx)
                .focus_handle()
                .is_focused(window)
            && !self.status_ui.input.read(cx).is_composing()
        {
            self.status_ui.armed = true;
        }
    }
    // Handle these at the root so focus escaping the surface cannot expose the
    // application underneath it to navigation keys.
    pub(super) fn status_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.status_ui.open || self.emoji_target.is_some() {
            return;
        }
        let key = event.keystroke.key.as_str();
        let picker = if self.status_ui.editor {
            self.status_ui.expiry_picker.clone()
        } else {
            self.status_ui.duration_picker.clone()
        };
        let focused = picker.read(cx).focus_handle().is_focused(window);
        let open = picker.read(cx).is_open();
        if picker_captures_key(focused, open, key) {
            self.status_ui.armed = false;
            picker.update(cx, |picker, cx| picker.handle_key_down(event, window, cx));
            cx.stop_propagation();
            return;
        }
        if self.status_ui.editor {
            match key {
                "escape" => self.cancel_status_editor(window, cx),
                "tab" => {
                    let input = self.status_ui.input.focus_handle(cx);
                    let current = if input.is_focused(window) {
                        0
                    } else if self.status_ui.emoji_focus.is_focused(window) {
                        1
                    } else if self.status_ui.emoji_clear_focus.is_focused(window) {
                        2
                    } else if self
                        .status_ui
                        .expiry_picker
                        .read(cx)
                        .focus_handle()
                        .is_focused(window)
                    {
                        3
                    } else if self.status_ui.cancel_focus.is_focused(window) {
                        4
                    } else {
                        5
                    };
                    let next = editor_tab_target(current, event.keystroke.modifiers.shift);
                    self.status_ui.armed = false;
                    match next {
                        0 => window.focus(&input),
                        1 => window.focus(&self.status_ui.emoji_focus),
                        2 => window.focus(&self.status_ui.emoji_clear_focus),
                        3 => window.focus(&self.status_ui.expiry_picker.read(cx).focus_handle()),
                        4 => window.focus(&self.status_ui.cancel_focus),
                        _ => window.focus(&self.status_ui.save_focus),
                    }
                }
                "enter" if !event.keystroke.modifiers.modified() => {
                    if self.status_ui.input.read(cx).is_composing() {
                        return;
                    }
                    if !event.is_held {
                        self.status_ui.armed = true;
                    }
                }
                _ => return,
            }
        } else {
            let count = 6 + usize::from(
                self.state
                    .user_settings
                    .get("custom_status")
                    .is_some_and(custom_status_active),
            );
            match key {
                "escape" => self.close_status(window, cx),
                "up" => {
                    self.status_ui.selected = (self.status_ui.selected + count - 1) % count;
                    self.status_ui.armed = false;
                    cx.notify();
                }
                "down" | "tab" => {
                    if key == "tab" && focused {
                        picker.update(cx, |picker, cx| picker.dismiss(cx));
                        window.focus(&self.status_ui.focus);
                        self.status_ui.selected = 4;
                    }
                    self.status_ui.selected = if key == "tab" && event.keystroke.modifiers.shift {
                        (self.status_ui.selected + count - 1) % count
                    } else {
                        (self.status_ui.selected + 1) % count
                    };
                    self.status_ui.armed = false;
                    cx.notify();
                }
                "enter" | "space" if !event.is_held && !event.keystroke.modifiers.modified() => {
                    self.status_ui.armed = true
                }
                _ => return,
            }
        }
        cx.stop_propagation();
    }
    pub(super) fn status_key_up(
        &mut self,
        event: &KeyUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.status_ui.open || self.emoji_target.is_some() {
            return;
        }
        let key = event.keystroke.key.as_str();
        let picker = if self.status_ui.editor {
            &self.status_ui.expiry_picker
        } else {
            &self.status_ui.duration_picker
        };
        if picker.read(cx).focus_handle().is_focused(window)
            && matches!(
                key,
                "enter" | "space" | "up" | "down" | "home" | "end" | "escape"
            )
        {
            self.status_ui.armed = false;
            cx.stop_propagation();
            return;
        }
        if (self.status_ui.editor && key == "enter")
            || (!self.status_ui.editor && matches!(key, "enter" | "space"))
        {
            let armed = std::mem::take(&mut self.status_ui.armed);
            if armed && !event.keystroke.modifiers.modified() {
                if self.status_ui.editor {
                    if !self.status_ui.input.read(cx).is_composing() {
                        if self.status_ui.cancel_focus.is_focused(window) {
                            self.cancel_status_editor(window, cx);
                        } else if self.status_ui.emoji_clear_focus.is_focused(window) {
                            self.status_ui.emoji_id = None;
                            self.status_ui.emoji_name = None;
                            cx.notify();
                        } else if self.status_ui.emoji_focus.is_focused(window) {
                            self.open_status_emoji(window, cx);
                        } else {
                            self.save_status_editor(cx);
                        }
                    }
                } else {
                    self.select_status(self.status_ui.selected, window, cx);
                }
            }
            cx.stop_propagation();
        }
    }
    pub(super) fn handle_presence_mutation(
        &mut self,
        status: RelationshipMutationStatus,
        cx: &mut Context<Self>,
    ) {
        if status == RelationshipMutationStatus::Complete {
            self.status_ui
                .expiry_picker
                .update(cx, |picker, cx| picker.dismiss(cx));
            self.status_ui
                .duration_picker
                .update(cx, |picker, cx| picker.dismiss(cx));
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
            let emoji = custom_status_emoji(
                self.status_ui.emoji_id.as_deref(),
                self.status_ui.emoji_name.as_deref(),
                self.media_endpoint.as_deref(),
                window,
                cx,
            );
            let emoji_label = if emoji.is_some() { "" } else { "Choose emoji" };
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
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            quiet_button("status-emoji", emoji_label)
                                .flex()
                                .items_center()
                                .min_h(px(30.))
                                .when_some(emoji, |button, emoji| button.child(emoji))
                                .track_focus(&self.status_ui.emoji_focus)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_status_emoji(window, cx)
                                })),
                        )
                        .child(
                            quiet_button("status-emoji-clear", "Clear emoji")
                                .track_focus(&self.status_ui.emoji_clear_focus)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.status_ui.emoji_id = None;
                                    this.status_ui.emoji_name = None;
                                    cx.notify();
                                })),
                        ),
                )
                .child(self.status_ui.expiry_picker.clone())
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
            // The root places status above normal content. Keep this surface
            // in normal drawing so its dropdown can schedule its own popup.
            return div()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000099))
                .occlude()
                .flex()
                .items_center()
                .justify_center()
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
            .get("custom_status")
            .filter(|value| custom_status_active(value))
            .and_then(|value| value.get("text"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        let has_custom = self
            .state
            .user_settings
            .get("custom_status")
            .is_some_and(custom_status_active);
        let custom_label = if custom.is_empty() {
            "Set a custom status".to_owned()
        } else {
            custom
        };
        let custom_art =
            emoji_sprites::unicode_text(&custom_label, 12., window.scale_factor() > 1., cx);
        let custom_emoji = self
            .state
            .user_settings
            .get("custom_status")
            .and_then(|custom| {
                custom_status_emoji(
                    custom.get("emoji_id").and_then(|value| value.as_str()),
                    custom.get("emoji_name").and_then(|value| value.as_str()),
                    self.media_endpoint.as_deref(),
                    window,
                    cx,
                )
            });
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
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                        if !this.status_ui.duration_picker.read(cx).is_open() {
                            this.close_status(window, cx);
                        }
                    }))
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
                            .rounded_md()
                            .bg(rgb(if selected == 4 { 0x41424a } else { 0x232428 }))
                            .child(self.status_ui.duration_picker.clone()),
                    )
                    .child(
                        div()
                            .id("self-custom-status")
                            .px_2()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(if selected == 5 { 0x41424a } else { 0x232428 }))
                            .flex()
                            .items_center()
                            .gap_2()
                            .when_some(custom_emoji, |row, emoji| row.child(emoji))
                            .child(custom_art)
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.select_status(5, window, cx)
                                }),
                            ),
                    )
                    .when(has_custom, |view| {
                        view.child(
                            div()
                                .id("self-clear-status")
                                .px_2()
                                .py_2()
                                .rounded_md()
                                .bg(rgb(if selected == 6 { 0x41424a } else { 0x232428 }))
                                .child("Clear custom status")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.select_status(6, window, cx)
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
            )
            .into_any_element()
    }
}

fn custom_status_emoji(
    id: Option<&str>,
    name: Option<&str>,
    endpoint: Option<&str>,
    window: &Window,
    cx: &mut App,
) -> Option<AnyElement> {
    if let Some(id) = id.and_then(|id| id.parse::<Snowflake>().ok()) {
        return endpoint.map(|endpoint| {
            img(SharedString::from(format!(
                "{}/emojis/{id}.webp?size=48",
                endpoint.trim_end_matches('/')
            )))
            .size(px(20.))
            .object_fit(ObjectFit::Contain)
            .into_any_element()
        });
    }
    name.filter(|name| !name.is_empty())
        .map(|name| emoji_sprites::unicode_art(name, 20., window.scale_factor() > 1., cx))
}

fn picker_captures_key(focused: bool, open: bool, key: &str) -> bool {
    focused
        && (matches!(key, "enter" | "space" | "up" | "down" | "home" | "end")
            || (open && key == "escape"))
}
fn picker_index(picker: &Entity<Dropdown>, cx: &App) -> usize {
    picker
        .read(cx)
        .selected()
        .and_then(|id| id.parse::<usize>().ok())
        .filter(|i| *i < EXPIRY_MINUTES.len())
        .unwrap_or(0)
}
fn custom_status_active(value: &serde_json::Value) -> bool {
    !value.is_null()
        && !value
            .get("expires_at")
            .and_then(serde_json::Value::as_str)
            .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
            .is_some_and(|date| date <= chrono::Utc::now())
}
const EXPIRY_MINUTES: [Option<i64>; 9] = [
    None,
    Some(15),
    Some(30),
    Some(60),
    Some(180),
    Some(240),
    Some(480),
    Some(1440),
    Some(4320),
];
const EXPIRY_LABELS: [&str; 9] = [
    "Don't clear",
    "15 minutes",
    "30 minutes",
    "1 hour",
    "3 hours",
    "4 hours",
    "8 hours",
    "24 hours",
    "3 days",
];
fn expiry_index(seconds: i64) -> usize {
    if seconds <= 0 {
        return 7;
    }
    EXPIRY_MINUTES
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, m)| seconds <= m.unwrap() * 60 + 60)
        .map(|(i, _)| i)
        .unwrap_or(8)
}
fn editor_tab_target(current: usize, reverse: bool) -> usize {
    if reverse {
        (current + 5) % 6
    } else {
        (current + 1) % 6
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dropdown_keys_do_not_submit_or_close_the_status_surface() {
        use super::picker_captures_key;
        assert!(picker_captures_key(true, false, "enter"));
        assert!(picker_captures_key(true, true, "enter"));
        assert!(picker_captures_key(true, true, "escape"));
        assert!(!picker_captures_key(true, false, "escape"));
        assert!(!picker_captures_key(false, true, "enter"));
        assert!(!picker_captures_key(true, true, "tab"));
    }
    #[test]
    fn expiry_preset_matches_source_tolerance_and_fallback() {
        assert_eq!(super::expiry_index(0), 7);
        assert_eq!(super::expiry_index(15 * 60 + 60), 1);
        assert_eq!(super::expiry_index(15 * 60 + 61), 2);
        assert_eq!(super::expiry_index(10 * 24 * 60 * 60), 8);
    }
    #[test]
    fn editor_focus_cycles_in_both_directions() {
        assert_eq!(
            (0..6)
                .map(|i| super::editor_tab_target(i, false))
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 0]
        );
        assert_eq!(
            (0..6)
                .map(|i| super::editor_tab_target(i, true))
                .collect::<Vec<_>>(),
            vec![5, 0, 1, 2, 3, 4]
        );
    }
}
