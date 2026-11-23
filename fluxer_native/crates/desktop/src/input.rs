// Adapted from GPUI 0.2.2 input example (Apache-2.0), Zed Industries.
use crate::context_menu::{ContextMenu, ContextMenuEvent, ContextMenuItem};
use crate::message_markup::{MessageDocument, MessageTarget};
use crate::timeline::{TextPosition, TextSelection, TranscriptRange};
use std::ops::Range;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
};
#[path = "code_highlight.rs"]
mod code_highlight;
#[path = "composer_completion.rs"]
pub mod composer_completion;
#[path = "composer_tokens.rs"]
mod composer_tokens;
#[path = "inline_emoji.rs"]
mod inline_emoji;
#[path = "message_text.rs"]
mod message_text;

/// Clear retained syntax source when the active account changes.
pub(crate) fn clear_code_highlight_cache() {
    code_highlight::clear_cache();
}

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div,
    fill, point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::*;

#[derive(Default)]
struct CopyFeedback {
    range: Option<Range<usize>>,
    generation: u64,
}
impl CopyFeedback {
    fn begin(&mut self, range: Range<usize>) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.range = Some(range);
        self.generation
    }
    fn clear_if_current(&mut self, generation: u64) -> bool {
        if self.generation != generation || self.range.is_none() {
            return false;
        }
        self.range = None;
        true
    }
    fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.range = None;
    }
}
fn table_drag_is_active(drag: &mut Option<usize>, pressed: Option<MouseButton>) -> bool {
    if pressed != Some(MouseButton::Left) {
        *drag = None;
    }
    drag.is_some()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditSnapshot {
    text: SharedString,
    selection: Range<usize>,
    reversed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    Insert,
    Backspace,
    Delete,
    Atomic,
}
#[derive(Default)]
struct EditHistory {
    undo: Vec<EditSnapshot>,
    redo: Vec<EditSnapshot>,
    group: Option<(EditKind, EditSnapshot, std::time::Instant)>,
}
impl EditHistory {
    fn record(&mut self, before: EditSnapshot, after: EditSnapshot, kind: EditKind) {
        self.record_at(before, after, kind, std::time::Instant::now());
    }
    fn record_at(
        &mut self,
        before: EditSnapshot,
        after: EditSnapshot,
        kind: EditKind,
        now: std::time::Instant,
    ) {
        if before.text == after.text {
            return;
        }
        let joins = kind != EditKind::Atomic
            && self.group.as_ref().is_some_and(|(last_kind, last, time)| {
                *last_kind == kind
                    && last == &before
                    && now.duration_since(*time) < std::time::Duration::from_millis(750)
            });
        if !joins {
            self.undo.push(before);
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.group = Some((kind, after, now));
    }
    fn undo(&mut self, current: EditSnapshot) -> Option<EditSnapshot> {
        self.group = None;
        let previous = self.undo.pop()?;
        self.redo.push(current);
        Some(previous)
    }
    fn redo(&mut self, current: EditSnapshot) -> Option<EditSnapshot> {
        self.group = None;
        let next = self.redo.pop()?;
        self.undo.push(current);
        Some(next)
    }
}

fn edit_kind(before: &EditSnapshot, range: &Range<usize>, inserted: &str) -> EditKind {
    if before.selection.is_empty() {
        if range.is_empty() && inserted.graphemes(true).count() == 1 && inserted != "\n" {
            return EditKind::Insert;
        }
        if inserted.is_empty() && !range.is_empty() {
            if range.end == before.selection.start {
                return EditKind::Backspace;
            }
            if range.start == before.selection.start {
                return EditKind::Delete;
            }
        }
    }
    EditKind::Atomic
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputMenuAction {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
}
#[derive(Default)]
struct InputMenuState {
    read_only: bool,
    secret: bool,
    selected: bool,
    selectable: bool,
    undo: bool,
    redo: bool,
    paste: bool,
}
impl InputMenuAction {
    fn id(self) -> &'static str {
        match self {
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Cut => "cut",
            Self::Copy => "copy",
            Self::Paste => "paste",
            Self::Delete => "delete",
            Self::SelectAll => "select-all",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::Cut => "Cut",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::Delete => "Delete",
            Self::SelectAll => "Select all",
        }
    }
    fn visible(self, state: &InputMenuState) -> bool {
        match self {
            Self::Copy => !state.secret,
            Self::Cut => !state.secret && !state.read_only,
            Self::SelectAll => true,
            _ => !state.read_only,
        }
    }
    fn enabled(self, state: &InputMenuState) -> bool {
        self.visible(state)
            && match self {
                Self::Undo => state.undo,
                Self::Redo => state.redo,
                Self::Cut | Self::Copy | Self::Delete => state.selected,
                Self::Paste => state.paste,
                Self::SelectAll => state.selectable,
            }
    }
}
const INPUT_MENU_ACTIONS: [InputMenuAction; 7] = [
    InputMenuAction::Undo,
    InputMenuAction::Redo,
    InputMenuAction::Cut,
    InputMenuAction::Copy,
    InputMenuAction::Paste,
    InputMenuAction::Delete,
    InputMenuAction::SelectAll,
];

fn input_menu_items(state: &InputMenuState) -> Vec<ContextMenuItem> {
    let mut items = Vec::new();
    let mut previous_group = None;
    for action in INPUT_MENU_ACTIONS {
        if !action.visible(state) {
            continue;
        }
        let group = match action {
            InputMenuAction::Undo | InputMenuAction::Redo => 0,
            InputMenuAction::SelectAll => 2,
            _ => 1,
        };
        if previous_group.is_some_and(|previous| previous != group) {
            items.push(ContextMenuItem::Separator);
        }
        previous_group = Some(group);
        items.push(ContextMenuItem::Action {
            id: action.id().into(),
            label: action.label().into(),
            icon: None,
            enabled: action.enabled(state),
            danger: false,
        });
    }
    items
}

fn context_click_selection(selection: &Range<usize>, index: usize) -> Option<Range<usize>> {
    // Secondary-clicking inside a selection must not destroy Cut/Copy's target.
    (selection.is_empty() || index < selection.start || index > selection.end)
        .then_some(index..index)
}

pub struct ImagePasted {
    pub content_type: &'static str,
    pub extension: &'static str,
    pub bytes: std::sync::Arc<Vec<u8>>,
}

enum PastePayload {
    Text(String),
    Image(ImagePasted),
    Empty,
}

fn paste_payload(item: ClipboardItem, allow_image: bool) -> PastePayload {
    let text = item.text();
    if allow_image {
        for entry in item.into_entries() {
            if let gpui::ClipboardEntry::Image(image) = entry
                && !image.bytes.is_empty()
            {
                let extension = match image.format {
                    gpui::ImageFormat::Png => "png",
                    gpui::ImageFormat::Jpeg => "jpg",
                    gpui::ImageFormat::Webp => "webp",
                    gpui::ImageFormat::Gif => "gif",
                    gpui::ImageFormat::Svg => "svg",
                    gpui::ImageFormat::Bmp => "bmp",
                    gpui::ImageFormat::Tiff => "tiff",
                };
                return PastePayload::Image(ImagePasted {
                    content_type: image.format.mime_type(),
                    extension,
                    bytes: std::sync::Arc::new(image.bytes),
                });
            }
        }
    }
    text.map_or(PastePayload::Empty, PastePayload::Text)
}

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectUp,
        SelectDown,
        SelectHome,
        SelectEnd,
        DocumentHome,
        DocumentEnd,
        SelectDocumentHome,
        SelectDocumentEnd,
        Newline,
        SelectLeft,
        SelectRight,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Undo,
        Redo,
        OpenTextContextMenu,
        ActivateInline,
        Quit,
    ]
);

pub struct TextInput {
    context_menu: Option<Entity<ContextMenu>>,
    context_menu_generation: u64,
    history: EditHistory,
    composer_labels: std::collections::BTreeMap<String, String>,
    pub completion_choices: Vec<composer_completion::Choice>,
    completion_index: usize,
    pub completion_enter_guard: bool,
    completion_dismissed: Option<composer_completion::Trigger>,
    message_document: Option<Arc<MessageDocument>>,
    timeline_selection: Option<(fluxer_core::Snowflake, Rc<RefCell<TextSelection>>)>,
    highlighted: Vec<code_highlight::Token>,
    highlight_generation: u64,
    highlight_ticket: Option<code_highlight::Ticket>,
    copy_feedback: CopyFeedback,
    table_scrolls: BTreeMap<usize, Pixels>,
    table_drag: Option<usize>,
    emoji_cache: Entity<message_text::EmojiCache>,
    emoji_reduced_motion: bool,
    emoji_hovered: bool,
    revealed_spoilers: BTreeSet<u32>,
    inline_press: Option<(Point<Pixels>, usize, bool)>,
    inline_focus: Option<Range<usize>>,
    inline_reveal: bool,
    inline_pointer_focus: bool,
    inline_visible_only: bool,
    hover_interactive: bool,
    focus_handle: FocusHandle,
    pub content: SharedString,
    pub secret: bool,
    pub multiline: bool,
    pub chat_picker: bool,
    pub attachment_paste: bool,
    read_only: bool,
    selection_origin: Option<Range<usize>>,
    selection_clicks: usize,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<DocumentLayout>,
    last_bounds: Option<Bounds<Pixels>>,
    last_visible_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    scroll_x: Pixels,
    scroll_y: Pixels,
    rows: usize,
    reveal_cursor: bool,
    preferred_x: Option<Pixels>,
}

impl gpui::EventEmitter<ImagePasted> for TextInput {}
pub struct MessageActivated {
    pub target: MessageTarget,
    /// Visible glyph bounds in window coordinates, for pointer and keyboard activation.
    pub anchor: Option<Bounds<Pixels>>,
}
impl gpui::EventEmitter<MessageActivated> for TextInput {}

impl TextInput {
    pub fn new(placeholder: &str, cx: &mut Context<Self>) -> Self {
        Self {
            context_menu: None,
            context_menu_generation: 0,
            history: EditHistory::default(),
            composer_labels: Default::default(),
            completion_choices: Vec::new(),
            completion_index: 0,
            completion_enter_guard: false,
            completion_dismissed: None,
            message_document: None,
            timeline_selection: None,
            highlighted: Vec::new(),
            highlight_generation: 0,
            highlight_ticket: None,
            copy_feedback: CopyFeedback::default(),
            table_scrolls: BTreeMap::new(),
            table_drag: None,
            emoji_cache: message_text::EmojiCache::new(cx),
            emoji_reduced_motion: false,
            emoji_hovered: false,
            revealed_spoilers: BTreeSet::new(),
            inline_press: None,
            inline_focus: None,
            inline_reveal: false,
            inline_pointer_focus: false,
            inline_visible_only: false,
            hover_interactive: false,
            focus_handle: cx.focus_handle().tab_stop(true),
            content: "".into(),
            secret: false,
            multiline: false,
            chat_picker: false,
            attachment_paste: false,
            read_only: false,
            selection_origin: None,
            selection_clicks: 1,
            placeholder: placeholder.to_owned().into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            last_visible_bounds: None,
            is_selecting: false,
            scroll_x: px(0.),
            scroll_y: px(0.),
            rows: 1,
            reveal_cursor: true,
            preferred_x: None,
        }
    }
    /// A transparent, wrapping message body with normal selection and copy.
    /// No text input handler is registered, so typing/IME cannot edit a message.
    pub fn read_only(text: &str, cx: &mut Context<Self>) -> Self {
        let mut input = Self::new("", cx);
        input.multiline = true;
        input.read_only = true;
        input.focus_handle = input.focus_handle.clone().tab_stop(false);
        input.content = text.to_owned().into();
        input
    }

