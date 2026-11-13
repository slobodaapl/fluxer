// SPDX-License-Identifier: AGPL-3.0-or-later
//! Compact native menus following classic ContextMenu and voice stream menus.
//! Callbacks are deferred until this entity is released, so owners may safely
//! reset the menu or open another surface. Check rows and sliders stay open.
use super::*;
use crate::controls::{Icon, icon, slider};
use std::rc::Rc;

#[derive(Clone)]
pub(super) enum ContextMenuItem {
    Action {
        id: String,
        label: String,
        icon: Option<Icon>,
        enabled: bool,
        danger: bool,
    },
    Toggle {
        id: String,
        label: String,
        checked: bool,
        enabled: bool,
    },
    Slider {
        id: String,
        label: String,
        value: f32,
        min: f32,
        max: f32,
        step: f32,
    },
    Header(String),
    Separator,
}
impl ContextMenuItem {
    fn id(&self) -> Option<&str> {
        match self {
            Self::Action { id, .. } | Self::Toggle { id, .. } | Self::Slider { id, .. } => Some(id),
            _ => None,
        }
    }
    fn selectable(&self) -> bool {
        match self {
            Self::Action { enabled, .. } | Self::Toggle { enabled, .. } => *enabled,
            Self::Slider { min, max, step, .. } => valid_range(*min, *max, *step),
            _ => false,
        }
    }
    fn height(&self) -> f32 {
        match self {
            Self::Header(_) => 28.,
            Self::Separator => 9.,
            Self::Slider { .. } => 64.,
            _ => 32.,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ContextMenuEvent {
    Action(String),
    Toggle(String, bool),
    Slider(String, f32),
    Dismiss,
}
pub(super) type ContextMenuCallback = Rc<dyn Fn(ContextMenuEvent, &mut Window, &mut App)>;

pub(super) struct ContextMenu {
    items: Vec<ContextMenuItem>,
    callback: Option<ContextMenuCallback>,
    anchor: Point<Pixels>,
    selected: Option<usize>,
    armed: Option<usize>,
    focus: FocusHandle,
    row_focus: Vec<Option<FocusHandle>>,
    previous_focus: Option<FocusHandle>,
    scroll: ScrollHandle,
    generation: u64,
}
impl ContextMenu {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            items: Vec::new(),
            callback: None,
            anchor: Point::default(),
            selected: None,
            armed: None,
            focus: cx.focus_handle(),
            row_focus: Vec::new(),
            previous_focus: None,
            scroll: ScrollHandle::new(),
            generation: 0,
        }
    }
    pub fn is_open(&self) -> bool {
        self.callback.is_some()
    }
    pub fn open(
        &mut self,
        items: Vec<ContextMenuItem>,
        anchor: Point<Pixels>,
        callback: ContextMenuCallback,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_open() {
            self.previous_focus = window.focused(cx);
        }
        self.generation = self.generation.wrapping_add(1);
        self.items = items;
        self.anchor = anchor;
        self.callback = Some(callback);
        self.armed = None;
        self.scroll = ScrollHandle::new();
        self.row_focus = self
            .items
            .iter()
            .map(|item| item.selectable().then(|| cx.focus_handle().tab_stop(true)))
            .collect();
        self.selected = selection(&self.items, None, "home");
        self.focus_selected(window);
        cx.notify();
    }
    /// Refresh live checked/volume states without emitting user actions. Stable
    /// item IDs preserve focus; removal selects the first available control.
    pub fn set_items(
        &mut self,
        items: Vec<ContextMenuItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_open() {
            return;
        }
        let selected_id = self
            .selected
            .and_then(|i| self.items.get(i))
            .and_then(ContextMenuItem::id)
            .map(str::to_owned);
        let old_focus = self
            .items
            .iter()
            .zip(&self.row_focus)
            .filter_map(|(item, focus)| Some((item.id()?.to_owned(), focus.clone()?)))
            .collect::<BTreeMap<_, _>>();
        self.items = items;
        self.row_focus = self
            .items
            .iter()
            .map(|item| {
                item.selectable().then(|| {
                    item.id()
                        .and_then(|id| old_focus.get(id))
                        .cloned()
                        .unwrap_or_else(|| cx.focus_handle().tab_stop(true))
                })
            })
            .collect();
        self.selected = self
            .items
            .iter()
            .position(|item| item.selectable() && item.id() == selected_id.as_deref())
            .or_else(|| selection(&self.items, None, "home"));
        self.armed = None;
        self.focus_selected(window);
        cx.notify();
    }
    /// Lifecycle reset without emitting Dismiss. For a visible close use dismiss,
    /// which also restores the invoking focus.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.items.clear();
        self.row_focus.clear();
        self.callback = None;
        self.previous_focus = None;
        self.selected = None;
        self.armed = None;
        cx.notify();
    }
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_open() {
            return;
        }
        let callback = self.callback.clone();
        self.close(window, cx);
        if let Some(callback) = callback {
            let generation = self.generation;
            let weak = cx.entity().downgrade();
            window.defer(cx, move |window, cx| {
                // An outside right-click may already have opened another menu
                // before deferred effects run. Do not close that new session.
                let current = weak
                    .update(cx, |menu, _| {
                        menu.generation == generation && !menu.is_open()
                    })
                    .unwrap_or(false);
                if current {
                    callback(ContextMenuEvent::Dismiss, window, cx);
                }
            });
        }
    }
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.previous_focus.take();
        self.reset(cx);
        if let Some(previous) = previous {
            previous.focus(window);
        }
    }
    fn emit(&self, event: ContextMenuEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(callback) = self.callback.clone() {
            window.defer(cx, move |window, cx| callback(event, window, cx));
        }
    }
    fn focus_selected(&self, window: &mut Window) {
        if let Some(focus) = self
            .selected
            .and_then(|i| self.row_focus.get(i))
            .and_then(Option::as_ref)
        {
            focus.focus(window);
            if let Some(index) = self.selected {
                self.scroll.scroll_to_item(index);
            }
        } else {
            self.focus.focus(window);
        }
    }
    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.items.get(index).is_none_or(|item| !item.selectable()) {
            return;
        }
        if self.selected != Some(index) {
            self.selected = Some(index);
            self.armed = None;
        }
        self.focus_selected(window);
        cx.notify();
    }
    fn focused_slider(&self, window: &Window) -> bool {
        self.row_focus.iter().enumerate().any(|(i, focus)| {
            focus.as_ref().is_some_and(|focus| focus.is_focused(window))
                && matches!(self.items.get(i), Some(ContextMenuItem::Slider { .. }))
        })
    }
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self
            .items
            .get(index)
            .cloned()
            .filter(ContextMenuItem::selectable)
        else {
            return;
        };
        match item {
            ContextMenuItem::Action { id, .. } => {
                let callback = self.callback.clone();
                self.close(window, cx);
                if let Some(callback) = callback {
                    window.defer(cx, move |window, cx| {
                        callback(ContextMenuEvent::Action(id), window, cx)
                    });
                }
            }
            ContextMenuItem::Toggle { id, checked, .. } => {
                if let Some(ContextMenuItem::Toggle { checked: state, .. }) =
                    self.items.get_mut(index)
                {
                    *state = !checked;
                }
                self.emit(ContextMenuEvent::Toggle(id, !checked), window, cx);
                cx.notify();
            }
            _ => {}
        }
    }
    fn change_slider(
        &mut self,
        index: usize,
        value: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ContextMenuItem::Slider {
            id,
            value: current,
            min,
            max,
            step,
            ..
        }) = self.items.get_mut(index)
        else {
            return;
        };
        if !valid_range(*min, *max, *step) {
            return;
        }
        let value = normalized_value(value, *min, *max);
        let event = (*current != value).then(|| ContextMenuEvent::Slider(id.clone(), value));
        *current = value;
        self.selected = Some(index);
        self.armed = None;
        self.focus_selected(window);
        if let Some(event) = event {
            self.emit(event, window, cx);
        }
        cx.notify();
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => self.dismiss(window, cx),
            "tab" => {
                self.armed = None;
                self.selected = selection(
                    &self.items,
                    self.selected,
                    if event.keystroke.modifiers.shift {
                        "up"
                    } else {
                        "down"
                    },
                );
                self.focus_selected(window);
                cx.notify();
            }
            "up" | "down" | "home" | "end" if !event.keystroke.modifiers.modified() => {
                // The shared slider owns all four keys while its thumb has focus.
                if self.focused_slider(window) {
                    return;
                }
                self.armed = None;
                self.selected = selection(&self.items, self.selected, key);
                self.focus_selected(window);
                cx.notify();
            }
            "enter" | "space" if !event.keystroke.modifiers.modified() => {
                if !event.is_held && !self.focused_slider(window) {
                    self.armed = self.selected;
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
    fn key_up(&mut self, event: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
            if let Some(index) = self.armed.take()
                && self.selected == Some(index)
            {
                self.activate(index, window, cx);
            }
            // Never let a synthesized GPUI click activate the row a second time,
            // or activate from the key release that originally opened this menu.
            cx.stop_propagation();
        }
    }
}

