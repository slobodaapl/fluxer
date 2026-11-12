// Adapted from GPUI 0.2.2 input example (Apache-2.0), Zed Industries.
use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill,
    point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::*;

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
        Newline,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Quit,
    ]
);

pub struct TextInput {
    focus_handle: FocusHandle,
    pub content: SharedString,
    pub secret: bool,
    pub multiline: bool,
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

impl TextInput {
    pub fn new(placeholder: &str, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            secret: false,
            multiline: false,
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
    pub fn set(&mut self, text: String, cx: &mut Context<Self>) {
        self.content = normalize_insert(&text, self.multiline).into();
        self.selected_range = self.content.len()..self.content.len();
        self.marked_range = None;
        self.reveal_cursor = true;
        self.preferred_x = None;
        cx.notify();
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
            layout.lines[next].range.start + layout.lines[next].layout.closest_index_for_x(x),
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
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
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
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
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

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
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
        let new_text = normalize_insert(new_text, self.multiline);
        let new_text = new_text.as_str();
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
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
        let new_text = normalize_insert(new_text, self.multiline);
        let new_text = new_text.as_str();
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

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
    layout: ShapedLine,
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
            line.layout
                .x_for_index(index.saturating_sub(line.range.start).min(line.range.len())),
            self.line_height * row,
        )
    }
    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let row = ((position.y.max(px(0.)) / self.line_height) as usize)
            .min(self.lines.len().saturating_sub(1));
        let line = &self.lines[row];
        line.range.start
            + line
                .layout
                .closest_index_for_x(position.x)
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
        if input.multiline && !empty {
            let wrapped = window
                .text_system()
                .shape_text(
                    display.clone(),
                    font_size,
                    &[base.clone()],
                    Some(bounds.size.width.max(px(1.))),
                    None,
                )
                .expect("shape native composer text");
            let paragraphs = logical_lines(&display);
            for (paragraph, wrapped) in paragraphs.into_iter().zip(wrapped.into_iter()) {
                let breaks = wrapped
                    .wrap_boundaries
                    .iter()
                    .map(|boundary| wrapped.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
                    .collect::<Vec<_>>();
                for range in visual_ranges(paragraph, &breaks) {
                    let text: SharedString = display[range.clone()].to_owned().into();
                    let runs = decoration_runs(&base, &range, input.marked_range.as_ref());
                    let layout = window
                        .text_system()
                        .shape_line(text, font_size, &runs, None);
                    lines.push(VisualLine { range, layout });
                }
            }
        } else {
            let range = 0..if empty { 0 } else { display.len() };
            let runs = if empty {
                vec![base.clone()]
            } else {
                decoration_runs(&base, &range, input.marked_range.as_ref())
            };
            let layout = window
                .text_system()
                .shape_line(display, font_size, &runs, None);
            lines.push(VisualLine { range, layout });
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
        if input.reveal_cursor {
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
                    let x1 = line.layout.x_for_index(from);
                    let x2 = if range.end > line.range.end {
                        bounds.size.width
                    } else {
                        line.layout.x_for_index(to)
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
        let cursor = input.selected_range.is_empty().then(|| {
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
        PrepaintState {
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
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }
        for (row, line) in prepaint.layout.lines.iter().enumerate() {
            let origin = point(
                bounds.left() - prepaint.scroll.x,
                bounds.top() + prepaint.layout.line_height * row - prepaint.scroll.y,
            );
            if origin.y + prepaint.layout.line_height >= bounds.top() && origin.y <= bounds.bottom()
            {
                line.layout
                    .paint(origin, prepaint.layout.line_height, window, cx)
                    .expect("paint native composer");
            }
        }
        if focus.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.input.update(cx, |input, cx| {
            let rows = prepaint.layout.lines.len();
            if input.rows != rows {
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
            .key_context(if self.multiline {
                "TextInput MultilineInput"
            } else {
                "TextInput"
            })
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
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
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .bg(rgb(0x292735))
            .line_height(px(24.))
            .text_size(px(14.))
            .text_color(rgb(0xe9e7f2))
            .child(
                div()
                    .h(px(16.
                        + 24.
                            * if self.multiline {
                                self.rows.clamp(1, 6) as f32
                            } else {
                                1.
                            }))
                    .w_full()
                    .p(px(8.))
                    .rounded_md()
                    .overflow_hidden()
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
        KeyBinding::new("shift-enter", Newline, Some("MultilineInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("TextInput")),
        KeyBinding::new("ctrl-v", Paste, Some("TextInput")),
        KeyBinding::new("ctrl-c", Copy, Some("TextInput")),
        KeyBinding::new("ctrl-x", Cut, Some("TextInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("TextInput")),
        KeyBinding::new("cmd-v", Paste, Some("TextInput")),
        KeyBinding::new("cmd-c", Copy, Some("TextInput")),
        KeyBinding::new("cmd-x", Cut, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
    ]);
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

#[cfg(test)]
mod tests {
    use super::{
        grapheme_offset, logical_lines, marked_selection, normalize_insert, row_for_index,
        utf16_offset, visual_ranges,
    };
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