    pub fn read_only_document(
        document: impl Into<Arc<MessageDocument>>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document = document.into();
        let mut input = Self::read_only(&document.text, cx);
        input.focus_handle = input
            .focus_handle
            .clone()
            .tab_stop(!inline_targets(&document, &input.revealed_spoilers).is_empty());
        input.message_document = Some(document.clone());
        let focus = input.focus_handle.clone();
        cx.defer_in(window, move |_, window, cx| {
            cx.on_focus(&focus, window, |this, window, cx| {
                if std::mem::take(&mut this.inline_pointer_focus) || this.timeline_range().is_some()
                {
                    return;
                }
                if this.inline_focus.is_some() {
                    return;
                }
                let targets = this.keyboard_inline_targets();
                let target = if window.modifiers().shift {
                    targets.last()
                } else {
                    targets.first()
                };
                if let Some(range) = target {
                    let range = range.clone();
                    this.move_to(range.start, cx);
                    this.inline_focus = Some(range);
                    this.queue_inline_reveal(cx);
                } else if this.inline_visible_only {
                    if window.modifiers().shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                }
            })
            .detach();
        });
        code_highlight::schedule(&mut input, document, cx);
        input
    }
    pub fn set_emoji_reduced_motion(&mut self, value: bool) {
        self.emoji_reduced_motion = value;
    }
    pub fn update_message_document(
        &mut self,
        document: impl Into<Arc<MessageDocument>>,
        cx: &mut Context<Self>,
    ) {
        let document = document.into();
        if self
            .message_document
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &document) || old == &document)
        {
            return;
        }
        self.reset_context_menu(cx);
        let text_changed = self.content.as_ref() != document.text;
        if self
            .message_document
            .as_ref()
            .is_none_or(|old| old.source != document.source || old.text != document.text)
        {
            self.revealed_spoilers.clear();
        }
        if text_changed {
            self.table_scrolls.clear();
            self.content = document.text.clone().into();
            self.selected_range = 0..0;
            self.selection_reversed = false;
            self.selection_origin = None;
        }
        self.inline_focus = None;
        self.focus_handle = self
            .focus_handle
            .clone()
            .tab_stop(!inline_targets(&document, &self.revealed_spoilers).is_empty());
        self.emoji_cache = message_text::EmojiCache::new(cx);
        self.copy_feedback.reset();
        self.message_document = Some(document.clone());
        code_highlight::schedule(self, document, cx);
        self.inline_press = None;
        self.hover_interactive = false;
        self.last_layout = None;
        cx.notify();
    }
    fn inline_tab(&mut self, event: &gpui::KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "tab"
            || event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
            || event.keystroke.modifiers.platform
        {
            return;
        }
        let targets = self.keyboard_inline_targets();
        let next = if let Some(current) = &self.inline_focus {
            if event.keystroke.modifiers.shift {
                targets
                    .iter()
                    .rev()
                    .find(|target| target.end <= current.start)
            } else {
                targets.iter().find(|target| target.start >= current.end)
            }
        } else if event.keystroke.modifiers.shift {
            targets
                .iter()
                .rev()
                .find(|target| target.start <= self.cursor_offset())
        } else {
            targets
                .iter()
                .find(|target| target.start >= self.cursor_offset())
        };
        if let Some(target) = next {
            let target = target.clone();
            self.move_to(target.start, cx);
            self.inline_focus = Some(target);
            self.queue_inline_reveal(cx);
            cx.stop_propagation();
        } else {
            self.inline_focus = None;
        }
    }
    pub fn set_inline_visible_only(&mut self) {
        self.inline_visible_only = true;
    }
    fn keyboard_inline_targets(&self) -> Vec<Range<usize>> {
        let Some(document) = self.message_document.as_ref() else {
            return Vec::new();
        };
        let targets = inline_targets(document, &self.revealed_spoilers);
        if !self.inline_visible_only {
            return targets;
        }
        let (Some(layout), Some(bounds), Some(visible)) = (
            &self.last_layout,
            self.last_bounds,
            self.last_visible_bounds,
        ) else {
            return Vec::new();
        };
        targets
            .into_iter()
            .filter(|range| {
                inline_target_line(layout, range).is_some_and(|(line, left, right)| {
                    let rect = Bounds::new(
                        point(
                            bounds.left() + left - self.scroll_x,
                            bounds.top() + line.y - self.scroll_y,
                        ),
                        size((right - left).max(px(1.)), line.height),
                    );
                    rect.intersects(&visible)
                })
            })
            .collect()
    }
    fn queue_inline_reveal(&mut self, cx: &mut Context<Self>) {
        self.inline_reveal = true;
        cx.notify();
    }
    fn activate_inline(&mut self, _: &ActivateInline, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.activate_inline_at(self.cursor_offset(), true, cx);
        }
    }
    fn activate_inline_at(&mut self, index: usize, keyboard: bool, cx: &mut Context<Self>) {
        let Some(document) = self.message_document.as_ref() else {
            return;
        };
        if let Some(id) = message_text::hidden_at(document, &self.revealed_spoilers, index) {
            self.revealed_spoilers.insert(id);
            self.inline_focus = None;
            self.last_layout = None;
            cx.notify();
            return;
        }
        if let Some(target) = message_text::target_at(document, &self.revealed_spoilers, index) {
            self.inline_focus = inline_targets(document, &self.revealed_spoilers)
                .into_iter()
                .find(|range| range.contains(&index));
            let anchor =
                self.last_bounds
                    .zip(self.last_layout.as_ref())
                    .and_then(|(bounds, layout)| {
                        let span = document.spans.iter().find(|span| {
                            span.range.contains(&index) && span.target.as_ref() == Some(&target)
                        })?;
                        let visible = self.last_visible_bounds?;
                        layout
                            .lines
                            .iter()
                            .filter(|line| line.range.contains(&index))
                            .find_map(|line| {
                                let start = line.range.start.max(span.range.start);
                                let end = line.range.end.min(span.range.end);
                                if start >= end {
                                    return None;
                                }
                                let left = bounds.left()
                                    + line.indent
                                    + line.layout.x_for_index(start - line.range.start)
                                    - self.scroll_x;
                                let right = bounds.left()
                                    + line.indent
                                    + line.layout.x_for_index(end - line.range.start)
                                    - self.scroll_x;
                                let rect = Bounds::new(
                                    point(left, bounds.top() + line.y - self.scroll_y),
                                    size((right - left).max(px(1.)), line.height),
                                );
                                rect.intersects(&visible).then(|| rect.intersect(&visible))
                            })
                    });
            if keyboard && anchor.is_none() {
                self.queue_inline_reveal(cx);
                return;
            }
            cx.emit(MessageActivated { target, anchor });
        }
    }

    fn copy_code(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let Some(document) = self.message_document.as_ref() else {
            return;
        };
        let Some(code) = message_text::code_for_copy(document, &range, &self.revealed_spoilers)
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(code.to_owned()));
        let generation = self.copy_feedback.begin(range);
        cx.notify();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |entity, cx| {
            executor.timer(std::time::Duration::from_secs(2)).await;
            let _ = entity.update(cx, |input, cx| {
                if input.copy_feedback.clear_if_current(generation) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn set_composer_labels(
        &mut self,
        labels: std::collections::BTreeMap<String, String>,
        cx: &mut Context<Self>,
    ) {
        if self.composer_labels != labels {
            self.composer_labels = labels;
            self.last_layout = None;
            cx.notify();
        }
    }
    fn composer_tokens(&self) -> Vec<composer_tokens::Token> {
        composer_tokens::tokens(&self.content, &self.composer_labels)
    }
    pub fn completion_trigger(&self) -> Option<composer_completion::Trigger> {
        let trigger = composer_completion::trigger(
            &self.content,
            self.selected_range.clone(),
            self.marked_range.is_some(),
        )?;
        (self.completion_dismissed.as_ref() != Some(&trigger)).then_some(trigger)
    }
    pub fn is_composing(&self) -> bool {
        self.marked_range.is_some()
    }
    pub fn set_completion_choices(
        &mut self,
        choices: Vec<composer_completion::Choice>,
        cx: &mut Context<Self>,
    ) {
        if self.completion_choices != choices {
            self.completion_choices = choices;
            self.completion_index = 0;
            cx.notify();
        }
    }
    pub fn completion_key(
        &mut self,
        key: &str,
        held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(trigger) = self.completion_trigger() else {
            return false;
        };
        if self.completion_choices.is_empty() {
            return false;
        }
        match key {
            "up" => {
                self.completion_index = self
                    .completion_index
                    .checked_sub(1)
                    .unwrap_or(self.completion_choices.len() - 1)
            }
            "down" => {
                self.completion_index = (self.completion_index + 1) % self.completion_choices.len()
            }
            "escape" => {
                self.completion_dismissed = Some(trigger);
                self.completion_choices.clear();
            }
            "enter" | "tab" => {
                if held {
                    return true;
                }
                self.completion_enter_guard = key == "enter";
                let insert = format!("{} ", self.completion_choices[self.completion_index].insert);
                self.history.group = None;
                let before = self.snapshot();
                self.selected_range = trigger.range;
                self.replace_text_in_range(None, &insert, window, cx);
                // Preserve the pre-completion caret, not the internal replacement selection.
                if let Some(last) = self.history.undo.last_mut() {
                    *last = before;
                }
                self.history.group = None;
                self.completion_choices.clear();
            }
            _ => return false,
        }
        cx.notify();
        true
    }
    pub fn insert_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.history.group = None;
        self.replace_text_in_range(None, text, window, cx);
        self.history.group = None;
    }

    pub(super) fn append_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
        self.insert_text(text, window, cx);
    }

    pub fn set(&mut self, text: String, cx: &mut Context<Self>) {
        self.reset_context_menu(cx);
        // Switching drafts and sending are history boundaries. A programmatic
        // reset must never resurrect a sent message or another channel's draft.
        self.history = EditHistory::default();
        self.content = normalize_insert(&text, self.multiline).into();
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.reveal_cursor = true;
        self.preferred_x = None;
        cx.notify();
    }

    pub fn context_menu_open(&self, cx: &App) -> bool {
        self.context_menu
            .as_ref()
            .is_some_and(|menu| menu.read(cx).is_open())
    }

    pub(super) fn reset_context_menu(&mut self, cx: &mut Context<Self>) {
        self.context_menu_generation = self.context_menu_generation.wrapping_add(1);
        if let Some(menu) = &self.context_menu {
            menu.update(cx, |menu, cx| menu.reset(cx));
        }
    }

    fn input_menu_state(&self, cx: &mut Context<Self>) -> InputMenuState {
        let paste = !self.read_only
            && cx.read_from_clipboard().is_some_and(|item| {
                match paste_payload(item, self.attachment_paste && !self.secret) {
                    PastePayload::Text(text) => !text.is_empty(),
                    PastePayload::Image(_) => true,
                    PastePayload::Empty => false,
                }
            });
        InputMenuState {
            read_only: self.read_only,
            secret: self.secret,
            selected: !self.selected_range.is_empty(),
            selectable: !self.content.is_empty() && self.selected_range != (0..self.content.len()),
            undo: !self.history.undo.is_empty(),
            redo: !self.history.redo.is_empty(),
            paste,
        }
    }

    fn on_context_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Rich message bodies belong to the enclosing message action menu.
        if self.message_document.is_some() {
            return;
        }
        let index = self.index_for_mouse_position(event.position);
        if let Some(selection) = context_click_selection(&self.selected_range, index) {
            self.move_to(selection.start, cx);
        }
        self.open_input_menu(event.position, window, cx);
        cx.stop_propagation();
    }

    fn open_text_context_menu(
        &mut self,
        _: &OpenTextContextMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.message_document.is_some() {
            return;
        }
        let anchor = self
            .last_bounds
            .map(|bounds| {
                let caret = self
                    .last_layout
                    .as_ref()
                    .map(|layout| layout.position_for_index(self.cursor_offset()))
                    .unwrap_or_default();
                point(
                    (bounds.left() + caret.x - self.scroll_x).clamp(bounds.left(), bounds.right()),
                    (bounds.top() + caret.y - self.scroll_y + window.line_height())
                        .clamp(bounds.top(), bounds.bottom()),
                )
            })
            .unwrap_or_default();
        self.open_input_menu(anchor, window, cx);
        cx.stop_propagation();
    }

    fn open_input_menu(
        &mut self,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history.group = None;
        self.marked_range = None;
        self.is_selecting = false;
        self.inline_press = None;
        self.selection_origin = None;
        window.focus(&self.focus_handle);
        let items = input_menu_items(&self.input_menu_state(cx));
        let snapshot = self.snapshot();
        self.context_menu_generation = self.context_menu_generation.wrapping_add(1);
        let generation = self.context_menu_generation;
        let owner = cx.entity().downgrade();
        let menu = self
            .context_menu
            .get_or_insert_with(|| cx.new(ContextMenu::new))
            .clone();
        menu.update(cx, |menu, cx| {
            menu.open(
                items,
                anchor,
                std::rc::Rc::new(move |event, window, cx| {
                    if let ContextMenuEvent::Action(id) = event {
                        let _ = owner.update(cx, |input, cx| {
                            // Draft switches and programmatic resets invalidate queued actions.
                            if input.context_menu_generation != generation
                                || input.content != snapshot.text
                            {
                                return;
                            }
                            input.selected_range = snapshot.selection.clone();
                            input.selection_reversed = snapshot.reversed;
                            if let Some(action) = INPUT_MENU_ACTIONS
                                .iter()
                                .copied()
                                .find(|action| action.id() == id)
                            {
                                input.run_input_menu_action(action, window, cx);
                            }
                        });
                    }
                }),
                window,
                cx,
            )
        });
        cx.notify();
    }

    fn run_input_menu_action(
        &mut self,
        action: InputMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !action.enabled(&self.input_menu_state(cx)) {
            return;
        }
        window.focus(&self.focus_handle);
        match action {
            InputMenuAction::Undo => self.undo(&Undo, window, cx),
            InputMenuAction::Redo => self.redo(&Redo, window, cx),
            InputMenuAction::Cut => self.cut(&Cut, window, cx),
            InputMenuAction::Copy => self.copy(&Copy, window, cx),
            InputMenuAction::Paste => self.paste(&Paste, window, cx),
            InputMenuAction::Delete => {
                self.history.group = None;
                self.replace_text_in_range(None, "", window, cx);
                self.history.group = None;
            }
            InputMenuAction::SelectAll => self.select_all(&SelectAll, window, cx),
        }
    }

    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            text: self.content.clone(),
            selection: self.selected_range.clone(),
            reversed: self.selection_reversed,
        }
    }

    fn restore_edit(&mut self, snapshot: EditSnapshot, cx: &mut Context<Self>) {
        self.content = snapshot.text;
        self.selected_range = snapshot.selection;
        self.selection_reversed = snapshot.reversed;
        self.marked_range = None;
        self.selection_origin = None;
        self.last_layout = None;
        self.reveal_cursor = true;
        self.preferred_x = None;
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if !self.read_only {
            let current = self.snapshot();
            if let Some(snapshot) = self.history.undo(current) {
                self.restore_edit(snapshot, cx);
            }
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if !self.read_only {
            let current = self.snapshot();
            if let Some(snapshot) = self.history.redo(current) {
                self.restore_edit(snapshot, cx);
            }
        }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(word_offset(&self.content, self.cursor_offset(), false), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(word_offset(&self.content, self.cursor_offset(), true), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(word_offset(&self.content, self.cursor_offset(), false), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(word_offset(&self.content, self.cursor_offset(), true), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_boundary(false), cx);
    }
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_boundary(true), cx);
    }
    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.row_boundary(false), cx);
    }
    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.row_boundary(true), cx);
    }
    fn document_home(&mut self, _: &DocumentHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }
    fn document_end(&mut self, _: &DocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }
    fn select_document_home(
        &mut self,
        _: &SelectDocumentHome,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(0, cx);
    }
    fn select_document_end(
        &mut self,
        _: &SelectDocumentEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.content.len(), cx);
    }
    fn row_boundary(&self, end: bool) -> usize {
        self.last_layout
            .as_ref()
            .and_then(|layout| layout.lines.get(layout.row_for_index(self.cursor_offset())))
            .map(|row| if end { row.range.end } else { row.range.start })
            .unwrap_or(if end { self.content.len() } else { 0 })
    }
    fn up(&mut self, _: &Up, window: &mut Window, cx: &mut Context<Self>) {
        if !self.completion_key("up", false, window, cx) {
            self.vertical(-1, false, cx);
        }
    }
    fn down(&mut self, _: &Down, window: &mut Window, cx: &mut Context<Self>) {
        if !self.completion_key("down", false, window, cx) {
            self.vertical(1, false, cx);
        }
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1, true, cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1, true, cx);
    }
    fn vertical(&mut self, direction: isize, select: bool, cx: &mut Context<Self>) {
        let Some(layout) = self.last_layout.as_ref() else {
            return;
        };
        let cursor = self.cursor_offset();
        let row = layout.row_for_index(cursor);
        let x = self
            .preferred_x
            .unwrap_or_else(|| layout.position_for_index(cursor).x);
        let current_y = layout.lines[row].y;
        let next_y = layout
            .lines
            .iter()
            .map(|line| line.y)
            .filter(|y| {
                if direction < 0 {
                    *y < current_y
                } else {
                    *y > current_y
                }
            })
            .reduce(|a, b| if direction < 0 { a.max(b) } else { a.min(b) });
        let next = next_y
            .map(|y| layout.row_for_position(point(x, y)))
            .unwrap_or(row);
        let index = grapheme_offset(
            &self.content,
            layout.lines[next].range.start
                + layout.lines[next]
                    .layout
                    .closest_index_for_x(x - layout.lines[next].indent),
        );
        if select {
            self.select_to(index, cx);
        } else {
            self.move_to(index, cx);
        }
        self.preferred_x = Some(x);
    }
    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.replace_text_in_range(None, "\n", window, cx);
        }
    }
    fn scroll(&mut self, event: &gpui::ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            if let (Some(layout), Some(bounds)) = (&self.last_layout, self.last_bounds) {
                let local_y = event.position.y - bounds.top() + self.scroll_y;
                let delta = event.delta.pixel_delta(layout.line_height);
                let dx = if delta.x != px(0.) {
                    delta.x
                } else if event.modifiers.shift {
                    delta.y
                } else {
                    px(0.)
                };
                if bounds.contains(&event.position)
                    && dx != px(0.)
                    && let Some(table) = layout
                        .table_regions
                        .iter()
                        .find(|t| local_y >= t.y && local_y < t.y + t.height)
                {
                    let entry = self.table_scrolls.entry(table.start).or_default();
                    let next =
                        (*entry - dx).clamp(px(0.), (table.width - bounds.size.width).max(px(0.)));
                    if next != *entry {
                        *entry = next;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }
            }
            return;
        }
        if !self.multiline {
            return;
        }
        if let (Some(layout), Some(bounds)) = (&self.last_layout, self.last_bounds) {
            let max = (layout.height() - bounds.size.height).max(px(0.));
            self.scroll_y =
                (self.scroll_y - event.delta.pixel_delta(layout.line_height).y).clamp(px(0.), max);
            self.reveal_cursor = false;
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selected_range.is_empty() {
            self.previous_boundary(self.cursor_offset())..self.cursor_offset()
        } else {
            self.selected_range.clone()
        };
        self.replace_text_in_range(Some(self.range_to_utf16(&range)), "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selected_range.is_empty() {
            self.cursor_offset()..self.next_boundary(self.cursor_offset())
        } else {
            self.selected_range.clone()
        };
        self.replace_text_in_range(Some(self.range_to_utf16(&range)), "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.inline_pointer_focus = !self.focus_handle.is_focused(window);
        window.focus(&self.focus_handle);
        self.inline_reveal = false;
        if let (Some(layout), Some(bounds)) = (&self.last_layout, self.last_bounds) {
            let local_y = event.position.y - bounds.top() + self.scroll_y;
            if let Some(table) = layout.table_regions.iter().find(|table| {
                table.width > table.viewport
                    && local_y >= table.y + table.height - px(8.)
                    && local_y < table.y + table.height
            }) {
                self.table_drag = Some(table.start);
                self.drag_table_scroll(event.position, cx);
                cx.stop_propagation();
                return;
            }
        }
        self.is_selecting = true;

        let index = self.index_for_mouse_position(event.position);
        if let Some((message, selection)) = &self.timeline_selection
            && event.click_count == 1
            && !event.modifiers.shift
        {
            selection.borrow_mut().begin(TextPosition {
                message: *message,
                offset: index,
            });
        }
        self.inline_press = if self.message_document.is_some()
            && event.click_count == 1
            && !event.modifiers.shift
        {
            self.inline_index_for_mouse_position(event.position)
                .map(|index| (event.position, index, false))
        } else {
            None
        };
        self.selection_clicks = event.click_count;
        self.selection_origin = None;
        if event.modifiers.shift {
            self.select_to(index, cx);
        } else if event.click_count >= 2 {
            let range = self.selection_unit(index, event.click_count);
            self.move_to(range.start, cx);
            self.select_to(range.end, cx);
            self.selection_origin = Some(range);
        } else {
            self.move_to(index, cx)
        }
        cx.stop_propagation();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.table_drag.take().is_some() {
            cx.stop_propagation();
            return;
        }
        self.is_selecting = false;
        if let Some((origin, index, dragged)) = self.inline_press.take()
            && !dragged
            && self.selected_range.is_empty()
            && !event.modifiers.shift
            && (event.position.x - origin.x).abs() <= px(3.)
            && (event.position.y - origin.y).abs() <= px(3.)
            && self.inline_index_for_mouse_position(event.position) == Some(index)
        {
            self.activate_inline_at(index, false, cx);
        }
    }

    fn drag_table_scroll(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(id), Some(layout), Some(bounds)) =
            (self.table_drag, &self.last_layout, self.last_bounds)
        else {
            return;
        };
        let Some(table) = layout.table_regions.iter().find(|table| table.start == id) else {
            return;
        };
        let thumb = (table.viewport * (table.viewport / table.width))
            .max(px(20.))
            .min(table.viewport);
        let travel = table.viewport - thumb;
        let ratio = if travel > px(0.) {
            ((position.x - bounds.left() - thumb / 2.) / travel).clamp(0., 1.)
        } else {
            0.
        };
        self.table_scrolls
            .insert(id, (table.width - table.viewport) * ratio);
        cx.notify();
    }
    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.table_drag.is_some() {
            if !table_drag_is_active(&mut self.table_drag, event.pressed_button) {
                return;
            }
            self.drag_table_scroll(event.position, cx);
            cx.stop_propagation();
            return;
        }
        if let Some((origin, _, dragged)) = &mut self.inline_press
            && ((event.position.x - origin.x).abs() > px(3.)
                || (event.position.y - origin.y).abs() > px(3.))
        {
            *dragged = true;
        }
        let hover = self
            .message_document
            .as_ref()
            .zip(self.inline_index_for_mouse_position(event.position))
            .is_some_and(|(document, index)| {
                !self.is_selecting
                    && (message_text::hidden_at(document, &self.revealed_spoilers, index).is_some()
                        || message_text::target_at(document, &self.revealed_spoilers, index)
                            .is_some())
            });
        if hover != self.hover_interactive {
            self.hover_interactive = hover;
            cx.notify();
        }
        if self.is_selecting && event.pressed_button == Some(MouseButton::Left) {
            let index = self.index_for_mouse_position(event.position);
            if let Some(origin) = self.selection_origin.clone() {
                let unit = self.selection_unit(index, self.selection_clicks);
                if index < origin.start {
                    self.move_to(origin.end, cx);
                    self.select_to(unit.start, cx);
                } else {
                    self.move_to(origin.start, cx);
                    self.select_to(unit.end, cx);
                }
            } else {
                self.select_to(index, cx);
            }
        } else if event.pressed_button != Some(MouseButton::Left) {
            self.is_selecting = false;
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        self.history.group = None;
        if let Some(item) = cx.read_from_clipboard() {
            match paste_payload(item, self.attachment_paste && !self.secret) {
                PastePayload::Text(text) => self.replace_text_in_range(None, &text, window, cx),
                PastePayload::Image(image) => cx.emit(image),
                PastePayload::Empty => {}
            }
        }
        self.history.group = None;
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.timeline_range().is_some() {
            cx.propagate();
            return;
        }
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        if self.secret || self.selected_range.is_empty() {
            return None;
        }
        let range = self.selected_range.clone();
        Some(if let Some(document) = self.message_document.as_ref() {
            if message_text::valid(document) {
                document.copy_range(range, &self.revealed_spoilers)
            } else {
                String::new()
            }
        } else {
            self.content[range].to_string()
        })
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only || self.secret {
            return;
        }
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.clear_completed_timeline_selection(cx);
        self.inline_focus = None;
        let offset = composer_tokens::snap(&self.composer_tokens(), offset);
        self.history.group = None;
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.reveal_cursor = true;
        self.preferred_x = None;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    pub(super) fn set_timeline_selection(
        &mut self,
        message: fluxer_core::Snowflake,
        selection: Rc<RefCell<TextSelection>>,
    ) {
        self.timeline_selection = Some((message, selection));
    }

    fn timeline_range(&self) -> Option<TranscriptRange> {
        self.timeline_selection.as_ref()?.1.borrow().transcript()
    }

    fn clear_completed_timeline_selection(&self, cx: &mut Context<Self>) {
        if let Some((_, selection)) = &self.timeline_selection {
            let mut selection = selection.borrow_mut();
            if !selection.dragging && selection.clear() {
                cx.refresh_windows();
            }
        }
    }

    pub(super) fn copy_transcript(&self, range: TranscriptRange) -> String {
        let Some((message, _)) = self.timeline_selection.as_ref() else {
            return String::new();
        };
        self.message_document
            .as_ref()
            .map_or_else(String::new, |document| {
                document.copy_transcript(*message, range, &self.revealed_spoilers)
            })
    }

    pub(super) fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(layout)) = (self.last_bounds, self.last_layout.as_ref()) else {
            return 0;
        };
        let local = point(
            position.x - bounds.left() + self.scroll_x,
            position.y - bounds.top() + self.scroll_y,
        );
        grapheme_offset(&self.content, layout.index_for_position(local))
    }
    fn inline_index_for_mouse_position(&self, position: Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let layout = self.last_layout.as_ref()?;
        if !bounds.contains(&position) {
            return None;
        }
        let local = point(
            position.x - bounds.left() + self.scroll_x,
            position.y - bounds.top() + self.scroll_y,
        );
        if local.y < px(0.) || local.y >= layout.height() {
            return None;
        }
        let row = layout.row_for_position(local);
        let line = &layout.lines[row];
        if local.x < line.indent || local.x > line.indent + line.layout.width {
            return None;
        }
        Some(grapheme_offset(
            &self.content,
            line.range.start + line.layout.index_for_x(local.x - line.indent)?,
        ))
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.clear_completed_timeline_selection(cx);
        self.inline_focus = None;
        let offset = composer_tokens::snap(&self.composer_tokens(), offset);
        self.history.group = None;
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.reveal_cursor = true;
        self.preferred_x = None;
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        utf16_offset(&self.content, offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn selection_unit(&self, index: usize, clicks: usize) -> Range<usize> {
        let tokens = self.composer_tokens();
        if clicks == 2
            && let Some(token) = tokens
                .iter()
                .find(|token| index >= token.source.start && index <= token.source.end)
        {
            return token.source.clone();
        }
        composer_tokens::edit_range(&tokens, selection_unit(&self.content, index, clicks))
    }
    fn previous_boundary(&self, offset: usize) -> usize {
        if let Some(token) = self
            .composer_tokens()
            .iter()
            .find(|token| offset > token.source.start && offset <= token.source.end)
        {
            return token.source.start;
        }
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        if let Some(token) = self
            .composer_tokens()
            .iter()
            .find(|token| offset >= token.source.start && offset < token.source.end)
        {
            return token.source.end;
        }
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let new_text = normalize_insert(new_text, self.multiline);
        let new_text = new_text.as_str();
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let tokens = self.composer_tokens();
        let range = composer_tokens::edit_range(&tokens, range);
        let before = self.snapshot();
        let composing = self.marked_range.is_some();
        let kind = if tokens
            .iter()
            .any(|token| token.source.start < range.end && token.source.end > range.start)
        {
            EditKind::Atomic
        } else {
            edit_kind(&before, &range, new_text)
        };
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        if !composing {
            self.history.record(before, self.snapshot(), kind);
        } else {
            self.history.group = None;
        }
        self.reveal_cursor = true;
        self.preferred_x = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let new_text = normalize_insert(new_text, self.multiline);
        let new_text = new_text.as_str();
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let range = composer_tokens::edit_range(&self.composer_tokens(), range);
        let before = self.snapshot();
        let composing = self.marked_range.is_some();
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range =
            marked_selection(range.start, new_text, new_selected_range_utf16.as_ref());
        self.selection_reversed = false;
        if !composing {
            self.history
                .record(before, self.snapshot(), EditKind::Atomic);
        }
        self.history.group = None;

        self.reveal_cursor = true;
        self.preferred_x = None;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let bounds = self.last_bounds?;
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.position_for_index(range.start);
        let end = layout.position_for_index(range.end);
        let right = if end.y == start.y {
            end.x.max(start.x + px(1.))
        } else {
            bounds.size.width
        };
        Some(Bounds::new(
            point(
                bounds.left() + start.x - self.scroll_x,
                bounds.top() + start.y - self.scroll_y,
            ),
            size((right - start.x).max(px(1.)), layout.line_height),
        ))
    }
    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.last_layout.as_ref()?;
        Some(self.offset_to_utf16(self.index_for_mouse_position(position)))
    }
}