impl Render for ContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open() {
            return div().into_any_element();
        }
        let desired_height = self.items.iter().map(ContextMenuItem::height).sum::<f32>() + 12.;
        let (origin, width, height) =
            placement(self.anchor, window.viewport_size(), desired_height);
        let rows =
            self.items
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, item)| match item {
                    ContextMenuItem::Header(label) => div()
                        .h(px(28.))
                        .px_2()
                        .flex()
                        .items_center()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(MUTED))
                        .child(
                            div()
                                .min_w_0()
                                .text_ellipsis()
                                .child(crate::emoji_sprites::unicode_text(&label, 11., false, cx)),
                        )
                        .into_any_element(),
                    ContextMenuItem::Separator => div()
                        .h(px(9.))
                        .py_1()
                        .child(div().h(px(1.)).bg(rgb(0x494151)))
                        .into_any_element(),
                    ContextMenuItem::Slider {
                        label,
                        value,
                        min,
                        max,
                        step,
                        ..
                    } => {
                        let enabled = valid_range(min, max, step);
                        let weak = cx.entity().downgrade();
                        div()
                            .id(("context-menu-slider-row", index))
                            .w_full()
                            .min_w_0()
                            .h(px(64.))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .when(self.selected == Some(index), |row| row.bg(rgb(0x393144)))
                            .when(!enabled, |row| row.opacity(0.45))
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .child(label.clone()),
                                    )
                                    .child(div().text_size(px(12.)).text_color(rgb(MUTED)).child(
                                        format!("{:.0}%", normalized_value(value, min, max)),
                                    )),
                            )
                            .when(enabled, |row| {
                                row.child(
                                    slider(
                                        ("context-menu-slider", index),
                                        normalized_value(value, min, max),
                                        min,
                                        max,
                                        step,
                                        SharedString::from(format!(
                                            "{label} · Arrow keys, Home, End"
                                        )),
                                        move |value, window, cx| {
                                            cx.stop_propagation();
                                            let value = *value;
                                            let _ = weak.update(cx, |menu, cx| {
                                                menu.change_slider(index, value, window, cx)
                                            });
                                        },
                                    )
                                    .track_focus(
                                        self.row_focus[index]
                                            .as_ref()
                                            .expect("enabled slider focus"),
                                    ),
                                )
                            })
                            .into_any_element()
                    }
                    item => {
                        let enabled = item.selectable();
                        let (label, artwork, checked, danger) = match item {
                            ContextMenuItem::Action {
                                label,
                                icon,
                                danger,
                                ..
                            } => (label, icon, None, danger),
                            ContextMenuItem::Toggle { label, checked, .. } => {
                                (label, None, Some(checked), false)
                            }
                            _ => unreachable!(),
                        };
                        div()
                            .id(("context-menu-row", index))
                            .w_full()
                            .min_w_0()
                            .h(px(32.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .text_size(px(13.))
                            .text_color(rgb(if danger { 0xef9b9f } else { TEXT }))
                            .when(!enabled, |row| row.opacity(0.45))
                            .when(enabled, |row| {
                                row.cursor_pointer()
                                    .track_focus(
                                        self.row_focus[index].as_ref().expect("enabled row focus"),
                                    )
                                    .when(self.selected == Some(index), |row| {
                                        row.bg(rgb(if danger { 0x57313b } else { 0x443953 }))
                                    })
                                    .hover(|row| {
                                        row.bg(rgb(if danger { 0x57313b } else { 0x443953 }))
                                    })
                            })
                            .child(div().flex_1().min_w_0().text_ellipsis().child(label))
                            .when_some(artwork, |row, artwork| row.child(icon(artwork, 16.)))
                            .when_some(checked, |row, checked| {
                                row.child(
                                    div()
                                        .size(px(16.))
                                        .flex_shrink_0()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(rgb(if checked { ACCENT } else { MUTED }))
                                        .when(checked, |box_| box_.bg(rgb(ACCENT)))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(12.))
                                        .text_color(rgb(TEXT))
                                        .child(if checked { "✓" } else { "" }),
                                )
                            })
                            .on_hover(cx.listener(move |menu, hovered: &bool, window, cx| {
                                if enabled && *hovered {
                                    menu.select(index, window, cx);
                                }
                            }))
                            .on_click(cx.listener(move |menu, event: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                if enabled && matches!(event, ClickEvent::Mouse(_)) {
                                    menu.activate(index, window, cx);
                                }
                            }))
                            .into_any_element()
                    }
                })
                .collect::<Vec<_>>();
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(origin)
                .anchor(Corner::TopLeft)
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .id("native-context-menu")
                        .track_focus(&self.focus)
                        .occlude()
                        .w(px(width))
                        .max_h(px(height))
                        .min_w_0()
                        .p_1()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(0x4c4458))
                        .bg(rgb(PANEL))
                        .shadow_lg()
                        .text_size(px(13.))
                        .on_mouse_down_out(
                            cx.listener(|menu, _, window, cx| menu.dismiss(window, cx)),
                        )
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .capture_key_down(cx.listener(Self::key_down))
                        .capture_key_up(cx.listener(Self::key_up))
                        .on_key_down(|_, _, cx| cx.stop_propagation())
                        .children(rows),
                ),
        )
        .with_priority(8)
        .into_any_element()
    }
}

