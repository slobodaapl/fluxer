//! Rich read-only text uses the same byte-indexed runs for measuring, wrapping
//! and painting. Selection stays in projected text; copy uses the document's
//! spoiler-aware projection instead of the hidden backing string.
use super::*;
use crate::message_markup::{MessageBlockKind, MessageTarget};
use gpui::{AnyElement, AvailableSpace, FontStyle, FontWeight, ObjectFit, StrikethroughStyle, img};

pub(super) fn valid(document: &MessageDocument) -> bool {
    let mut end = 0;
    for span in &document.spans {
        if span.range.start != end
            || span.range.end < span.range.start
            || span.range.end > document.text.len()
            || !document.text.is_char_boundary(span.range.start)
            || !document.text.is_char_boundary(span.range.end)
        {
            return false;
        }
        end = span.range.end;
    }
    end == document.text.len()
}
fn concealed(span: &crate::message_markup::MessageSpan, revealed: &BTreeSet<u32>) -> bool {
    span.spoilers.iter().any(|id| !revealed.contains(id))
}
pub(super) fn hidden_at(
    document: &MessageDocument,
    revealed: &BTreeSet<u32>,
    index: usize,
) -> Option<u32> {
    if !valid(document) {
        return None;
    }
    document
        .spans
        .iter()
        .find(|span| span.range.contains(&index))
        .and_then(|span| {
            span.spoilers
                .iter()
                .find(|id| !revealed.contains(id))
                .copied()
        })
}
pub(super) fn target_at(
    document: &MessageDocument,
    revealed: &BTreeSet<u32>,
    index: usize,
) -> Option<MessageTarget> {
    if !valid(document) {
        return None;
    }
    document
        .spans
        .iter()
        .find(|span| span.range.contains(&index) && !concealed(span, revealed) && !span.style.code)
        .and_then(|span| span.target.as_ref())
        .filter(|target| {
            matches!(
                target,
                MessageTarget::Link(_)
                    | MessageTarget::Channel(_)
                    | MessageTarget::Author { .. }
                    | MessageTarget::User(_)
                    | MessageTarget::Role(_)
            )
        })
        .cloned()
}
pub(super) fn runs(
    document: &MessageDocument,
    base: &TextRun,
    revealed: &BTreeSet<u32>,
) -> Vec<TextRun> {
    if !valid(document) {
        return vec![TextRun {
            len: document.text.len(),
            color: rgba(0x00000000).into(),
            background_color: Some(rgb(0x34313b).into()),
            ..base.clone()
        }];
    }
    document
        .spans
        .iter()
        .filter(|span| !span.range.is_empty())
        .map(|span| {
            let mut run = TextRun {
                len: span.range.len(),
                ..base.clone()
            };
            if span.style.bold || span.style.heading > 0 {
                run.font.weight = FontWeight::BOLD;
            }
            if span.style.italic {
                run.font.style = FontStyle::Italic;
            }
            if span.style.code {
                run.font.family = "Fluxer Mono".into();
                run.background_color = Some(rgb(0x25222b).into());
            }
            if span.style.subtext || span.style.quote_depth > 0 {
                run.color = rgb(0xb1acbd).into();
            }
            if let Some(target) = &span.target {
                run.color = match target {
                    MessageTarget::Author { .. } => base.color,
                    MessageTarget::Link(_) => rgb(0x83bdf5).into(),
                    _ => rgb(0xb5a7ef).into(),
                };
                if !matches!(
                    target,
                    MessageTarget::Link(_) | MessageTarget::Author { .. }
                ) {
                    run.background_color = Some(rgba(0x8873ee20).into());
                }
            }
            if span.style.underline || matches!(span.target, Some(MessageTarget::Link(_))) {
                run.underline = Some(UnderlineStyle {
                    color: Some(run.color),
                    thickness: px(1.),
                    wavy: false,
                });
            }
            if span.style.strike {
                run.strikethrough = Some(StrikethroughStyle {
                    color: Some(run.color),
                    thickness: px(1.),
                });
            }
            if concealed(span, revealed) {
                run.color = rgba(0x00000000).into();
                run.background_color = Some(rgb(0x34313b).into());
                run.underline = None;
                run.strikethrough = None;
            }
            run
        })
        .collect()
}
/// Apply non-overlapping, byte-indexed syntax tokens without revealing masked runs.
pub(super) fn highlight_runs(runs: Vec<TextRun>, tokens: &[code_highlight::Token]) -> Vec<TextRun> {
    let mut output = Vec::new();
    let mut offset = 0;
    for run in runs {
        let end = offset + run.len;
        let mut cursor = offset;
        for token in tokens
            .iter()
            .filter(|t| t.range.start < end && t.range.end > offset)
        {
            let from = token.range.start.max(offset).max(cursor);
            let to = token.range.end.min(end);
            if from > cursor {
                output.push(TextRun {
                    len: from - cursor,
                    ..run.clone()
                });
            }
            if to > from {
                let mut part = TextRun {
                    len: to - from,
                    ..run.clone()
                };
                if run.color.a > 0. {
                    part.color = rgb(token.color).into();
                }
                output.push(part);
                cursor = to;
            }
        }
        if cursor < end {
            output.push(TextRun {
                len: end - cursor,
                ..run
            });
        }
        offset = end;
    }
    output
}