fn inline_targets(document: &MessageDocument, revealed: &BTreeSet<u32>) -> Vec<Range<usize>> {
    #[derive(PartialEq)]
    enum Key {
        Spoiler(u32),
        Target(MessageTarget),
    }
    if !message_text::valid(document) {
        return Vec::new();
    }
    let mut ranges: Vec<(Key, Range<usize>)> = Vec::new();
    for span in &document.spans {
        if span.range.is_empty() {
            continue;
        }
        let key = if let Some(id) = span.spoilers.iter().find(|id| !revealed.contains(id)) {
            Some(Key::Spoiler(*id))
        } else if !span.style.code {
            span.target
                .clone()
                .filter(|target| {
                    matches!(
                        target,
                        MessageTarget::Link(_)
                            | MessageTarget::Channel(_)
                            | MessageTarget::User(_)
                            | MessageTarget::Role(_)
                            | MessageTarget::Author { .. }
                    )
                })
                .map(Key::Target)
        } else {
            None
        };
        if let Some(key) = key {
            if let Some((previous, range)) = ranges.last_mut()
                && *previous == key
                && range.end == span.range.start
            {
                range.end = span.range.end;
            } else {
                ranges.push((key, span.range.clone()));
            }
        }
    }
    ranges.into_iter().map(|(_, range)| range).collect()
}