fn valid_range(min: f32, max: f32, step: f32) -> bool {
    min.is_finite() && max.is_finite() && max > min && step.is_finite() && step > 0.
}
fn normalized_value(value: f32, min: f32, max: f32) -> f32 {
    if !min.is_finite() || !max.is_finite() || max <= min {
        return 0.;
    }
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        min
    }
}
fn selection(items: &[ContextMenuItem], current: Option<usize>, key: &str) -> Option<usize> {
    let choices = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| item.selectable().then_some(i))
        .collect::<Vec<_>>();
    if choices.is_empty() {
        return None;
    }
    let current = current.and_then(|current| choices.iter().position(|index| *index == current));
    let next = match (key, current) {
        ("home", _) | ("down", None) => 0,
        ("end", _) | ("up", None) => choices.len() - 1,
        ("up", Some(i)) => (i + choices.len() - 1) % choices.len(),
        ("down", Some(i)) => (i + 1) % choices.len(),
        (_, Some(i)) => i,
        _ => 0,
    };
    Some(choices[next])
}
fn placement(
    anchor: Point<Pixels>,
    viewport: Size<Pixels>,
    desired_height: f32,
) -> (Point<Pixels>, f32, f32) {
    let available_width = (f32::from(viewport.width) - 16.).max(1.);
    let available_height = (f32::from(viewport.height) - 16.).max(1.);
    let width = available_width.min(248.);
    let height = available_height.min(desired_height.max(1.)).min(360.);
    let x = f32::from(anchor.x);
    let y = f32::from(anchor.y);
    let x = if x.is_finite() { x } else { 8. };
    let y = if y.is_finite() { y } else { 8. };
    (
        point(
            px(x.clamp(8., (f32::from(viewport.width) - width - 8.).max(8.))),
            px(y.clamp(8., (f32::from(viewport.height) - height - 8.).max(8.))),
        ),
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::{ContextMenuItem, normalized_value, placement, selection};
    use gpui::{point, px, size};
    fn action(enabled: bool) -> ContextMenuItem {
        ContextMenuItem::Action {
            id: "action".into(),
            label: "Action".into(),
            icon: None,
            enabled,
            danger: false,
        }
    }
    #[test]
    fn keyboard_navigation_skips_headers_separators_and_disabled_rows() {
        let items = vec![
            ContextMenuItem::Header("Voice".into()),
            action(false),
            action(true),
            ContextMenuItem::Separator,
            ContextMenuItem::Slider {
                id: "gain".into(),
                label: "Volume".into(),
                value: 100.,
                min: 0.,
                max: 200.,
                step: 1.,
            },
        ];
        assert_eq!(selection(&items, None, "home"), Some(2));
        assert_eq!(selection(&items, Some(2), "down"), Some(4));
        assert_eq!(selection(&items, Some(4), "down"), Some(2));
        assert_eq!(selection(&items, Some(2), "up"), Some(4));
        assert_eq!(selection(&items, None, "end"), Some(4));
        assert_eq!(selection(&[action(false)], None, "home"), None);
    }
    #[test]
    fn pointer_anchor_fits_small_viewport_and_scrolls_tall_menu() {
        let (origin, width, height) =
            placement(point(px(790.), px(590.)), size(px(800.), px(600.)), 900.);
        assert_eq!(origin, point(px(544.), px(232.)));
        assert_eq!((width, height), (248., 360.));
        let (origin, width, height) =
            placement(point(px(-20.), px(-30.)), size(px(200.), px(100.)), 900.);
        assert_eq!(origin, point(px(8.), px(8.)));
        assert_eq!((width, height), (184., 84.));
    }
    #[test]
    fn invalid_slider_metadata_cannot_create_a_keyboard_stop_or_nan_style() {
        let item = ContextMenuItem::Slider {
            id: "bad".into(),
            label: "Bad".into(),
            value: f32::NAN,
            min: 0.,
            max: 200.,
            step: 0.,
        };
        assert_eq!(selection(&[item], None, "home"), None);
        assert_eq!(normalized_value(f32::NAN, 0., 200.), 0.);
        assert_eq!(normalized_value(300., 0., 200.), 200.);
    }
}