/// Clip globally indexed runs to a visual row without splitting their styles.
pub(super) fn clip(runs: &[TextRun], range: &Range<usize>) -> Vec<TextRun> {
    let mut offset = 0;
    runs.iter()
        .filter_map(|run| {
            let start = offset;
            offset += run.len;
            let from = start.max(range.start);
            let to = offset.min(range.end);
            (to > from).then(|| TextRun {
                len: to - from,
                ..run.clone()
            })
        })
        .collect()
}
/// Glyphs at a soft-wrap boundary belong to the downstream row. Carets use
/// upstream affinity, so their position helper cannot place inline images.
fn glyph_row(rows: impl IntoIterator<Item = Range<usize>>, index: usize) -> Option<usize> {
    rows.into_iter().position(|range| range.contains(&index))
}
fn paragraph_indent(document: &MessageDocument, paragraph: &Range<usize>) -> Pixels {
    let mut quote = 0;
    let mut code = false;
    let mut list = 0;
    let mut alert = false;
    for block in &document.blocks {
        if block.range.start <= paragraph.start && block.range.end > paragraph.start {
            match block.kind {
                MessageBlockKind::Quote { depth } => quote = quote.max(depth.min(4)),
                MessageBlockKind::Code { .. } => code = true,
                MessageBlockKind::List { depth, .. } => list = list.max(depth.min(8)),
                MessageBlockKind::Alert { .. } => alert = true,
                _ => {}
            }
        }
    }
    px(quote as f32 * 10.
        + list as f32 * 16.
        + if code { 8. } else { 0. }
        + if alert { 16. } else { 0. })
}
pub(super) fn layout(
    document: &MessageDocument,
    runs: &[TextRun],
    font_size: Pixels,
    line_height: Pixels,
    width: Option<Pixels>,
    table_scrolls: &BTreeMap<usize, Pixels>,
    window: &mut Window,
) -> DocumentLayout {
    let mut lines = Vec::new();
    let mut y = px(0.);
    let mut code_headers = Vec::new();
    let mut table_cells = Vec::new();
    let mut table_regions = Vec::new();
    let mut table_end = 0;
    for paragraph in logical_lines(&document.text) {
        if paragraph.start < table_end {
            continue;
        }
        if let Some(block) = document.blocks.iter().find(|block| {
            block.range.start == paragraph.start
                && matches!(block.kind, MessageBlockKind::Table { .. })
        }) {
            if let MessageBlockKind::Table { rows, alignments } = &block.kind {
                let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
                if columns > 0 {
                    let available = width.unwrap_or(px(480.));
                    let cell_width = (available / columns as f32).max(px(80.));
                    let table_width = cell_width * columns;
                    let offset = table_scrolls
                        .get(&block.range.start)
                        .copied()
                        .unwrap_or(px(0.))
                        .clamp(px(0.), (table_width - available).max(px(0.)));
                    let top = y;
                    for (row_index, row) in rows.iter().enumerate() {
                        let mut column_lines = Vec::new();
                        let mut row_height = line_height;
                        for (column, cell) in row.iter().enumerate() {
                            let text = &document.text[cell.clone()];
                            let cell_runs = clip(runs, cell);
                            let breaks = inline_emoji::wrap_breaks(
                                text,
                                font_size,
                                &cell_runs,
                                Some((cell_width - px(24.)).max(px(1.))),
                                window,
                            );
                            let mut shaped = Vec::new();
                            for range in visual_ranges(cell.clone(), &breaks) {
                                let shape = inline_emoji::shape_line(
                                    &document.text[range.clone()],
                                    font_size,
                                    &clip(runs, &range),
                                    window,
                                );
                                shaped.push((range, shape));
                            }
                            row_height = row_height.max(line_height * shaped.len());
                            column_lines.push((column, shaped));
                        }
                        row_height += px(12.);
                        for column in 0..columns {
                            table_cells.push(TableCell {
                                x: cell_width * column - offset,
                                y,
                                width: cell_width,
                                height: row_height,
                                header: row_index == 0,
                                row: row_index,
                            });
                        }
                        for (column, shaped) in column_lines {
                            for (line_index, (range, shape)) in shaped.into_iter().enumerate() {
                                let left = cell_width * column - offset;
                                let inner = (cell_width - px(24.)).max(px(1.));
                                let alignment = alignments
                                    .get(column)
                                    .copied()
                                    .unwrap_or(fluxer_markdown_parser::ast::TableAlignment::None);
                                let shift = match alignment {
                                    fluxer_markdown_parser::ast::TableAlignment::Center => {
                                        ((inner - shape.width) / 2.).max(px(0.))
                                    }
                                    fluxer_markdown_parser::ast::TableAlignment::Right => {
                                        (inner - shape.width).max(px(0.))
                                    }
                                    _ => px(0.),
                                };
                                lines.push(VisualLine {
                                    range,
                                    layout: shape,
                                    indent: left + px(12.) + shift,
                                    y: y + px(6.) + line_height * line_index,
                                    height: line_height,
                                    cell_right: Some(left + cell_width - px(12.)),
                                });
                            }
                        }
                        y += row_height;
                    }
                    if table_width > available {
                        y += px(8.);
                    }
                    table_regions.push(TableRegion {
                        start: block.range.start,
                        y: top,
                        height: y - top,
                        width: table_width,
                        viewport: available,
                        offset,
                    });
                    table_end = block.range.end;
                    continue;
                }
            }
        }
        for block in &document.blocks {
            if block.range.start == paragraph.start {
                if let MessageBlockKind::Code { language } = &block.kind {
                    code_headers.push(CodeHeader {
                        range: block.range.clone(),
                        language: language.clone(),
                        y,
                    });
                    y += line_height;
                }
            }
        }
        let indent = paragraph_indent(document, &paragraph);
        let heading_block = document.blocks.iter().find(|block| {
            block.range.start <= paragraph.start
                && block.range.end > paragraph.start
                && matches!(block.kind, MessageBlockKind::Heading { .. })
        });
        let heading = heading_block.and_then(|block| {
            if let MessageBlockKind::Heading { level } = block.kind {
                Some(level)
            } else {
                None
            }
        });
        if let Some(block) = heading_block.filter(|block| block.range.start == paragraph.start) {
            let body_start = document
                .spans
                .iter()
                .take(2)
                .filter(|span| {
                    matches!(span.target, Some(MessageTarget::Author { .. }))
                        || (span.range.start > 0
                            && document.text[span.range.clone()].trim().is_empty())
                })
                .map(|span| span.range.end)
                .max()
                .unwrap_or(0)
                .min(block.range.start);
            let first = document.text[body_start..block.range.start]
                .trim()
                .is_empty();
            y += window.rem_size() * if first { 0.25 } else { 1.25 };
        }
        let scale = match heading {
            Some(1) => 1.375,
            Some(2) => 1.25,
            Some(3) => 1.125,
            Some(5) => 0.9375,
            Some(6) => 0.875,
            _ => 1.,
        };
        let row_font_size = font_size * scale;
        let row_height = if heading.is_some() {
            row_font_size * 1.2
        } else {
            line_height
        };
        let text: SharedString = document.text[paragraph.clone()].to_owned().into();
        let paragraph_runs = clip(runs, &paragraph);
        let breaks = inline_emoji::wrap_breaks(
            &text,
            row_font_size,
            &paragraph_runs,
            width.map(|width| (width - indent).max(px(1.))),
            window,
        );
        for range in visual_ranges(paragraph.clone(), &breaks) {
            let text: SharedString = document.text[range.clone()].to_owned().into();
            let layout =
                inline_emoji::shape_line(&text, row_font_size, &clip(runs, &range), window);
            lines.push(VisualLine {
                range,
                layout,
                indent,
                y,
                height: row_height,
                cell_right: None,
            });
            y += row_height;
        }
        if heading_block.is_some_and(|block| block.range.end == paragraph.end) {
            y += window.rem_size() * 0.75;
        }
    }
    DocumentLayout {
        lines,
        code_headers,
        table_cells,
        table_regions,
        content_height: y,
        line_height,
    }
}
// Rebuild header geometry from the first code row after the shared layout has
// been moved into the prepaint state.
pub(super) fn code_headers(
    document: &MessageDocument,
    lines: &[VisualLine],
    height: Pixels,
) -> Vec<CodeHeader> {
    document
        .blocks
        .iter()
        .filter_map(|block| {
            let MessageBlockKind::Code { language } = &block.kind else {
                return None;
            };
            let line = lines
                .iter()
                .find(|line| line.range.start == block.range.start)?;
            Some(CodeHeader {
                range: block.range.clone(),
                language: language.clone(),
                y: line.y - height,
            })
        })
        .collect()
}