// Positive scroll distance needed to reveal an interval. Larger targets align
// their leading edge; clamp prevents scrolling past the available content.
fn reveal_offset(
    offset: Pixels,
    start: Pixels,
    end: Pixels,
    visible_start: Pixels,
    visible_end: Pixels,
    maximum: Pixels,
) -> Pixels {
    let delta = if start < visible_start {
        start - visible_start
    } else if end > visible_end {
        (end - visible_end).min(start - visible_start)
    } else {
        px(0.)
    };
    (offset + delta).clamp(px(0.), maximum.max(px(0.)))
}
fn inline_target_line<'a>(
    layout: &'a DocumentLayout,
    range: &Range<usize>,
) -> Option<(&'a VisualLine, Pixels, Pixels)> {
    let line = layout
        .lines
        .iter()
        .find(|line| line.range.contains(&range.start))?;
    let end = range.end.min(line.range.end);
    let left = line.indent + line.layout.x_for_index(range.start - line.range.start);
    let right = line.indent + line.layout.x_for_index(end - line.range.start);
    Some((line, left, right.max(left + px(1.))))
}

/// Source byte ranges deliberately exclude hard newlines. A soft wrap shares
/// its boundary with the next row; a hard newline leaves a one-byte gap.
#[derive(Clone)]
struct VisualLine {
    range: Range<usize>,
    layout: inline_emoji::EmojiLine,
    indent: Pixels,
    y: Pixels,
    height: Pixels,
    cell_right: Option<Pixels>,
}
#[derive(Clone)]
struct TableCell {
    range: Option<Range<usize>>,
    x: Pixels,
    y: Pixels,
    width: Pixels,
    height: Pixels,
    header: bool,
    row: usize,
}
#[derive(Clone)]
struct TableRegion {
    start: usize,
    y: Pixels,
    height: Pixels,
    width: Pixels,
    viewport: Pixels,
    offset: Pixels,
}
#[derive(Clone)]
struct CodeHeader {
    range: Range<usize>,
    language: Option<String>,
    y: Pixels,
}
#[derive(Clone)]
struct DocumentLayout {
    lines: Vec<VisualLine>,
    ellipsis: Option<VisualLine>,
    code_headers: Vec<CodeHeader>,
    table_cells: Vec<TableCell>,
    table_regions: Vec<TableRegion>,
    content_height: Pixels,
    line_height: Pixels,
}
/// A table row may be taller than an individual cell's wrapped content. Use
/// the cell rectangle before comparing line positions, including its padding.
fn table_line_for_position<'a>(
    cells: &[TableCell],
    lines: impl Iterator<Item = (usize, &'a Range<usize>, Pixels, Pixels)>,
    position: Point<Pixels>,
) -> Option<usize> {
    let cell = cells.iter().find(|cell| {
        position.x >= cell.x
            && position.x < cell.x + cell.width
            && position.y >= cell.y
            && position.y < cell.y + cell.height
    })?;
    let range = cell.range.as_ref()?;
    let mut best: Option<(usize, bool, Pixels)> = None;
    for (index, line, y, height) in lines {
        if line.start < range.start || line.end > range.end {
            continue;
        }
        let inside = position.y >= y && position.y < y + height;
        let distance = if position.y < y {
            y - position.y
        } else if position.y >= y + height {
            position.y - (y + height)
        } else {
            px(0.)
        };
        if best
            .as_ref()
            .is_none_or(|(_, previous_inside, previous_distance)| {
                (inside && !previous_inside)
                    || (inside == *previous_inside && distance < *previous_distance)
            })
        {
            best = Some((index, inside, distance));
        }
    }
    best.map(|(index, _, _)| index)
}
impl DocumentLayout {
    fn height(&self) -> Pixels {
        self.lines
            .iter()
            .map(|line| line.y + line.height)
            .chain(
                self.table_regions
                    .iter()
                    .map(|table| table.y + table.height),
            )
            .fold(self.content_height, Pixels::max)
    }
    fn row_for_y(&self, y: Pixels) -> usize {
        self.lines
            .iter()
            .position(|line| y < line.y + line.height)
            .unwrap_or_else(|| self.lines.len().saturating_sub(1))
    }
    fn row_for_position(&self, position: Point<Pixels>) -> usize {
        if let Some(row) = table_line_for_position(
            &self.table_cells,
            self.lines
                .iter()
                .enumerate()
                .map(|(index, line)| (index, &line.range, line.y, line.height)),
            position,
        ) {
            return row;
        }
        let mut best = None;
        for (index, line) in self.lines.iter().enumerate() {
            if position.y < line.y || position.y >= line.y + line.height {
                continue;
            }
            let right = line.cell_right.unwrap_or(line.indent + line.layout.width);
            let distance = if position.x < line.indent {
                line.indent - position.x
            } else if position.x > right {
                position.x - right
            } else {
                px(0.)
            };
            if best.as_ref().is_none_or(|(_, d)| distance < *d) {
                best = Some((index, distance));
            }
        }
        best.map(|(i, _)| i)
            .unwrap_or_else(|| self.row_for_y(position.y))
    }
    fn row_for_index(&self, index: usize) -> usize {
        row_for_index(
            &self
                .lines
                .iter()
                .map(|r| r.range.clone())
                .collect::<Vec<_>>(),
            index,
        )
    }
    fn position_for_index(&self, index: usize) -> Point<Pixels> {
        let row = self.row_for_index(index);
        let line = &self.lines[row];
        point(
            line.indent
                + line
                    .layout
                    .x_for_index(index.saturating_sub(line.range.start).min(line.range.len())),
            line.y,
        )
    }
    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        if position.y < px(0.) {
            return 0;
        }
        if position.y >= self.height() {
            return self.lines.last().map_or(0, |line| line.range.end);
        }
        let row = self.row_for_position(position);
        let line = &self.lines[row];
        line.range.start
            + line
                .layout
                .closest_index_for_x(position.x - line.indent)
                .min(line.range.len())
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    layout: DocumentLayout,
    scroll: Point<Pixels>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    backgrounds: Vec<PaintQuad>,
    emoji_images: Vec<gpui::AnyElement>,
    code_controls: Vec<gpui::AnyElement>,
}
impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let input = self.input.read(cx);
        if input.read_only {
            if let Some(document) = input.message_document.clone() {
                let text_style = window.text_style();
                let font_size = text_style.font_size.to_pixels(window.rem_size());
                let line_height = window.line_height();
                let runs = message_text::highlight_runs(
                    message_text::runs(
                        &document,
                        &text_style.to_run(document.text.len()),
                        &input.revealed_spoilers,
                    ),
                    &input.highlighted,
                );
                let table_scrolls = input.table_scrolls.clone();
                let inline_preview = input.inline_visible_only;
                let mut style = Style::default();
                style.size.width = relative(1.).into();
                return (
                    window.request_measured_layout(style, move |known, available, window, _| {
                        let width = known.width.or(match available.width {
                            gpui::AvailableSpace::Definite(width) => Some(width),
                            _ => None,
                        });
                        let layout = if inline_preview {
                            message_text::reply_preview_layout(
                                &document,
                                &runs,
                                font_size,
                                line_height,
                                width,
                                window,
                            )
                        } else {
                            message_text::layout(
                                &document,
                                &runs,
                                font_size,
                                line_height,
                                width,
                                &table_scrolls,
                                window,
                            )
                        };
                        let measured_width = layout
                            .lines
                            .iter()
                            .map(|line| line.indent + line.layout.width)
                            .fold(px(0.), Pixels::max);
                        size(width.unwrap_or(measured_width), layout.height())
                    }),
                    (),
                );
            }
            let text = input.content.clone();
            let text_style = window.text_style();
            let font_size = text_style.font_size.to_pixels(window.rem_size());
            let line_height = window.line_height();
            let run = text_style.to_run(text.len());
            let mut style = Style::default();
            style.size.width = relative(1.).into();
            return (
                window.request_measured_layout(style, move |known, available, window, _| {
                    let width = known.width.or(match available.width {
                        gpui::AvailableSpace::Definite(width) => Some(width),
                        _ => None,
                    });
                    let mut height = px(0.);
                    let mut measured_width = px(0.);
                    for paragraph in logical_lines(&text) {
                        let runs = message_text::clip(std::slice::from_ref(&run), &paragraph);
                        let breaks = inline_emoji::wrap_breaks(
                            &text[paragraph.clone()],
                            font_size,
                            &runs,
                            width,
                            window,
                        );
                        for range in visual_ranges(paragraph, &breaks) {
                            let runs = message_text::clip(std::slice::from_ref(&run), &range);
                            let line =
                                inline_emoji::shape_line(&text[range], font_size, &runs, window);
                            measured_width = measured_width.max(line.width);
                            height += line_height;
                        }
                    }
                    size(width.unwrap_or(measured_width), height)
                }),
                (),
            );
        }
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height()
            * if input.multiline {
                input.rows.clamp(1, 6)
            } else {
                1
            })
        .into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let empty = input.content.is_empty();
        let display: SharedString = if empty {
            input.placeholder.clone()
        } else if input.secret {
            "*".repeat(input.content.len()).into()
        } else {
            input.content.clone()
        };
        let base = TextRun {
            len: display.len(),
            font: style.font(),
            color: if empty {
                rgb(0x8c8a9c).into()
            } else {
                style.color
            },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let mut lines = Vec::new();
        let mut ellipsis = None;
        let mut code_headers = Vec::new();
        let mut table_cells = Vec::new();
        let mut table_regions = Vec::new();
        let mut content_height = px(0.);
        let composer_tokens = input.composer_tokens();
        if let Some(document) = input.message_document.as_ref() {
            let runs = message_text::highlight_runs(
                message_text::runs(document, &base, &input.revealed_spoilers),
                &input.highlighted,
            );
            let width = Some(bounds.size.width.max(px(1.)));
            let document_layout = if input.inline_visible_only {
                message_text::reply_preview_layout(
                    document,
                    &runs,
                    font_size,
                    window.line_height(),
                    width,
                    window,
                )
            } else {
                message_text::layout(
                    document,
                    &runs,
                    font_size,
                    window.line_height(),
                    width,
                    &input.table_scrolls,
                    window,
                )
            };
            ellipsis = document_layout.ellipsis;
            code_headers = document_layout.code_headers;
            lines = document_layout.lines;
            table_cells = document_layout.table_cells;
            table_regions = document_layout.table_regions;
            content_height = document_layout.content_height;
        } else if input.multiline && !empty {
            for paragraph in logical_lines(&display) {
                let paragraph_runs =
                    decoration_runs(&base, &paragraph, input.marked_range.as_ref());
                let breaks = inline_emoji::wrap_breaks_with_tokens(
                    &display[paragraph.clone()],
                    font_size,
                    &paragraph_runs,
                    Some(bounds.size.width.max(px(1.))),
                    &composer_tokens::clip(&composer_tokens, &paragraph),
                    window,
                );
                for range in visual_ranges(paragraph, &breaks) {
                    let runs = decoration_runs(&base, &range, input.marked_range.as_ref());
                    let layout = inline_emoji::shape_line_with_tokens(
                        &display[range.clone()],
                        font_size,
                        &runs,
                        &composer_tokens::clip(&composer_tokens, &range),
                        window,
                    );
                    lines.push(VisualLine {
                        range,
                        layout,
                        indent: px(0.),
                        y: window.line_height() * lines.len(),
                        height: window.line_height(),
                        cell_right: None,
                    });
                }
            }
        } else {
            let range = 0..if empty { 0 } else { display.len() };
            let runs = if empty {
                vec![base.clone()]
            } else {
                decoration_runs(&base, &range, input.marked_range.as_ref())
            };
            let layout = inline_emoji::shape_line_with_tokens(
                &display,
                font_size,
                &runs,
                &composer_tokens,
                window,
            );
            lines.push(VisualLine {
                range,
                layout,
                indent: px(0.),
                y: window.line_height() * lines.len(),
                height: window.line_height(),
                cell_right: None,
            });
        }
        let layout = DocumentLayout {
            lines,
            ellipsis,
            code_headers,
            table_cells,
            table_regions,
            content_height,
            line_height: window.line_height(),
        };
        // Apply horizontal table changes before asking the containing list to
        // reveal the focused run vertically.
        let revealing = input.inline_reveal && input.focus_handle.is_focused(window);
        let mut table_adjustment = None;
        if revealing
            && let Some(range) = input.inline_focus.as_ref()
            && let Some((line, left, right)) = inline_target_line(&layout, range)
        {
            if let Some(table) = layout
                .table_regions
                .iter()
                .find(|table| line.y >= table.y && line.y < table.y + table.height)
            {
                let next = reveal_offset(
                    table.offset,
                    left,
                    right,
                    px(0.),
                    table.viewport,
                    (table.width - table.viewport).max(px(0.)),
                );
                if next != table.offset {
                    table_adjustment = Some((table.start, next));
                }
            }
            if table_adjustment.is_none() {
                let target = Bounds::new(
                    point(bounds.left() + left, bounds.top() + line.y - input.scroll_y),
                    size((right - left).max(px(1.)), line.height),
                );
                window.request_autoscroll(target);
            }
        }
        let position = layout.position_for_index(input.cursor_offset());
        let mut scroll = point(input.scroll_x, input.scroll_y);
        if input.multiline {
            scroll.x = px(0.);
        } else {
            scroll.y = px(0.);
        }
        if input.reveal_cursor && !input.read_only {
            if position.x < scroll.x {
                scroll.x = position.x;
            }
            if position.x > scroll.x + bounds.size.width - px(3.) {
                scroll.x = (position.x - bounds.size.width + px(3.)).max(px(0.));
            }
            if position.y < scroll.y {
                scroll.y = position.y;
            }
            if position.y + layout.line_height > scroll.y + bounds.size.height {
                scroll.y = position.y + layout.line_height - bounds.size.height;
            }
        }
        scroll.y = scroll
            .y
            .clamp(px(0.), (layout.height() - bounds.size.height).max(px(0.)));
        let mut selections = Vec::new();
        let timeline_range = input
            .timeline_selection
            .as_ref()
            .and_then(|(message, selection)| {
                selection
                    .borrow()
                    .transcript()
                    .map(|range| (*message, range))
            });
        let selection = if let Some((message, range)) = timeline_range {
            range.for_message(message, input.content.len())
        } else if input.focus_handle.is_focused(window) {
            if input.selected_range.is_empty() {
                input.inline_focus.clone()
            } else {
                Some(input.selected_range.clone())
            }
        } else {
            None
        };
        if let Some(range) = selection {
            for line in &layout.lines {
                if range.start <= line.range.end && range.end > line.range.start {
                    let from = range
                        .start
                        .saturating_sub(line.range.start)
                        .min(line.range.len());
                    let to = range
                        .end
                        .saturating_sub(line.range.start)
                        .min(line.range.len());
                    let x1 = line.indent + line.layout.x_for_index(from);
                    let x2 = if range.end > line.range.end {
                        line.cell_right.unwrap_or(bounds.size.width)
                    } else {
                        line.indent + line.layout.x_for_index(to)
                    };
                    if x2 > x1 {
                        selections.push(fill(
                            Bounds::new(
                                point(
                                    bounds.left() + x1 - scroll.x,
                                    bounds.top() + line.y - scroll.y,
                                ),
                                size(x2 - x1, line.height),
                            ),
                            rgba(0x8873ee60),
                        ));
                    }
                }
            }
        }
        let cursor = (!input.read_only && input.selected_range.is_empty()).then(|| {
            fill(
                Bounds::new(
                    point(
                        bounds.left() + position.x - scroll.x,
                        bounds.top() + position.y - scroll.y,
                    ),
                    size(px(2.), layout.line_height),
                ),
                rgb(0xb29bff),
            )
        });
        let document = input.message_document.clone();
        let revealed = input.revealed_spoilers.clone();
        let copied_code = input.copy_feedback.range.clone();
        let backgrounds = document
            .as_ref()
            .map(|document| message_text::backgrounds(document, &layout, bounds, scroll))
            .unwrap_or_default();
        let content = input.content.clone();
        let secret = input.secret;
        let emoji_cache = input.emoji_cache.clone();
        let emoji_reduced_motion = input.emoji_reduced_motion;
        let emoji_hovered = input.emoji_hovered;
        let mut emoji_images = document
            .as_ref()
            .map(|document| {
                message_text::emoji_images(
                    message_text::MessagePaintContext {
                        document,
                        revealed: &revealed,
                        layout: &layout,
                        bounds,
                        scroll,
                    },
                    message_text::EmojiImageOptions {
                        cache: &emoji_cache,
                        reduced_motion: emoji_reduced_motion,
                        hovered: emoji_hovered,
                    },
                    window,
                    cx,
                )
            })
            .unwrap_or_default();
        if !empty && !secret && document.as_ref().is_none_or(|doc| message_text::valid(doc)) {
            emoji_images.extend(inline_emoji::images(
                &content,
                document.as_deref(),
                &revealed,
                &layout,
                (bounds, scroll),
                window,
                cx,
            ));
        }
        let code_controls = document
            .as_ref()
            .map(|doc| {
                message_text::code_controls(
                    message_text::MessagePaintContext {
                        document: doc,
                        revealed: &revealed,
                        layout: &layout,
                        bounds,
                        scroll,
                    },
                    self.input.clone(),
                    copied_code.as_ref(),
                    window,
                    cx,
                )
            })
            .unwrap_or_default();
        if let Some(document) = document.as_ref() {
            emoji_images.extend(message_text::timestamp_widgets(
                document, &revealed, &layout, bounds, scroll, window, cx,
            ));
        }
        if revealing {
            if table_adjustment.is_some() {
                window.on_next_frame(|window, _| window.refresh());
            }
            self.input.update(cx, |input, cx| {
                if let Some((table, offset)) = table_adjustment {
                    input.table_scrolls.insert(table, offset);
                }
                input.inline_reveal = table_adjustment.is_some();
                cx.notify();
            });
        }
        PrepaintState {
            code_controls,
            backgrounds,
            emoji_images,
            layout,
            scroll,
            cursor,
            selections,
        }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        if !self.input.read(cx).read_only {
            window.handle_input(
                &focus,
                ElementInputHandler::new(bounds, self.input.clone()),
                cx,
            );
        }
        // Continue an active selection when the pointer leaves the text bounds.
        let input = self.input.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == gpui::DispatchPhase::Capture
                && !bounds.contains(&event.position)
                && (input.read(cx).is_selecting || input.read(cx).table_drag.is_some())
            {
                input.update(cx, |input, cx| input.on_mouse_move(event, window, cx));
            }
        });
        let input = self.input.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == gpui::DispatchPhase::Capture
                && event.button == MouseButton::Left
                && !bounds.contains(&event.position)
                && (input.read(cx).is_selecting || input.read(cx).table_drag.is_some())
            {
                input.update(cx, |input, cx| input.on_mouse_up(event, window, cx));
            }
        });
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for background in prepaint.backgrounds.drain(..) {
                window.paint_quad(background);
            }
            let input = self.input.read(cx);
            if input.read_only && input.message_document.is_some() {
                for line in &prepaint.layout.lines {
                    let origin = point(
                        bounds.left() + line.indent - prepaint.scroll.x,
                        bounds.top() + line.y - prepaint.scroll.y,
                    );
                    if origin.y + line.height >= bounds.top() && origin.y <= bounds.bottom() {
                        line.layout
                            .paint_background(origin, line.height, window, cx)
                            .expect("paint rich message run backgrounds");
                    }
                }
            }
            for selection in prepaint.selections.drain(..) {
                window.paint_quad(selection);
            }
            for line in &prepaint.layout.lines {
                let origin = point(
                    bounds.left() + line.indent - prepaint.scroll.x,
                    bounds.top() + line.y - prepaint.scroll.y,
                );
                if origin.y + line.height >= bounds.top() && origin.y <= bounds.bottom() {
                    line.layout
                        .paint(origin, line.height, window, cx)
                        .expect("paint native composer");
                }
            }
            if let Some(line) = &prepaint.layout.ellipsis {
                let origin = point(
                    bounds.left() + line.indent - prepaint.scroll.x,
                    bounds.top() + line.y - prepaint.scroll.y,
                );
                line.layout
                    .paint(origin, line.height, window, cx)
                    .expect("paint reply preview ellipsis");
            }
            for control in &mut prepaint.code_controls {
                control.paint(window, cx);
            }
            for image in &mut prepaint.emoji_images {
                image.paint(window, cx);
            }
        });
        if focus.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        let parent_mask = window.content_mask().bounds;
        let visible_bounds = bounds
            .intersects(&parent_mask)
            .then(|| bounds.intersect(&parent_mask));
        self.input.update(cx, |input, cx| {
            input.last_visible_bounds = visible_bounds;
            let rows = prepaint.layout.lines.len();
            if !input.read_only && input.rows != rows {
                input.rows = rows;
                cx.notify();
            }
            input.last_layout = Some(prepaint.layout.clone());
            input.last_bounds = Some(bounds);
            input.scroll_x = prepaint.scroll.x;
            input.scroll_y = prepaint.scroll.y;
            input.reveal_cursor = false;
        });
    }
}

