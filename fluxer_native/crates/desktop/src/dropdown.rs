// SPDX-License-Identifier: AGPL-3.0-or-later
//! A compact single-choice control with a window-fitted, scrollable popup.
use gpui::{prelude::*, *};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

pub struct Selected(pub String);

pub struct Dropdown {
    label: &'static str,
    options: Vec<(String, String)>,
    selected: Option<String>,
    open: bool,
    highlighted: usize,
    focus: FocusHandle,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    scroll: ScrollHandle,
    prefix: String,
    typed_at: Instant,
}

impl EventEmitter<Selected> for Dropdown {}

impl Dropdown {
    pub fn new(
        label: &'static str,
        options: Vec<(String, String)>,
        selected: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle().tab_stop(true);
        cx.on_blur(&focus, window, |this, _, cx| {
            this.open = false;
            cx.notify();
        })
        .detach();
        Self {
            label,
            options,
            selected,
            open: false,
            highlighted: 0,
            focus,
            bounds: Rc::new(Cell::new(Bounds::default())),
            scroll: ScrollHandle::new(),
            prefix: String::new(),
            typed_at: Instant::now(),
        }
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Reflect resolved server state without emitting a user-selection event.
    pub fn set_selected(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(id) {
            self.selected = Some(id.to_owned());
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window, _: &mut Context<Self>) {
        window.focus(&self.focus);
    }

    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    pub fn set_device_options(
        &mut self,
        mut options: Vec<(String, String)>,
        preferred: &str,
        cx: &mut Context<Self>,
    ) -> String {
        let label = options
            .iter()
            .find(|(id, _)| id == preferred)
            .map(|(_, label)| label.clone())
            .or_else(|| {
                self.options
                    .iter()
                    .find(|(id, _)| id == preferred)
                    .map(|(_, label)| label.trim_end_matches(" (unavailable)").to_owned())
            })
            .unwrap_or_else(|| "Selected device".into());
        if !options.iter().any(|(id, _)| id == preferred) {
            options.push((preferred.into(), format!("{label} (unavailable)")));
        }
        self.set_selected(preferred, cx);
        self.set_options(options, cx);
        label
    }

    pub fn set_options(&mut self, options: Vec<(String, String)>, cx: &mut Context<Self>) {
        // Preserve the exact selection, even if unplugged: displaying an
        // unavailable choice must never silently switch a live microphone.
        self.options = options;
        self.open = false;
        cx.notify();
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.options.is_empty() {
            return;
        }
        self.open = true;
        self.highlighted = self
            .options
            .iter()
            .position(|(id, _)| Some(id.as_str()) == self.selected())
            .unwrap_or(0);
        self.prefix.clear();
        window.focus(&self.focus);
        self.scroll.scroll_to_item(self.highlighted);
        cx.notify();
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some((id, _)) = self.options.get(index) {
            let id = id.clone();
            let changed = self.selected.as_ref() != Some(&id);
            self.selected = Some(id.clone());
            self.open = false;
            if changed {
                cx.emit(Selected(id));
            }
            cx.notify();
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "tab" {
            self.open = false;
            cx.notify();
            return;
        }
        if matches!(
            key,
            "escape" | "enter" | "space" | "up" | "down" | "home" | "end"
        ) {
            cx.stop_propagation();
            if key == "escape" {
                self.open = false;
            } else if !self.open {
                self.open(window, cx);
            } else if matches!(key, "enter" | "space") {
                self.choose(self.highlighted, cx);
            } else {
                let last = self.options.len().saturating_sub(1);
                self.highlighted = match key {
                    "up" => self.highlighted.saturating_sub(1),
                    "down" => (self.highlighted + 1).min(last),
                    "home" => 0,
                    "end" => last,
                    _ => self.highlighted,
                };
                self.scroll.scroll_to_item(self.highlighted);
            }
            cx.notify();
        } else if key.chars().count() == 1
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.platform
        {
            cx.stop_propagation();
            if !self.open {
                self.open(window, cx);
            }
            if self.typed_at.elapsed() > Duration::from_millis(700) {
                self.prefix.clear();
            }
            self.typed_at = Instant::now();
            self.prefix.push_str(&key.to_lowercase());
            if let Some(index) = self
                .options
                .iter()
                .position(|(_, label)| label.to_lowercase().starts_with(&self.prefix))
            {
                self.highlighted = index;
                self.scroll.scroll_to_item(index);
                cx.notify();
            }
        }
    }
}

impl Render for Dropdown {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self
            .options
            .iter()
            .find(|(id, _)| Some(id.as_str()) == self.selected())
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| {
                if self.selected.is_some() {
                    "Selected source unavailable".into()
                } else {
                    format!("Choose {}…", self.label.to_lowercase())
                }
            });
        let bounds = self.bounds.clone();
        let trigger_bounds = self.bounds.clone();
        div()
            .w_full()
            .max_w(px(520.))
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(super::muted(self.label))
            .child(
                div()
                    .id("trigger")
                    .relative()
                    .w_full()
                    .min_w_0()
                    .h(px(38.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x484054))
                    .bg(rgb(0x292633))
                    .text_size(px(13.))
                    .cursor_pointer()
                    .key_context("Dropdown")
                    .track_focus(&self.focus)
                    .focus(|style| style.border_color(rgb(super::ACCENT)))
                    .hover(|style| style.bg(rgb(0x353142)))
                    .on_key_down(cx.listener(Self::key_down))
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        // Enter/Space also synthesize a GPUI click. key_down
                        // already handles them, so never toggle a second time.
                        if !matches!(event, ClickEvent::Mouse(_)) {
                            return;
                        }
                        if this.open {
                            this.open = false;
                            cx.notify();
                        } else {
                            this.open(window, cx);
                        }
                    }))
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                trigger_bounds.set(bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(div().flex_1().min_w_0().text_ellipsis().child(text))
                    .child(div().text_color(rgb(super::MUTED)).child(if self.open {
                        "▴"
                    } else {
                        "▾"
                    }))
                    .when(self.open, |view| {
                        let bounds = bounds.get();
                        view.child(deferred(
                            div().absolute().top_0().left_0().child(
                                anchored()
                                    .position_mode(AnchoredPositionMode::Local)
                                    .position(point(px(0.), px(42.)))
                                    .snap_to_window_with_margin(px(8.))
                                    .child(
                                        div()
                                            .id("menu")
                                            .w(bounds.size.width)
                                            .max_h(px(288.))
                                            .overflow_y_scroll()
                                            .track_scroll(&self.scroll)
                                            .occlude()
                                            .p_1()
                                            .rounded_md()
                                            .border_1()
                                            .border_color(rgb(0x514861))
                                            .bg(rgb(0x252330))
                                            .shadow_lg()
                                            .on_mouse_down_out(cx.listener(
                                                |this, event: &MouseDownEvent, _, cx| {
                                                    if !this.bounds.get().contains(&event.position)
                                                    {
                                                        this.open = false;
                                                        cx.notify();
                                                    }
                                                },
                                            ))
                                            .children(self.options.iter().enumerate().map(
                                                |(index, (id, label))| {
                                                    let selected =
                                                        Some(id.as_str()) == self.selected();
                                                    div()
                                                        .id(index)
                                                        .h(px(34.))
                                                        .px_2()
                                                        .flex()
                                                        .items_center()
                                                        .gap_2()
                                                        .rounded_sm()
                                                        .cursor_pointer()
                                                        .text_size(px(13.))
                                                        .bg(rgb(if index == self.highlighted {
                                                            0x44395e
                                                        } else {
                                                            0x252330
                                                        }))
                                                        .hover(|style| style.bg(rgb(0x44395e)))
                                                        .child(
                                                            div()
                                                                .w(px(16.))
                                                                .text_color(rgb(super::ACCENT))
                                                                .child(if selected {
                                                                    "✓"
                                                                } else {
                                                                    ""
                                                                }),
                                                        )
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .text_ellipsis()
                                                                .child(label.clone()),
                                                        )
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                cx.stop_propagation();
                                                                this.choose(index, cx);
                                                            },
                                                        ))
                                                },
                                            )),
                                    ),
                            ),
                        ))
                    }),
            )
            .when(self.options.is_empty(), |view| {
                view.child(super::muted("No sources available. Refresh to try again."))
            })
    }
}