fn code_for_copy(
    document: &MessageDocument,
    range: &Range<usize>,
    revealed: &BTreeSet<u32>,
) -> Option<String> {
    if !valid(document)
        || document.spans.iter().any(|span| {
            span.range.start < range.end
                && span.range.end > range.start
                && concealed(span, revealed)
        })
    {
        return None;
    }
    document.text.get(range.clone()).map(str::to_owned)
}

pub(super) fn code_controls(
    document: &MessageDocument,
    revealed: &BTreeSet<u32>,
    layout: &DocumentLayout,
    bounds: Bounds<Pixels>,
    scroll: Point<Pixels>,
    owner: u64,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let mut controls = Vec::new();
    for (index, header) in layout.code_headers.iter().enumerate() {
        let Some(code) = code_for_copy(document, &header.range, revealed) else {
            continue;
        };
        let y = bounds.top() + header.y - scroll.y;
        if y + layout.line_height < bounds.top() || y > bounds.bottom() {
            continue;
        }
        let label: String = header
            .language
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(40)
            .collect();
        let width = bounds.size.width;
        let keyboard_code = code.clone();
        let mut element = div()
            .id(SharedString::from(format!("code-header-{owner}-{index}")))
            .w(width)
            .h(layout.line_height)
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .bg(rgb(0x25222b))
            .text_size(px(11.))
            .text_color(rgb(0xa9a4b5))
            .child(label)
            .child(
                div()
                    .id(("copy-code", index))
                    .px_2()
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .focus(|style| style.bg(rgb(0x514a5d)).text_color(rgb(0xffffff)))
                    .on_key_down(move |event, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            cx.write_to_clipboard(ClipboardItem::new_string(keyboard_code.clone()));
                            cx.stop_propagation();
                        }
                    })
                    .text_color(rgb(0xc8c2d4))
                    .hover(|style| style.text_color(rgb(0xffffff)))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                        cx.stop_propagation();
                    })
                    .child("Copy"),
            )
            .into_any_element();
        element.layout_as_root(
            size(
                AvailableSpace::Definite(width),
                AvailableSpace::Definite(layout.line_height),
            ),
            window,
            cx,
        );
        element.prepaint_at(point(bounds.left() - scroll.x, y), window, cx);
        controls.push(element);
    }
    controls
}

