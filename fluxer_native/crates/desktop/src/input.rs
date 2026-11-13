// Adapted from GPUI 0.2.2 input example (Apache-2.0), Zed Industries.
use crate::message_markup::{MessageDocument, MessageTarget};
use std::ops::Range;
use std::{collections::BTreeSet, sync::Arc};
#[path = "inline_emoji.rs"]
mod inline_emoji;
#[path = "message_text.rs"]
mod message_text;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill,
    point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::*;

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
        ActivateInline,
        Quit,
    ]
);

pub struct TextInput {
    history: EditHistory,
    message_document: Option<Arc<MessageDocument>>,
    revealed_spoilers: BTreeSet<u32>,
    inline_press: Option<(Point<Pixels>, usize, bool)>,
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
}
impl gpui::EventEmitter<MessageActivated> for TextInput {}

impl TextInput {
    pub fn new(placeholder: &str, cx: &mut Context<Self>) -> Self {
        Self {
            history: EditHistory::default(),
            message_document: None,
            revealed_spoilers: BTreeSet::new(),
            inline_press: None,
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
        cx: &mut Context<Self>,
    ) -> Self {
        let document = document.into();
        let mut input = Self::read_only(&document.text, cx);
        input.message_document = Some(document);
        input
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
        let text_changed = self.content.as_ref() != document.text;
        if self
            .message_document
            .as_ref()
            .is_none_or(|old| old.source != document.source || old.text != document.text)
        {
            self.revealed_spoilers.clear();
        }
        if text_changed {
            self.content = document.text.clone().into();
            self.selected_range = 0..0;
            self.selection_reversed = false;
            self.selection_origin = None;
        }
        self.message_document = Some(document);
        self.inline_press = None;
        self.hover_interactive = false;
        self.last_layout = None;
        cx.notify();
    }
    fn activate_inline(&mut self, _: &ActivateInline, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.activate_inline_at(self.cursor_offset(), cx);
        }
    }
    fn activate_inline_at(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(document) = self.message_document.as_ref() else {
            return;
        };
        if let Some(id) = message_text::hidden_at(document, &self.revealed_spoilers, index) {
            self.revealed_spoilers.insert(id);
            self.last_layout = None;
            cx.notify();
            return;
        }
        if let Some(target) = message_text::target_at(document, &self.revealed_spoilers, index) {
            cx.emit(MessageActivated { target });
        }
    }