fn decoration_runs(
    base: &TextRun,
    line: &Range<usize>,
    marked: Option<&Range<usize>>,
) -> Vec<TextRun> {
    let Some(marked) = marked else {
        return vec![TextRun {
            len: line.len(),
            ..base.clone()
        }];
    };
    let start = marked.start.max(line.start).min(line.end);
    let end = marked.end.min(line.end).max(start);
    [
        (start - line.start, false),
        (end - start, true),
        (line.end - end, false),
    ]
    .into_iter()
    .filter(|(len, _)| *len > 0)
    .map(|(len, marked)| TextRun {
        len,
        underline: marked.then_some(UnderlineStyle {
            color: Some(base.color),
            thickness: px(1.),
            wavy: false,
        }),
        ..base.clone()
    })
    .collect()
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let popup = self.completion_trigger().and_then(|_| {
            if self.completion_choices.is_empty() || !self.focus_handle.is_focused(window) {
                return None;
            }
            let bounds = self.last_bounds?;
            let caret = self
                .last_layout
                .as_ref()?
                .position_for_index(self.cursor_offset());
            let origin = point(
                (bounds.left() + caret.x - self.scroll_x).clamp(bounds.left(), bounds.right()),
                (bounds.top() + caret.y - self.scroll_y).clamp(bounds.top(), bounds.bottom()),
            );
            let first = self.completion_index.saturating_sub(5);
            Some(
                gpui::deferred(
                    gpui::anchored()
                        .position_mode(gpui::AnchoredPositionMode::Window)
                        .position(origin)
                        .anchor(gpui::Corner::BottomLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .id("composer-completion")
                                .occlude()
                                .w(px(320.))
                                .p_1()
                                .rounded_md()
                                .bg(rgb(0x24212d))
                                .border_1()
                                .border_color(rgb(0x494253))
                                .shadow_lg()
                                .on_scroll_wheel(cx.listener(
                                    |this, event: &gpui::ScrollWheelEvent, window, cx| {
                                        let delta = event.delta.pixel_delta(px(38.)).y;
                                        if delta != px(0.) {
                                            this.completion_key(
                                                if delta < px(0.) { "down" } else { "up" },
                                                false,
                                                window,
                                                cx,
                                            );
                                        }
                                        cx.stop_propagation();
                                    },
                                ))
                                .children(
                                    self.completion_choices
                                        .iter()
                                        .enumerate()
                                        .skip(first)
                                        .take(6)
                                        .map(|(index, choice)| {
                                            div()
                                                .id(("completion-row", index))
                                                .h(px(38.))
                                                .px_2()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .rounded_sm()
                                                .when(index == self.completion_index, |v| {
                                                    v.bg(rgb(0x443853))
                                                })
                                                .child(
                                                    div()
                                                        .relative()
                                                        .w(px(26.))
                                                        .h(px(26.))
                                                        .child(if let Some(user) = &choice.user {
                                                            crate::avatar_images::user_avatar(
                                                                user,
                                                                choice.media.as_deref(),
                                                                26.,
                                                            )
                                                        } else if let Some(url) = &choice.image {
                                                            gpui::img(SharedString::from(
                                                                url.clone(),
                                                            ))
                                                            .size(px(26.))
                                                            .object_fit(gpui::ObjectFit::Contain)
                                                            .into_any_element()
                                                        } else {
                                                            crate::emoji_sprites::unicode_art(
                                                                &choice.glyph,
                                                                24.,
                                                                window.scale_factor() > 1.,
                                                                cx,
                                                            )
                                                        })
                                                        .when_some(choice.status, |view, color| {
                                                            view.child(
                                                                div()
                                                                    .absolute()
                                                                    .right_0()
                                                                    .bottom_0()
                                                                    .size(px(8.))
                                                                    .rounded_full()
                                                                    .border_1()
                                                                    .border_color(rgb(0x24212d))
                                                                    .bg(rgb(color)),
                                                            )
                                                        }),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .child(
                                                            div()
                                                                .text_size(px(13.))
                                                                .text_ellipsis()
                                                                .child(choice.label.clone()),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(10.))
                                                                .text_color(rgb(0xa59caf))
                                                                .text_ellipsis()
                                                                .child(choice.detail.clone()),
                                                        ),
                                                )
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, _, window, cx| {
                                                        this.completion_index = index;
                                                        this.completion_key(
                                                            "tab", false, window, cx,
                                                        );
                                                        cx.stop_propagation();
                                                    }),
                                                )
                                        }),
                                ),
                        ),
                )
                .with_priority(2)
                .into_any_element(),
            )
        });
        let editor = div()
            .id("text-input")
            .flex()
            .w_full()
            .min_w_0()
            .when(self.read_only, |view| view.w_full().min_w_0())
            .key_context(if self.message_document.is_some() {
                "SelectableText"
            } else if self.read_only {
                "SelectableText InputContextMenu"
            } else if self.chat_picker {
                "TextInput ChatPicker InputContextMenu"
            } else if self.multiline {
                "TextInput MultilineInput InputContextMenu"
            } else {
                "TextInput InputContextMenu"
            })
            .track_focus(&self.focus_handle(cx))
            .cursor(if self.hover_interactive {
                CursorStyle::PointingHand
            } else {
                CursorStyle::IBeam
            })
            .on_key_down(cx.listener(Self::inline_tab))
            .on_action(cx.listener(Self::activate_inline))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::document_home))
            .on_action(cx.listener(Self::document_end))
            .on_action(cx.listener(Self::select_document_home))
            .on_action(cx.listener(Self::select_document_end))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::open_text_context_menu))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_context_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_hover(cx.listener(|this, hovered, _, cx| {
                if this.emoji_hovered != *hovered {
                    this.emoji_hovered = *hovered;
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .when(!self.read_only, |view| view.bg(rgb(0x292735)))
            .line_height(if self.read_only {
                window.line_height()
            } else {
                px(24.)
            })
            .when(!self.read_only, |view| view.text_size(px(14.)))
            .text_color(rgb(0xe9e7f2))
            .child(
                div()
                    .w_full()
                    .when(!self.read_only, |view| {
                        view.h(px(16.
                            + 24.
                                * if self.multiline {
                                    self.rows.clamp(1, 6) as f32
                                } else {
                                    1.
                                }))
                        .p(px(8.))
                        .rounded_md()
                        .overflow_hidden()
                    })
                    .child(TextElement { input: cx.entity() }),
            );
        // Keep the menu beside the editor's focus/action subtree. Menu pointer
        // events and Ctrl shortcuts must not edit or reselect the backing input.
        div()
            .flex()
            .w_full()
            .min_w_0()
            .child(editor)
            .when_some(popup, |view, popup| view.child(popup))
            .when_some(self.context_menu.clone(), |view, menu| view.child(menu))
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("shift-f10", OpenTextContextMenu, Some("InputContextMenu")),
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("up", Up, Some("MultilineInput")),
        KeyBinding::new("down", Down, Some("MultilineInput")),
        KeyBinding::new("shift-up", SelectUp, Some("MultilineInput")),
        KeyBinding::new("shift-down", SelectDown, Some("MultilineInput")),
        KeyBinding::new("shift-home", SelectHome, Some("TextInput")),
        KeyBinding::new("shift-end", SelectEnd, Some("TextInput")),
        KeyBinding::new("ctrl-home", DocumentHome, Some("TextInput")),
        KeyBinding::new("ctrl-end", DocumentEnd, Some("TextInput")),
        KeyBinding::new("ctrl-shift-home", SelectDocumentHome, Some("TextInput")),
        KeyBinding::new("ctrl-shift-end", SelectDocumentEnd, Some("TextInput")),
        KeyBinding::new("shift-enter", Newline, Some("MultilineInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("ctrl-left", WordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-right", WordRight, Some("TextInput")),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, Some("TextInput")),
        KeyBinding::new("alt-left", WordLeft, Some("TextInput")),
        KeyBinding::new("alt-right", WordRight, Some("TextInput")),
        KeyBinding::new("alt-shift-left", SelectWordLeft, Some("TextInput")),
        KeyBinding::new("alt-shift-right", SelectWordRight, Some("TextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("TextInput")),
        KeyBinding::new("ctrl-v", Paste, Some("TextInput")),
        KeyBinding::new("ctrl-c", Copy, Some("TextInput")),
        KeyBinding::new("ctrl-x", Cut, Some("TextInput")),
        KeyBinding::new("ctrl-z", Undo, Some("TextInput")),
        KeyBinding::new("ctrl-shift-z", Redo, Some("TextInput")),
        KeyBinding::new("ctrl-y", Redo, Some("TextInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("TextInput")),
        KeyBinding::new("cmd-v", Paste, Some("TextInput")),
        KeyBinding::new("cmd-c", Copy, Some("TextInput")),
        KeyBinding::new("cmd-x", Cut, Some("TextInput")),
        KeyBinding::new("cmd-z", Undo, Some("TextInput")),
        KeyBinding::new("cmd-shift-z", Redo, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("left", Left, Some("SelectableText")),
        KeyBinding::new("right", Right, Some("SelectableText")),
        KeyBinding::new("up", Up, Some("SelectableText")),
        KeyBinding::new("down", Down, Some("SelectableText")),
        KeyBinding::new("shift-left", SelectLeft, Some("SelectableText")),
        KeyBinding::new("shift-right", SelectRight, Some("SelectableText")),
        KeyBinding::new("shift-up", SelectUp, Some("SelectableText")),
        KeyBinding::new("shift-down", SelectDown, Some("SelectableText")),
        KeyBinding::new("home", Home, Some("SelectableText")),
        KeyBinding::new("end", End, Some("SelectableText")),
        KeyBinding::new("shift-home", SelectHome, Some("SelectableText")),
        KeyBinding::new("shift-end", SelectEnd, Some("SelectableText")),
        KeyBinding::new("ctrl-home", DocumentHome, Some("SelectableText")),
        KeyBinding::new("ctrl-end", DocumentEnd, Some("SelectableText")),
        KeyBinding::new(
            "ctrl-shift-home",
            SelectDocumentHome,
            Some("SelectableText"),
        ),
        KeyBinding::new("ctrl-shift-end", SelectDocumentEnd, Some("SelectableText")),
        KeyBinding::new("cmd-up", DocumentHome, Some("SelectableText")),
        KeyBinding::new("cmd-down", DocumentEnd, Some("SelectableText")),
        KeyBinding::new("cmd-shift-up", SelectDocumentHome, Some("SelectableText")),
        KeyBinding::new("cmd-shift-down", SelectDocumentEnd, Some("SelectableText")),
        KeyBinding::new("cmd-left", Home, Some("SelectableText")),
        KeyBinding::new("cmd-right", End, Some("SelectableText")),
        KeyBinding::new("cmd-shift-left", SelectHome, Some("SelectableText")),
        KeyBinding::new("cmd-shift-right", SelectEnd, Some("SelectableText")),
        KeyBinding::new("ctrl-left", WordLeft, Some("SelectableText")),
        KeyBinding::new("ctrl-right", WordRight, Some("SelectableText")),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, Some("SelectableText")),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, Some("SelectableText")),
        KeyBinding::new("alt-left", WordLeft, Some("SelectableText")),
        KeyBinding::new("alt-right", WordRight, Some("SelectableText")),
        KeyBinding::new("alt-shift-left", SelectWordLeft, Some("SelectableText")),
        KeyBinding::new("alt-shift-right", SelectWordRight, Some("SelectableText")),
        KeyBinding::new("ctrl-a", SelectAll, Some("SelectableText")),
        KeyBinding::new("cmd-a", SelectAll, Some("SelectableText")),
        KeyBinding::new("ctrl-c", Copy, Some("SelectableText")),
        KeyBinding::new("cmd-c", Copy, Some("SelectableText")),
        KeyBinding::new("ctrl-c", Copy, Some("ChatTimeline")),
        KeyBinding::new("cmd-c", Copy, Some("ChatTimeline")),
        KeyBinding::new("enter", ActivateInline, Some("SelectableText")),
        KeyBinding::new("space", ActivateInline, Some("SelectableText")),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-up", DocumentHome, Some("TextInput")),
        KeyBinding::new("cmd-down", DocumentEnd, Some("TextInput")),
        KeyBinding::new("cmd-shift-up", SelectDocumentHome, Some("TextInput")),
        KeyBinding::new("cmd-shift-down", SelectDocumentEnd, Some("TextInput")),
        KeyBinding::new("cmd-left", Home, Some("TextInput")),
        KeyBinding::new("cmd-right", End, Some("TextInput")),
        KeyBinding::new("cmd-shift-left", SelectHome, Some("TextInput")),
        KeyBinding::new("cmd-shift-right", SelectEnd, Some("TextInput")),
    ]);
}

#[cfg(target_os = "macos")]
pub fn edit_menu() -> gpui::Menu {
    use gpui::{Menu, MenuItem, OsAction};
    Menu {
        name: "Edit".into(),
        items: vec![
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ],
    }
}

fn word_offset(text: &str, offset: usize, forward: bool) -> usize {
    let offset = grapheme_offset(text, offset);
    let words = text
        .split_word_bound_indices()
        .filter(|(_, part)| !part.trim().is_empty());
    if forward {
        words
            .map(|(start, part)| start + part.len())
            .find(|end| *end > offset)
            .unwrap_or(text.len())
    } else {
        words
            .map(|(start, _)| start)
            .take_while(|start| *start < offset)
            .last()
            .unwrap_or(0)
    }
}

/// Word selection follows Unicode word boundaries; punctuation and emoji remain
/// selectable units. Triple click selects a logical line including its newline.
fn selection_unit(text: &str, index: usize, clicks: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let index = grapheme_offset(text, index.min(text.len()));
    if clicks >= 3 {
        let start = text[..index].rfind('\n').map_or(0, |offset| offset + 1);
        let end = text[index..]
            .find('\n')
            .map_or(text.len(), |offset| index + offset + 1);
        return start..end;
    }
    let index = if index == text.len() {
        text.grapheme_indices(true)
            .next_back()
            .map_or(0, |(offset, _)| offset)
    } else {
        index
    };
    text.split_word_bound_indices()
        .find_map(|(start, word)| {
            (index >= start && index < start + word.len()).then_some(start..start + word.len())
        })
        .unwrap_or(index..text.len())
}

// IME selections are relative to the newly marked text, in UTF-16 units.
fn utf16_offset(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (index, ch) in text.char_indices() {
        if units >= offset {
            return index;
        }
        units += ch.len_utf16();
    }
    text.len()
}

fn grapheme_offset(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    text.grapheme_indices(true)
        .map(|(offset, _)| offset)
        .take_while(|offset| *offset <= index)
        .last()
        .unwrap_or(0)
}

fn normalize_insert(text: &str, multiline: bool) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if multiline {
        text
    } else {
        text.replace('\n', " ")
    }
}
fn logical_lines(text: &str) -> Vec<Range<usize>> {
    let mut offset = 0;
    text.split('\n')
        .map(|line| {
            let range = offset..offset + line.len();
            offset = range.end + 1;
            range
        })
        .collect()
}
fn visual_ranges(paragraph: Range<usize>, breaks: &[usize]) -> Vec<Range<usize>> {
    let mut boundaries = vec![paragraph.start];
    boundaries.extend(
        breaks
            .iter()
            .filter(|index| **index > 0 && **index < paragraph.len())
            .map(|index| paragraph.start + index),
    );
    boundaries.push(paragraph.end);
    boundaries.sort_unstable();
    // Preserve empty paragraphs, including the trailing row after a newline.
    if paragraph.is_empty() {
        return vec![paragraph];
    }
    boundaries.dedup();
    boundaries.windows(2).map(|pair| pair[0]..pair[1]).collect()
}
fn row_for_index(rows: &[Range<usize>], index: usize) -> usize {
    rows.iter()
        .position(|row| index <= row.end)
        .unwrap_or(rows.len().saturating_sub(1))
}

fn marked_selection(start: usize, text: &str, selection: Option<&Range<usize>>) -> Range<usize> {
    selection
        .map(|selected| {
            start + utf16_offset(text, selected.start)..start + utf16_offset(text, selected.end)
        })
        .unwrap_or(start + text.len()..start + text.len())
}

#[cfg(test)]
mod tests {
    use super::{
        EditHistory, EditKind, EditSnapshot, InputMenuAction, InputMenuState,
        context_click_selection, input_menu_items,
    };
    use super::{
        PastePayload, grapheme_offset, logical_lines, marked_selection, normalize_insert,
        paste_payload, row_for_index, selection_unit, utf16_offset, visual_ranges, word_offset,
    };
    use crate::context_menu::ContextMenuItem;
    use gpui::ClipboardItem;
    use std::ops::Range;
    fn edit_snapshot(text: &str, selection: Range<usize>, reversed: bool) -> EditSnapshot {
        EditSnapshot {
            text: text.to_owned().into(),
            selection,
            reversed,
        }
    }
    fn menu_rows(state: &InputMenuState) -> Vec<(String, bool)> {
        input_menu_items(state)
            .into_iter()
            .filter_map(|item| match item {
                ContextMenuItem::Action { id, enabled, .. } => Some((id, enabled)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn copy_feedback_refreshes_timer_and_ignores_stale_document_callbacks() {
        let mut feedback = super::CopyFeedback::default();
        let first = feedback.begin(5..11);
        let second = feedback.begin(5..11);
        assert!(!feedback.clear_if_current(first));
        assert_eq!(feedback.range, Some(5..11));
        assert!(feedback.clear_if_current(second));
        let previous = feedback.begin(0..4);
        feedback.reset();
        feedback.begin(8..12);
        assert!(!feedback.clear_if_current(previous));
        assert_eq!(feedback.range, Some(8..12));
    }
    #[test]
    fn scrollbar_drag_stops_as_soon_as_left_button_is_released() {
        let mut drag = Some(7);
        assert!(super::table_drag_is_active(
            &mut drag,
            Some(gpui::MouseButton::Left)
        ));
        assert!(!super::table_drag_is_active(&mut drag, None));
        assert_eq!(drag, None);
        assert!(!super::table_drag_is_active(
            &mut drag,
            Some(gpui::MouseButton::Left)
        ));
    }
    #[test]
    fn table_hit_testing_keeps_blank_padding_in_the_clicked_scrolled_cell() {
        use super::{TableCell, table_line_for_position};
        use gpui::{point, px};
        let cells = vec![
            TableCell {
                range: Some(0..12),
                x: px(-40.),
                y: px(10.),
                width: px(100.),
                height: px(72.),
                header: false,
                row: 1,
            },
            TableCell {
                range: Some(13..17),
                x: px(60.),
                y: px(10.),
                width: px(100.),
                height: px(72.),
                header: false,
                row: 1,
            },
            TableCell {
                range: Some(18..18),
                x: px(160.),
                y: px(10.),
                width: px(100.),
                height: px(72.),
                header: false,
                row: 1,
            },
        ];
        let lines = [
            (0..4, px(16.), px(20.)),
            (4..8, px(36.), px(20.)),
            (8..12, px(56.), px(20.)),
            (13..17, px(16.), px(20.)),
            (18..18, px(16.), px(20.)),
        ];
        let hit = |x, y| {
            table_line_for_position(
                &cells,
                lines
                    .iter()
                    .enumerate()
                    .map(|(i, (range, y, height))| (i, range, *y, *height)),
                point(px(x), px(y)),
            )
        };
        assert_eq!(hit(90., 65.), Some(3)); // blank lower right -> right cell
        assert_eq!(hit(90., 11.), Some(3)); // top padding -> right cell
        assert_eq!(hit(30., 41.), Some(1)); // scrolled left cell, second line
        assert_eq!(hit(30., 56.), Some(2)); // exact wrap boundary -> downstream
        assert_eq!(hit(59., 65.), Some(2));
        assert_eq!(hit(60., 65.), Some(3)); // column boundary belongs right
        assert_eq!(hit(180., 65.), Some(4)); // empty cell retains its own caret
    }
    #[test]
    fn text_context_menu_disables_selection_actions_and_tracks_undo_redo() {
        let mut state = InputMenuState {
            selectable: true,
            paste: true,
            ..Default::default()
        };
        assert_eq!(
            menu_rows(&state),
            [
                ("undo".into(), false),
                ("redo".into(), false),
                ("cut".into(), false),
                ("copy".into(), false),
                ("paste".into(), true),
                ("delete".into(), false),
                ("select-all".into(), true),
            ]
        );
        let mut history = EditHistory::default();
        let selected = edit_snapshot("a👍🏽b", 1..9, true);
        let deleted = edit_snapshot("ab", 1..1, false);
        let kind = super::edit_kind(&selected, &selected.selection, "");
        assert_eq!(kind, EditKind::Atomic);
        history.record(selected.clone(), deleted.clone(), kind);
        state.undo = !history.undo.is_empty();
        state.redo = !history.redo.is_empty();
        assert!(InputMenuAction::Undo.enabled(&state));
        assert!(!InputMenuAction::Redo.enabled(&state));
        assert_eq!(history.undo(deleted), Some(selected));
        state.undo = !history.undo.is_empty();
        state.redo = !history.redo.is_empty();
        assert!(!InputMenuAction::Undo.enabled(&state));
        assert!(InputMenuAction::Redo.enabled(&state));
    }
    #[test]
    fn readonly_menu_exposes_only_copy_and_select_all_and_passwords_never_offer_export() {
        let state = InputMenuState {
            read_only: true,
            selected: true,
            selectable: true,
            undo: true,
            redo: true,
            paste: true,
            secret: false,
        };
        assert_eq!(
            menu_rows(&state),
            [("copy".into(), true), ("select-all".into(), true)]
        );
        let password = InputMenuState {
            secret: true,
            read_only: false,
            ..state
        };
        let rows = menu_rows(&password);
        assert_eq!(
            rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["undo", "redo", "paste", "delete", "select-all"]
        );
        assert!(!InputMenuAction::Copy.enabled(&password));
        assert!(!InputMenuAction::Cut.enabled(&password));
    }
    #[test]
    fn secondary_click_preserves_selected_unicode_including_trailing_caret_boundary() {
        let text = "a👩‍💻b";
        let selection = 1..text.len() - 1;
        for index in [selection.start, selection.end] {
            assert_eq!(context_click_selection(&selection, index), None);
        }
        assert_eq!(context_click_selection(&selection, 0), Some(0..0));
        assert_eq!(
            context_click_selection(&selection, text.len()),
            Some(text.len()..text.len())
        );
        assert_eq!(context_click_selection(&(0..0), 1), Some(1..1));
    }
    #[test]
    fn undo_groups_adjacent_unicode_typing_and_redo_restores_final_caret() {
        let mut history = EditHistory::default();
        let now = std::time::Instant::now();
        let empty = edit_snapshot("", 0..0, false);
        let first = edit_snapshot("a", 1..1, false);
        let second = edit_snapshot("a👍🏽", 9..9, false);
        history.record_at(empty.clone(), first.clone(), EditKind::Insert, now);
        history.record_at(
            first,
            second.clone(),
            EditKind::Insert,
            now + std::time::Duration::from_millis(200),
        );
        assert_eq!(history.undo(second.clone()), Some(empty.clone()));
        assert_eq!(history.redo(empty), Some(second));
    }
    #[test]
    fn selection_replacement_and_paste_are_atomic_and_restore_reversed_selection() {
        let mut history = EditHistory::default();
        let original = edit_snapshot("hello 👍🏽", 6..14, true);
        let replaced = edit_snapshot("hello 🌈", 10..10, false);
        history.record(original.clone(), replaced.clone(), EditKind::Atomic);
        let pasted = edit_snapshot("hello 🌈 pasted", 17..17, false);
        history.record(replaced.clone(), pasted.clone(), EditKind::Atomic);
        assert_eq!(history.undo(pasted), Some(replaced.clone()));
        assert_eq!(history.undo(replaced), Some(original));
    }
    #[test]
    fn deletion_direction_and_time_break_groups_and_new_edits_discard_redo() {
        let mut history = EditHistory::default();
        let now = std::time::Instant::now();
        let a = edit_snapshot("abcd", 2..2, false);
        let b = edit_snapshot("acd", 1..1, false);
        let c = edit_snapshot("ad", 1..1, false);
        let d = edit_snapshot("a", 1..1, false);
        history.record_at(a.clone(), b.clone(), EditKind::Backspace, now);
        history.record_at(b.clone(), c.clone(), EditKind::Delete, now);
        history.record_at(
            c.clone(),
            d.clone(),
            EditKind::Delete,
            now + std::time::Duration::from_secs(1),
        );
        assert_eq!(history.undo(d), Some(c.clone()));
        assert_eq!(history.undo(c), Some(b.clone()));
        assert_eq!(history.undo(b.clone()), Some(a.clone()));
        history.record(a, b.clone(), EditKind::Atomic);
        assert_eq!(history.redo(b), None);
    }
    #[test]
    fn clipboard_text_paste_preserves_unicode_and_newlines_with_attachment_support() {
        let text = "one\n雪 🦀";
        for allow_image in [false, true] {
            match paste_payload(ClipboardItem::new_string(text.into()), allow_image) {
                PastePayload::Text(actual) => assert_eq!(actual, text),
                _ => panic!("text clipboard must stay text"),
            }
        }
    }
    #[test]
    fn clipboard_image_paste_transfers_encoded_bytes_without_decoding() {
        let encoded = vec![0, 1, 2, 3]; // Deliberately not a decodable image.
        let item = ClipboardItem::new_image(&gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            encoded.clone(),
        ));
        match paste_payload(item.clone(), true) {
            PastePayload::Image(image) => {
                assert_eq!(image.content_type, "image/png");
                assert_eq!(image.extension, "png");
                assert_eq!(*image.bytes, encoded);
            }
            _ => panic!("composer image clipboard must emit encoded bytes"),
        }
        assert!(matches!(paste_payload(item, false), PastePayload::Empty));
    }
    #[test]
    fn word_navigation_skips_whitespace_and_respects_unicode_and_emoji() {
        let text = "one  café\n👩‍💻!";
        let cafe_end = text.find('\n').unwrap();
        let emoji_start = cafe_end + 1;
        let emoji_end = text.len() - 1;
        assert_eq!(word_offset(text, 0, true), 3);
        assert_eq!(word_offset(text, 3, true), cafe_end);
        assert_eq!(word_offset(text, cafe_end, true), emoji_end);
        assert_eq!(word_offset(text, emoji_start + 5, true), emoji_end);
        assert_eq!(word_offset(text, text.len(), false), emoji_end);
        assert_eq!(word_offset(text, emoji_end, false), emoji_start);
        assert_eq!(word_offset(text, emoji_start, false), 5);
        assert_eq!(word_offset(text, 5, false), 0);
        assert_eq!(word_offset("", 0, true), 0);
        assert_eq!(word_offset("   ", 2, false), 0);
    }

    #[test]
    fn double_click_selects_unicode_words_and_whitespace_without_splitting_emoji() {
        let text = "Hello  café 👩‍💻!";
        assert_eq!(&text[selection_unit(text, 2, 2)], "Hello");
        assert_eq!(&text[selection_unit(text, 5, 2)], "  ");
        assert_eq!(&text[selection_unit(text, 9, 2)], "café");
        let emoji = text.find('👩').unwrap();
        assert_eq!(&text[selection_unit(text, emoji + 5, 2)], "👩‍💻");
        assert_eq!(&text[selection_unit(text, text.len(), 2)], "!");
        assert_eq!(selection_unit("", 0, 2), 0..0);
    }

    #[test]
    fn triple_click_preserves_newline_for_copy_and_handles_empty_paragraphs() {
        let text = "first\n😀 second\n\nlast";
        assert_eq!(&text[selection_unit(text, 2, 3)], "first\n");
        assert_eq!(&text[selection_unit(text, 8, 3)], "😀 second\n");
        let empty = text.find("\n\n").unwrap() + 1;
        assert_eq!(&text[selection_unit(text, empty, 3)], "\n");
        assert_eq!(&text[selection_unit(text, text.len(), 3)], "last");
        assert_eq!(selection_unit("first\n", 6, 3), 6..6);
    }

    #[test]
    fn hard_newlines_preserve_unicode_byte_offsets_and_trailing_empty_row() {
        let text = "a😀\né\n";
        let rows = logical_lines(text);
        assert_eq!(rows, vec![0..5, 6..8, 9..9]);
        assert_eq!(
            rows.iter().map(|r| &text[r.clone()]).collect::<Vec<_>>(),
            vec!["a😀", "é", ""]
        );
        assert_eq!(logical_lines("\n\n"), vec![0..0, 1..1, 2..2]);
    }
    #[test]
    fn soft_wraps_preserve_source_offsets_without_creating_newlines() {
        let text = "prefix\n你好 world";
        let paragraph = logical_lines(text)[1].clone();
        let rows = visual_ranges(paragraph, &[6, 7]);
        assert_eq!(rows, vec![7..13, 13..14, 14..19]);
        assert_eq!(
            rows.iter().map(|r| &text[r.clone()]).collect::<String>(),
            "你好 world"
        );
        assert_eq!(visual_ranges(20..20, &[]), vec![20..20]);
    }
    #[test]
    fn cursor_row_mapping_distinguishes_hard_and_soft_boundaries() {
        let rows = vec![0..4, 4..8, 9..9];
        assert_eq!(row_for_index(&rows, 4), 0); // soft-wrap end affinity
        assert_eq!(row_for_index(&rows, 5), 1);
        assert_eq!(row_for_index(&rows, 8), 1); // before hard newline
        assert_eq!(row_for_index(&rows, 9), 2); // after hard newline
    }
    #[test]
    fn multiline_paste_normalizes_crlf_while_login_stays_single_line() {
        assert_eq!(
            normalize_insert("first\r\n😀\rthird", true),
            "first\n😀\nthird"
        );
        assert_eq!(
            normalize_insert("first\r\n😀\rthird", false),
            "first 😀 third"
        );
    }
    #[test]
    fn marked_ime_selection_is_relative_to_inserted_unicode_multiline_text() {
        let inserted = "😀\né";
        assert_eq!(marked_selection(11, inserted, Some(&(2..4))), 15..18);
        assert_eq!(marked_selection(11, inserted, None), 18..18);
        assert_eq!(marked_selection(11, inserted, Some(&(0..0))), 11..11);
    }
    #[test]
    fn masked_mouse_offsets_never_split_unicode_or_combining_marks() {
        let text = "a😀e\u{301}";
        for offset in 1..5 {
            assert_eq!(grapheme_offset(text, offset), 1);
        }
        assert_eq!(grapheme_offset(text, 6), 5);
        assert_eq!(grapheme_offset(text, 7), 5);
        assert_eq!(grapheme_offset(text, 99), text.len());
    }
    #[test]
    fn ime_utf16_offsets_preserve_unicode_boundaries() {
        let text = "a😀é";
        assert_eq!(utf16_offset(text, 0), 0);
        assert_eq!(utf16_offset(text, 1), 1);
        assert_eq!(utf16_offset(text, 3), 5);
        assert_eq!(utf16_offset(text, 4), 7);
        assert_eq!(utf16_offset(text, 99), 7);
    }
    #[test]
    fn deleting_a_named_token_is_one_undo_step_with_original_wire_and_caret() {
        let text = "雪 <@103> suffix";
        let labels = std::collections::BTreeMap::from([("<@103>".into(), "@River".into())]);
        let tokens = super::composer_tokens::tokens(text, &labels);
        let end = tokens[0].source.end;
        let range = super::composer_tokens::edit_range(&tokens, end - 1..end);
        let before = edit_snapshot(text, end..end, false);
        let after_text = format!("{}{}", &text[..range.start], &text[range.end..]);
        let after = edit_snapshot(&after_text, range.start..range.start, false);
        let mut history = EditHistory::default();
        history.record(before.clone(), after.clone(), EditKind::Atomic);
        assert_eq!(history.undo(after), Some(before));
        assert_eq!(after_text, "雪  suffix");
    }
}

#[cfg(test)]
mod inline_navigation_tests {
    use super::*;
    #[test]
    fn reveal_offsets_scroll_only_as_needed_and_clamp_to_content() {
        // Window coordinates in a vertically scrolled timeline.
        assert_eq!(
            reveal_offset(px(150.), px(240.), px(260.), px(100.), px(300.), px(500.)),
            px(150.)
        );
        assert_eq!(
            reveal_offset(px(150.), px(70.), px(90.), px(100.), px(300.), px(500.)),
            px(120.)
        );
        assert_eq!(
            reveal_offset(px(150.), px(330.), px(350.), px(100.), px(300.), px(500.)),
            px(200.)
        );
        assert_eq!(
            reveal_offset(px(490.), px(330.), px(350.), px(100.), px(300.), px(500.)),
            px(500.)
        );
        // A far table cell, then a previous cell, in coordinates relative to the viewport.
        assert_eq!(
            reveal_offset(px(0.), px(250.), px(290.), px(0.), px(160.), px(240.)),
            px(130.)
        );
        assert_eq!(
            reveal_offset(px(130.), px(-55.), px(-5.), px(0.), px(160.), px(240.)),
            px(75.)
        );
        assert_eq!(
            reveal_offset(px(40.), px(10.), px(300.), px(0.), px(160.), px(240.)),
            px(50.)
        );
    }
    #[test]
    fn keyboard_targets_coalesce_styles_skip_code_and_preserve_spoiler_activation() {
        let message = fluxer_core::Message::text(
            100.into(),
            10.into(),
            fluxer_core::User::default(),
            "[first **bold**](https://example.test) `https://not-link.test` ||[hidden](https://hidden.test)|| [雪](https://snow.test)",
        );
        let doc = crate::message_markup::document(
            &message,
            &fluxer_core::ClientState::default(),
            None,
            crate::message_markup::MessageContext::Standard,
        );
        let hidden = BTreeSet::new();
        let targets = inline_targets(&doc, &hidden);
        assert_eq!(
            targets
                .iter()
                .map(|range| &doc.text[range.clone()])
                .collect::<Vec<_>>(),
            ["first bold", "hidden", "雪"]
        );
        let concealed = targets[1].start;
        assert!(message_text::target_at(&doc, &hidden, concealed).is_none());
        let id = message_text::hidden_at(&doc, &hidden, concealed).unwrap();
        let revealed = BTreeSet::from([id]);
        assert!(matches!(
            message_text::target_at(&doc, &revealed, concealed),
            Some(MessageTarget::Link(_))
        ));
        assert_eq!(inline_targets(&doc, &revealed), targets);
    }
}