pub(super) fn backgrounds(
    document: &MessageDocument,
    layout: &DocumentLayout,
    bounds: Bounds<Pixels>,
    scroll: Point<Pixels>,
) -> Vec<PaintQuad> {
    let mut quads = Vec::new();
    for table in &layout.table_regions {
        if table.width <= table.viewport {
            continue;
        }
        let y = bounds.top() + table.y + table.height - px(6.) - scroll.y;
        let thumb = (table.viewport * (table.viewport / table.width))
            .max(px(20.))
            .min(table.viewport);
        let travel = table.viewport - thumb;
        let x = bounds.left()
            + if table.width > table.viewport {
                travel * (table.offset / (table.width - table.viewport))
            } else {
                px(0.)
            };
        quads.push(fill(
            Bounds::new(point(bounds.left(), y), size(table.viewport, px(4.))),
            rgb(0x24212a),
        ));
        quads.push(fill(
            Bounds::new(point(x, y), size(thumb, px(4.))),
            rgb(0x777080),
        ));
    }
    for cell in &layout.table_cells {
        let left = (bounds.left() + cell.x - scroll.x).max(bounds.left());
        let right = (bounds.left() + cell.x + cell.width - scroll.x).min(bounds.right());
        let top = bounds.top() + cell.y - scroll.y;
        if right <= left || top + cell.height < bounds.top() || top > bounds.bottom() {
            continue;
        }
        let color = if cell.header {
            0x302c38
        } else if cell.row % 2 == 0 {
            0x25232b
        } else {
            0x292630
        };
        quads.push(fill(
            Bounds::new(point(left, top), size(right - left, cell.height)),
            rgb(color),
        ));
        quads.push(fill(
            Bounds::new(
                point(left, top + cell.height - px(1.)),
                size(right - left, px(1.)),
            ),
            rgb(0x514c5b),
        ));
        if cell.x >= px(0.) {
            quads.push(fill(
                Bounds::new(point(left, top), size(px(1.), cell.height)),
                rgb(0x514c5b),
            ));
        }
    }
    for line in &layout.lines {
        let y = bounds.top() + line.y - scroll.y;
        if y + line.height < bounds.top() || y > bounds.bottom() {
            continue;
        }
        for block in &document.blocks {
            if block.range.start <= line.range.start && block.range.end > line.range.start {
                match block.kind {
                    MessageBlockKind::Code { .. } => quads.push(fill(
                        Bounds::new(
                            point(bounds.left(), y),
                            size(bounds.size.width, line.height),
                        ),
                        rgb(0x25222b),
                    )),
                    MessageBlockKind::Alert { kind } => {
                        use fluxer_markdown_parser::ast::AlertType;
                        let color = match kind {
                            AlertType::Note => 0x5b9cf6,
                            AlertType::Tip => 0x55b985,
                            AlertType::Important => 0xad81ef,
                            AlertType::Warning => 0xe5b658,
                            AlertType::Caution => 0xe26767,
                        };
                        quads.push(fill(
                            Bounds::new(point(bounds.left(), y), size(px(4.), line.height)),
                            rgb(color),
                        ));
                    }
                    MessageBlockKind::Quote { depth } => {
                        for level in 0..depth.min(4) {
                            quads.push(fill(
                                Bounds::new(
                                    point(bounds.left() + px(level as f32 * 10.), y),
                                    size(px(2.), line.height),
                                ),
                                rgb(0x645f70),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    quads
}
fn emoji_fallback() -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.))
        .text_color(rgb(0xa19bb3))
        .child("◇")
        .into_any_element()
}
pub(super) fn emoji_images(
    document: &MessageDocument,
    revealed: &BTreeSet<u32>,
    layout: &DocumentLayout,
    bounds: Bounds<Pixels>,
    scroll: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    if !valid(document) {
        return Vec::new();
    }
    let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
    let mut images = Vec::new();
    for span in &document.spans {
        let Some(emoji) = &span.custom_emoji else {
            continue;
        };
        if concealed(span, revealed) {
            continue;
        }
        let Some(row) = glyph_row(
            layout.lines.iter().map(|line| line.range.clone()),
            span.range.start,
        ) else {
            continue;
        };
        let line = &layout.lines[row];
        let x = line.indent + line.layout.x_for_index(span.range.start - line.range.start);
        let end_x = line.indent
            + line
                .layout
                .x_for_index(span.range.end.min(line.range.end) - line.range.start);
        let width = (end_x - x).max(px(1.));
        let height = (line.height - px(2.)).max(px(1.));
        let origin = point(
            bounds.left() + x - scroll.x,
            bounds.top() + line.y - scroll.y + px(1.),
        );
        if !Bounds::new(origin, size(width, height)).intersects(&viewport) {
            continue;
        }
        if images.len() >= 64 {
            break;
        }
        let mut element = if let Some(url) = emoji
            .url
            .as_ref()
            .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        {
            img(SharedString::from(url.clone()))
                .w(width)
                .h(height)
                .object_fit(ObjectFit::Contain)
                .with_loading(emoji_fallback)
                .with_fallback(emoji_fallback)
                .into_any_element()
        } else {
            div()
                .w(width)
                .h(height)
                .child(emoji_fallback())
                .into_any_element()
        };
        element.layout_as_root(
            size(
                AvailableSpace::Definite(width),
                AvailableSpace::Definite(height),
            ),
            window,
            cx,
        );
        element.prepaint_at(origin, window, cx);
        images.push(element);
    }
    images
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_markup::{self, MessageContext};
    use fluxer_core::{ClientState, Message, User};

    fn document(source: &str) -> MessageDocument {
        message_markup::document(
            &Message::text(100.into(), 10.into(), User::default(), source),
            &ClientState::default(),
            Some("https://media.example"),
            MessageContext::Standard,
        )
    }
    fn base() -> TextRun {
        TextRun {
            len: 0,
            font: gpui::font("sans-serif"),
            color: rgb(0xffffff).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    #[test]
    fn syntax_colors_never_reveal_masked_ranges_and_preserve_byte_partition() {
        let doc = document("||secret 雪|| visible");
        let tokens = vec![code_highlight::Token {
            range: 0..doc.text.len(),
            color: 0xff7b72,
        }];
        let result = highlight_runs(runs(&doc, &base(), &BTreeSet::new()), &tokens);
        assert_eq!(
            result.iter().map(|run| run.len).sum::<usize>(),
            doc.text.len()
        );
        assert_eq!(result[0].color.a, 0.);
        assert!(result.last().unwrap().color.a > 0.);
    }

    #[test]
    fn code_copy_uses_exact_block_text_and_respects_concealment() {
        let doc = document("```rust\nlet 雪 = \"✨\";\n```");
        let block = doc
            .blocks
            .iter()
            .find(|block| matches!(block.kind, MessageBlockKind::Code { .. }))
            .unwrap();
        assert_eq!(
            code_for_copy(&doc, &block.range, &BTreeSet::new()).as_deref(),
            Some("let 雪 = \"✨\";\n")
        );
        let mut concealed = doc.clone();
        for span in &mut concealed.spans {
            span.spoilers.push(7);
        }
        assert!(code_for_copy(&concealed, &block.range, &BTreeSet::new()).is_none());
        assert!(code_for_copy(&concealed, &block.range, &BTreeSet::from([7])).is_some());
    }

    #[test]
    fn compact_author_preserves_source_spoilers_ranges_and_exact_hit_target() {
        let message = Message::text(
            100.into(),
            10.into(),
            User {
                username: "Aléx 🙂".into(),
                ..Default::default()
            },
            "**hello** https://example.test ||hidden||\n> quote",
        );
        let original = document(&message.content);
        let doc = original.with_author(&message);
        assert!(valid(&doc));
        assert_eq!(doc.source, original.source);
        let name = message.author.display_name();
        assert_eq!(
            target_at(&doc, &BTreeSet::new(), 0),
            Some(MessageTarget::Author {
                channel: message.channel_id,
                message: message.id
            })
        );
        assert!(target_at(&doc, &BTreeSet::new(), name.len()).is_none());
        assert_eq!(
            doc.blocks[0].range.start,
            original.blocks[0].range.start + name.len() + 1
        );
        assert!(matches!(
            target_at(&doc, &BTreeSet::new(), doc.text.find("https:").unwrap()),
            Some(MessageTarget::Link(_))
        ));
        assert!(
            !doc.copy_range(0..doc.text.len(), &BTreeSet::new())
                .contains("hidden")
        );
        let styled = runs(&doc, &base(), &BTreeSet::new());
        assert_eq!(styled[0].color, base().color);
        assert!(styled[0].background_color.is_none());
        assert_eq!(styled[0].font.weight, FontWeight::BOLD);
        assert_eq!(
            styled.iter().map(|run| run.len).sum::<usize>(),
            doc.text.len()
        );
    }

    #[test]
    fn compact_author_keeps_initial_quote_and_code_in_their_own_paragraph() {
        let message = Message::text(
            100.into(),
            10.into(),
            User {
                username: "Alex".into(),
                ..Default::default()
            },
            "",
        );
        for source in ["> quoted text", "```rust\nlet example = 1;\n```"] {
            let original = document(source);
            assert_eq!(original.blocks[0].range.start, 0);
            let doc = original.with_author(&message);
            assert!(valid(&doc));
            assert!(doc.text.starts_with("Alex\n"));
            let content_start = "Alex\n".len();
            assert!(paragraph_indent(&doc, &(content_start..doc.text.len())) > px(0.));
            assert_eq!(paragraph_indent(&doc, &(0.."Alex".len())), px(0.));
            assert_eq!(doc.blocks[0].range.start, content_start);
        }
    }

    #[test]
    fn actual_ast_nested_unicode_styles_survive_visual_row_clipping() {
        let doc = document("**bold *ital😄* end** __under__ ~~gone~~ `code`");
        assert!(valid(&doc));
        let styled = runs(&doc, &base(), &BTreeSet::new());
        assert_eq!(
            styled.iter().map(|run| run.len).sum::<usize>(),
            doc.text.len()
        );
        let start = doc.text.find("ital").unwrap();
        let end = start + "ital😄".len();
        let row = clip(&styled, &(start..end));
        assert_eq!(row.iter().map(|run| run.len).sum::<usize>(), "ital😄".len());
        assert!(
            row.iter()
                .all(|run| run.font.weight == FontWeight::BOLD
                    && run.font.style == FontStyle::Italic)
        );
        for (word, test) in [("under", 0), ("gone", 1), ("code", 2)] {
            let start = doc.text.find(word).unwrap();
            let row = clip(&styled, &(start..start + word.len()));
            assert!(row.iter().all(|run| match test {
                0 => run.underline.is_some(),
                1 => run.strikethrough.is_some(),
                _ => run.font.family.as_ref() == "Fluxer Mono" && run.background_color.is_some(),
            }));
        }
    }

    #[test]
    fn hidden_link_is_inert_and_transparent_until_its_spoiler_is_revealed() {
        let doc = document("start ||~~[secret😄](https://example.org)~~ <:party:123>|| end");
        let start = doc.text.find("secret").unwrap();
        let mut revealed = BTreeSet::new();
        let id = hidden_at(&doc, &revealed, start).unwrap();
        assert!(target_at(&doc, &revealed, start).is_none());
        let masked = clip(
            &runs(&doc, &base(), &revealed),
            &(start..start + "secret😄".len()),
        );
        assert!(masked.iter().all(|run| run.color.a == 0.
            && run.underline.is_none()
            && run.strikethrough.is_none()));
        let emoji = doc
            .spans
            .iter()
            .find(|span| span.custom_emoji.is_some())
            .unwrap();
        assert!(concealed(emoji, &revealed));
        assert_eq!(doc.copy_range(0..doc.text.len(), &revealed), "start  end");
        revealed.insert(id);
        assert!(hidden_at(&doc, &revealed, start).is_none());
        assert_eq!(
            target_at(&doc, &revealed, start),
            Some(MessageTarget::Link("https://example.org/".into()))
        );
        assert!(!concealed(emoji, &revealed));
        assert_eq!(
            doc.copy_range(0..doc.text.len(), &revealed),
            "start secret😄 :party: end"
        );
    }

    #[test]
    fn malformed_byte_ranges_fail_closed_and_nonactionable_mentions_stay_text() {
        let mut doc = MessageDocument::plain("雪");
        doc.spans[0].target = Some(MessageTarget::User(12.into()));
        assert_eq!(
            target_at(&doc, &BTreeSet::new(), 0),
            Some(MessageTarget::User(12.into()))
        );
        doc.spans[0].target = Some(MessageTarget::Role(34.into()));
        assert_eq!(
            target_at(&doc, &BTreeSet::new(), 0),
            Some(MessageTarget::Role(34.into()))
        );
        doc.spans[0].target = Some(MessageTarget::Channel(56.into()));
        assert_eq!(
            target_at(&doc, &BTreeSet::new(), 0),
            Some(MessageTarget::Channel(56.into()))
        );
        doc.spans[0].range.end = 1;
        assert!(!valid(&doc));
        assert!(target_at(&doc, &BTreeSet::new(), 0).is_none());
        assert!(
            runs(&doc, &base(), &BTreeSet::new())
                .iter()
                .all(|run| run.color.a == 0.)
        );
    }

    #[test]
    fn inline_emoji_at_soft_wrap_start_uses_downstream_glyph_row() {
        let rows = vec![0..3, 3..8, 9..12];
        assert_eq!(glyph_row(rows.clone(), 0), Some(0));
        assert_eq!(glyph_row(rows.clone(), 3), Some(1));
        assert_eq!(glyph_row(rows.clone(), 8), None); // hard newline
        assert_eq!(glyph_row(rows.clone(), 9), Some(2));
        assert_eq!(glyph_row(rows, 12), None);
    }
}