    pub fn insert_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.history.group = None;
        self.replace_text_in_range(None, text, window, cx);
        self.history.group = None;
    }

    pub fn set(&mut self, text: String, cx: &mut Context<Self>) {
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
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1, false, cx);
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1, false, cx);
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
        let next = (row as isize + direction)
            .clamp(0, layout.lines.len().saturating_sub(1) as isize) as usize;
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
        if !self.multiline || self.read_only {
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
        window.focus(&self.focus_handle);
        self.is_selecting = true;

        let index = self.index_for_mouse_position(event.position);
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
            let range = selection_unit(&self.content, index, event.click_count);
            self.move_to(range.start, cx);
            self.select_to(range.end, cx);
            self.selection_origin = Some(range);
        } else {
            self.move_to(index, cx)
        }
        cx.stop_propagation();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = false;
        if let Some((origin, index, dragged)) = self.inline_press.take()
            && !dragged
            && self.selected_range.is_empty()
            && !event.modifiers.shift
            && (event.position.x - origin.x).abs() <= px(3.)
            && (event.position.y - origin.y).abs() <= px(3.)
            && self.inline_index_for_mouse_position(event.position) == Some(index)
        {
            self.activate_inline_at(index, cx);
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
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
                let unit = selection_unit(&self.content, index, self.selection_clicks);
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
        self.history.group = None;
        if let Some(item) = cx.read_from_clipboard() {
            match paste_payload(item, self.attachment_paste && !self.read_only) {
                PastePayload::Text(text) => self.replace_text_in_range(None, &text, window, cx),
                PastePayload::Image(image) => cx.emit(image),
                PastePayload::Empty => {}
            }
        }
        self.history.group = None;
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                if let Some(document) = self.message_document.as_ref() {
                    if message_text::valid(document) {
                        document.copy_range(self.selected_range.clone(), &self.revealed_spoilers)
                    } else {
                        String::new()
                    }
                } else {
                    self.content[self.selected_range.clone()].to_string()
                },
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
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

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
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
        let row =
            ((local.y / layout.line_height) as usize).min(layout.lines.len().saturating_sub(1));
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

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
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
        let before = self.snapshot();
        let composing = self.marked_range.is_some();
        let kind = edit_kind(&before, &range, new_text);
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

/// Source byte ranges deliberately exclude hard newlines. A soft wrap shares
/// its boundary with the next row; a hard newline leaves a one-byte gap.
#[derive(Clone)]
struct VisualLine {
    range: Range<usize>,
    layout: inline_emoji::EmojiLine,
    indent: Pixels,
}
#[derive(Clone)]
struct DocumentLayout {
    lines: Vec<VisualLine>,
    line_height: Pixels,
}
impl DocumentLayout {
    fn height(&self) -> Pixels {
        self.line_height * self.lines.len()
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
            self.line_height * row,
        )
    }
    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        if position.y < px(0.) {
            return 0;
        }
        if position.y >= self.height() {
            return self.lines.last().map_or(0, |line| line.range.end);
        }
        let row = ((position.y.max(px(0.)) / self.line_height) as usize)
            .min(self.lines.len().saturating_sub(1));
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
                let runs = message_text::runs(
                    &document,
                    &text_style.to_run(document.text.len()),
                    &input.revealed_spoilers,
                );
                let mut style = Style::default();
                style.size.width = relative(1.).into();
                return (
                    window.request_measured_layout(style, move |known, available, window, _| {
                        let width = known.width.or(match available.width {
                            gpui::AvailableSpace::Definite(width) => Some(width),
                            _ => None,
                        });
                        let layout = message_text::layout(
                            &document,
                            &runs,
                            font_size,
                            line_height,
                            width,
                            window,
                        );
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
        if let Some(document) = input.message_document.as_ref() {
            let runs = message_text::runs(document, &base, &input.revealed_spoilers);
            lines = message_text::layout(
                document,
                &runs,
                font_size,
                window.line_height(),
                Some(bounds.size.width.max(px(1.))),
                window,
            )
            .lines;
        } else if input.multiline && !empty {
            for paragraph in logical_lines(&display) {
                let paragraph_runs =
                    decoration_runs(&base, &paragraph, input.marked_range.as_ref());
                let breaks = inline_emoji::wrap_breaks(
                    &display[paragraph.clone()],
                    font_size,
                    &paragraph_runs,
                    Some(bounds.size.width.max(px(1.))),
                    window,
                );
                for range in visual_ranges(paragraph, &breaks) {
                    let runs = decoration_runs(&base, &range, input.marked_range.as_ref());
                    let layout =
                        inline_emoji::shape_line(&display[range.clone()], font_size, &runs, window);
                    lines.push(VisualLine {
                        range,
                        layout,
                        indent: px(0.),
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
            let layout = inline_emoji::shape_line(&display, font_size, &runs, window);
            lines.push(VisualLine {
                range,
                layout,
                indent: px(0.),
            });
        }
        let layout = DocumentLayout {
            lines,
            line_height: window.line_height(),
        };
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
        if !input.selected_range.is_empty() {
            for (row, line) in layout.lines.iter().enumerate() {
                let range = &input.selected_range;
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
                        bounds.size.width
                    } else {
                        line.indent + line.layout.x_for_index(to)
                    };
                    if x2 > x1 {
                        selections.push(fill(
                            Bounds::new(
                                point(
                                    bounds.left() + x1 - scroll.x,
                                    bounds.top() + layout.line_height * row - scroll.y,
                                ),
                                size(x2 - x1, layout.line_height),
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
        let backgrounds = document
            .as_ref()
            .map(|document| message_text::backgrounds(document, &layout, bounds, scroll))
            .unwrap_or_default();
        let content = input.content.clone();
        let secret = input.secret;
        let mut emoji_images = document
            .as_ref()
            .map(|document| {
                message_text::emoji_images(document, &revealed, &layout, bounds, scroll, window, cx)
            })
            .unwrap_or_default();
        if !empty && !secret && document.as_ref().is_none_or(|doc| message_text::valid(doc)) {
            emoji_images.extend(inline_emoji::images(
                &content,
                document.as_deref(),
                &revealed,
                &layout,
                bounds,
                scroll,
                window,
                cx,
            ));
        }
        PrepaintState {
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
                && input.read(cx).is_selecting
            {
                input.update(cx, |input, cx| input.on_mouse_move(event, window, cx));
            }
        });
        for background in prepaint.backgrounds.drain(..) {
            window.paint_quad(background);
        }
        let input = self.input.read(cx);
        if input.read_only && input.message_document.is_some() {
            for (row, line) in prepaint.layout.lines.iter().enumerate() {
                let origin = point(
                    bounds.left() + line.indent - prepaint.scroll.x,
                    bounds.top() + prepaint.layout.line_height * row - prepaint.scroll.y,
                );
                if origin.y + prepaint.layout.line_height >= bounds.top()
                    && origin.y <= bounds.bottom()
                {
                    line.layout
                        .paint_background(origin, prepaint.layout.line_height, window, cx)
                        .expect("paint rich message run backgrounds");
                }
            }
        }
        for selection in prepaint.selections.drain(..) {
            if focus.is_focused(window) {
                window.paint_quad(selection);
            }
        }
        for (row, line) in prepaint.layout.lines.iter().enumerate() {
            let origin = point(
                bounds.left() + line.indent - prepaint.scroll.x,
                bounds.top() + prepaint.layout.line_height * row - prepaint.scroll.y,
            );
            if origin.y + prepaint.layout.line_height >= bounds.top() && origin.y <= bounds.bottom()
            {
                line.layout
                    .paint(origin, prepaint.layout.line_height, window, cx)
                    .expect("paint native composer");
            }
        }
        for image in &mut prepaint.emoji_images {
            image.paint(window, cx);
        }
        if focus.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.input.update(cx, |input, cx| {
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .when(self.read_only, |view| view.w_full().min_w_0())
            .key_context(if self.read_only {
                "SelectableText"
            } else if self.chat_picker {
                "TextInput ChatPicker"
            } else if self.multiline {
                "TextInput MultilineInput"
            } else {
                "TextInput"
            })
            .track_focus(&self.focus_handle(cx))
            .cursor(if self.hover_interactive {
                CursorStyle::PointingHand
            } else {
                CursorStyle::IBeam
            })
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
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .when(!self.read_only, |view| view.bg(rgb(0x292735)))
            .line_height(px(if self.read_only { 22. } else { 24. }))
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
            )
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub fn init(cx: &mut App) {
    cx.bind_keys([
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
        KeyBinding::new("enter", ActivateInline, Some("SelectableText")),
        KeyBinding::new("space", ActivateInline, Some("SelectableText")),
    ]);
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
    use super::{EditHistory, EditKind, EditSnapshot};
    use super::{
        PastePayload, grapheme_offset, logical_lines, marked_selection, normalize_insert,
        paste_payload, row_for_index, selection_unit, utf16_offset, visual_ranges, word_offset,
    };
    use gpui::ClipboardItem;
    use std::ops::Range;
    fn edit_snapshot(text: &str, selection: Range<usize>, reversed: bool) -> EditSnapshot {
        EditSnapshot {
            text: text.to_owned().into(),
            selection,
            reversed,
        }
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
}
