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
fn alert_color(kind: fluxer_markdown_parser::ast::AlertType) -> u32 {
    use fluxer_markdown_parser::ast::AlertType;
    match kind {
        AlertType::Note => 0x5b9cf6,
        AlertType::Tip => 0x55b985,
        AlertType::Important => 0xad81ef,
        AlertType::Warning => 0xe5b658,
        AlertType::Caution => 0xe26767,
    }
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
            if let Some(kind) = document.blocks.iter().find_map(|block| {
                if block.range.start == span.range.start
                    && let MessageBlockKind::Alert { kind } = block.kind
                {
                    return Some(kind);
                }
                None
            }) {
                run.color = rgb(alert_color(kind)).into();
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
fn heading_is_first(document: &MessageDocument, start: usize) -> bool {
    let body_start = if document
        .spans
        .first()
        .is_some_and(|span| matches!(span.target, Some(MessageTarget::Author { .. })))
    {
        document.spans.get(1).map_or(0, |span| span.range.end)
    } else {
        0
    }
    .min(start);
    document.text[body_start..start].trim().is_empty()
}
/// CodeElements render literal code text. Only non-code spans participate in
/// image replacement, bitmap painting, and emoji-driven row height.
pub(super) fn unicode_image_at(document: &MessageDocument, index: usize) -> bool {
    document
        .spans
        .get(
            document
                .spans
                .partition_point(|span| span.range.end <= index),
        )
        .is_some_and(|span| span.range.contains(&index) && !span.style.code)
}
pub(super) fn inline_emoji_height(
    document: &MessageDocument,
    range: &Range<usize>,
    font_size: Pixels,
) -> Option<Pixels> {
    let has_custom = document.spans.iter().any(|span| {
        span.custom_emoji.is_some()
            && !span.style.code
            && span.range.start >= range.start
            && span.range.start < range.end
    });
    let has_unicode =
        document.text[range.clone()]
            .grapheme_indices(true)
            .any(|(start, grapheme)| {
                crate::chat_helpers::unicode_catalog_contains(grapheme)
                    && unicode_image_at(document, range.start + start)
            });
    (has_custom || has_unicode).then_some(
        font_size
            * if document.restricted_inline {
                1.25
            } else {
                1.5
            },
    )
}
/// Labels occupy atomic source ranges; layout, hit testing, selection and image
/// placement use one projection. Never rewrite document.text or source offsets.
fn display_tokens(document: &MessageDocument, current: i64) -> Vec<composer_tokens::Token> {
    let mut tokens: Vec<_> = document
        .timestamps
        .iter()
        .map(|timestamp| composer_tokens::Token {
            source: timestamp.range.clone(),
            label: crate::message_timestamp::format(
                timestamp.seconds,
                timestamp.style,
                &document.timestamp_preferences,
                current,
            ),
        })
        .collect();
    let spacer = if document.jumbo_emojis {
        "\u{2003}"
    } else if document.restricted_inline {
        "\u{2003}\u{2005}"
    } else {
        "\u{2003}\u{2002}"
    };
    tokens.extend(
        document
            .spans
            .iter()
            .filter(|span| span.custom_emoji.is_some() && !span.style.code)
            .map(|span| composer_tokens::Token {
                source: span.range.clone(),
                label: spacer.into(),
            }),
    );
    tokens.extend(
        document
            .text
            .grapheme_indices(true)
            .filter(|(start, grapheme)| {
                crate::chat_helpers::unicode_catalog_contains(grapheme)
                    && unicode_image_at(document, *start)
            })
            .map(|(start, grapheme)| composer_tokens::Token {
                source: start..start + grapheme.len(),
                label: spacer.into(),
            }),
    );
    tokens.sort_by_key(|token| token.source.start);
    tokens
}
fn hanging_rows(
    paragraph: Range<usize>,
    split: usize,
    breaks: &[usize],
    indent: Pixels,
    hanging: Pixels,
) -> Vec<(Range<usize>, Pixels)> {
    let mut rows = vec![(paragraph.start..split, indent)];
    rows.extend(
        visual_ranges(split..paragraph.end, breaks)
            .into_iter()
            .map(|range| (range, indent + hanging)),
    );
    rows
}
const TABLE_CELL_PADDING: f32 = 24.;
/// Atomic display labels cannot wrap at source-byte boundaries. Give their
/// column enough intrinsic width and let the table's existing scroller expose
/// the complete label instead of painting it across the neighboring cell.
fn table_column_widths(
    rows: &[Vec<Range<usize>>],
    columns: usize,
    available: Pixels,
    atomic: &[(Range<usize>, Pixels)],
) -> Vec<Pixels> {
    let mut widths = vec![(available / columns.max(1) as f32).max(px(80.)); columns];
    for row in rows {
        for (column, cell) in row.iter().enumerate() {
            for (range, width) in atomic {
                if range.start >= cell.start && range.end <= cell.end {
                    widths[column] = widths[column].max(*width + px(TABLE_CELL_PADDING));
                }
            }
        }
    }
    widths
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
    let tokens = display_tokens(document, crate::message_timestamp::now());
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
        }) && let MessageBlockKind::Table { rows, alignments } = &block.kind
        {
            let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
            if columns > 0 {
                let available = width.unwrap_or(px(480.));
                let atomic: Vec<_> = tokens
                    .iter()
                    .filter(|token| {
                        token.source.start >= block.range.start
                            && token.source.end <= block.range.end
                    })
                    .map(|token| {
                        let token_runs = clip(runs, &token.source);
                        let local =
                            composer_tokens::clip(std::slice::from_ref(token), &token.source);
                        let width = inline_emoji::shape_message_line(
                            &document.text[token.source.clone()],
                            font_size,
                            &token_runs,
                            &local,
                            window,
                        )
                        .width;
                        (token.source.clone(), width)
                    })
                    .collect();
                let column_widths = table_column_widths(rows, columns, available, &atomic);
                let mut table_width = px(0.);
                let column_offsets: Vec<_> = column_widths
                    .iter()
                    .map(|width| {
                        let x = table_width;
                        table_width += *width;
                        x
                    })
                    .collect();
                let offset = table_scrolls
                    .get(&block.range.start)
                    .copied()
                    .unwrap_or(px(0.))
                    .clamp(px(0.), (table_width - available).max(px(0.)));
                let top = y;
                for (row_index, row) in rows.iter().enumerate() {
                    let mut column_lines = Vec::new();
                    let cell_line_height = row
                        .iter()
                        .filter_map(|cell| inline_emoji_height(document, cell, font_size))
                        .fold(line_height, Pixels::max);
                    let mut row_height = cell_line_height;
                    for (column, cell) in row.iter().enumerate() {
                        let cell_width = column_widths[column];
                        let text = &document.text[cell.clone()];
                        let cell_runs = clip(runs, cell);
                        let cell_tokens = composer_tokens::clip(&tokens, cell);
                        let breaks = inline_emoji::wrap_message_breaks(
                            text,
                            font_size,
                            &cell_runs,
                            Some((cell_width - px(24.)).max(px(1.))),
                            &cell_tokens,
                            window,
                        );
                        let mut shaped = Vec::new();
                        for range in visual_ranges(cell.clone(), &breaks) {
                            let shape = inline_emoji::shape_message_line(
                                &document.text[range.clone()],
                                font_size,
                                &clip(runs, &range),
                                &composer_tokens::clip(&tokens, &range),
                                window,
                            );
                            shaped.push((range, shape));
                        }
                        row_height = row_height.max(cell_line_height * shaped.len());
                        column_lines.push((column, shaped));
                    }
                    row_height += px(12.);
                    for column in 0..columns {
                        table_cells.push(TableCell {
                            range: row.get(column).cloned(),
                            x: column_offsets[column] - offset,
                            y,
                            width: column_widths[column],
                            height: row_height,
                            header: row_index == 0,
                            row: row_index,
                        });
                    }
                    for (column, shaped) in column_lines {
                        for (line_index, (range, shape)) in shaped.into_iter().enumerate() {
                            let left = column_offsets[column] - offset;
                            let cell_width = column_widths[column];
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
                                y: y + px(6.) + cell_line_height * line_index,
                                height: cell_line_height,
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
        for block in &document.blocks {
            if block.range.start == paragraph.start
                && let MessageBlockKind::Code { language } = &block.kind
            {
                code_headers.push(CodeHeader {
                    range: block.range.clone(),
                    language: language.clone(),
                    y,
                });
                y += line_height;
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
            let first = heading_is_first(document, block.range.start);
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
        let jumbo_size = window.rem_size() * 3.;
        let emoji_height = inline_emoji_height(document, &paragraph, row_font_size);
        let row_height = if document.jumbo_emojis && emoji_height.is_some() {
            jumbo_size + px(2.)
        } else if let Some(emoji_height) = emoji_height {
            (if heading.is_some() {
                row_font_size * 1.2
            } else {
                line_height
            })
            .max(emoji_height)
        } else if heading.is_some() {
            row_font_size * 1.2
        } else {
            line_height
        };
        let text: SharedString = document.text[paragraph.clone()].to_owned().into();
        let paragraph_runs = clip(runs, &paragraph);
        let paragraph_tokens = composer_tokens::clip(&tokens, &paragraph);
        let available = width.map(|width| (width - indent).max(px(1.)));
        let breaks = if document.jumbo_emojis {
            inline_emoji::wrap_jumbo_breaks(
                &text,
                row_font_size,
                &paragraph_runs,
                available,
                &paragraph_tokens,
                jumbo_size,
                window,
            )
        } else {
            inline_emoji::wrap_message_breaks(
                &text,
                row_font_size,
                &paragraph_runs,
                available,
                &paragraph_tokens,
                window,
            )
        };
        let marker = document.blocks.iter().find_map(|block| {
            if let MessageBlockKind::ListItem { marker } = &block.kind
                && marker.start == paragraph.start
                && marker.end <= paragraph.end
            {
                return Some(marker);
            }
            None
        });
        let hanging = marker
            .map(|marker| {
                inline_emoji::shape_message_line(
                    &document.text[marker.clone()],
                    row_font_size,
                    &clip(runs, marker),
                    &composer_tokens::clip(&tokens, marker),
                    window,
                )
                .width
            })
            .unwrap_or(px(0.));
        let first_break = breaks
            .iter()
            .copied()
            .find(|index| *index > 0 && *index < paragraph.len());
        let mut rows = Vec::new();
        if hanging > px(0.)
            && let Some(first_break) = first_break
        {
            let split = paragraph.start + first_break;
            let remainder = split..paragraph.end;
            let rest_text = &document.text[remainder.clone()];
            let rest_width = width.map(|width| (width - indent - hanging).max(px(1.)));
            let rest_breaks = inline_emoji::wrap_message_breaks(
                rest_text,
                row_font_size,
                &clip(runs, &remainder),
                rest_width,
                &composer_tokens::clip(&tokens, &remainder),
                window,
            );
            rows = hanging_rows(paragraph.clone(), split, &rest_breaks, indent, hanging);
        } else {
            rows.extend(
                visual_ranges(paragraph.clone(), &breaks)
                    .into_iter()
                    .map(|range| (range, indent)),
            );
        }
        for (range, row_indent) in rows {
            let text: SharedString = document.text[range.clone()].to_owned().into();
            let clipped_runs = clip(runs, &range);
            let clipped_tokens = composer_tokens::clip(&tokens, &range);
            let layout = if document.jumbo_emojis {
                inline_emoji::shape_jumbo_line(
                    &text,
                    row_font_size,
                    &clipped_runs,
                    &clipped_tokens,
                    jumbo_size,
                    window,
                )
            } else {
                inline_emoji::shape_message_line(
                    &text,
                    row_font_size,
                    &clipped_runs,
                    &clipped_tokens,
                    window,
                )
            };
            lines.push(VisualLine {
                range,
                layout,
                indent: row_indent,
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

pub(super) fn code_for_copy<'a>(
    document: &'a MessageDocument,
    range: &Range<usize>,
    revealed: &BTreeSet<u32>,
) -> Option<&'a str> {
    if !valid(document)
        || document.spans.iter().any(|span| {
            span.range.start < range.end
                && span.range.end > range.start
                && concealed(span, revealed)
        })
    {
        return None;
    }
    document.text.get(range.clone())
}

/// Shared geometry and selection state for the elements painted over a message.
pub(super) struct MessagePaintContext<'a> {
    pub document: &'a MessageDocument,
    pub revealed: &'a BTreeSet<u32>,
    pub layout: &'a DocumentLayout,
    pub bounds: Bounds<Pixels>,
    pub scroll: Point<Pixels>,
}
pub(super) struct EmojiImageOptions<'a> {
    pub cache: &'a Entity<EmojiCache>,
    pub reduced_motion: bool,
    pub hovered: bool,
}
pub(super) fn code_controls(
    context: MessagePaintContext<'_>,
    owner: Entity<TextInput>,
    copied: Option<&Range<usize>>,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let MessagePaintContext {
        document,
        revealed,
        layout,
        bounds,
        scroll,
    } = context;
    let mut controls = Vec::new();
    for (index, header) in layout.code_headers.iter().enumerate() {
        let Some(_) = code_for_copy(document, &header.range, revealed) else {
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
        let click_owner = owner.clone();
        let click_range = header.range.clone();
        let feedback = copied == Some(&header.range);
        let mut element = div()
            .id(SharedString::from(format!(
                "code-header-{}-{index}",
                owner.entity_id().as_u64()
            )))
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
                            // GPUI dispatches the click on key-up; block parent
                            // shortcuts here without writing the clipboard twice.
                            cx.stop_propagation();
                        }
                    })
                    .text_color(rgb(0xc8c2d4))
                    .hover(|style| style.text_color(rgb(0xffffff)))
                    .on_click(move |_, _, cx| {
                        click_owner
                            .update(cx, |input, cx| input.copy_code(click_range.clone(), cx));
                        cx.stop_propagation();
                    })
                    .child(if feedback { "✓ Copied" } else { "Copy" }),
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
                        quads.push(fill(
                            Bounds::new(point(bounds.left(), y), size(px(4.), line.height)),
                            rgb(alert_color(kind)),
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
/// Cache ownership follows one projected message. Retain only resources whose
/// actual image boxes intersect the current ancestor clip and viewport.
pub(super) struct EmojiCache {
    images: Entity<gpui::RetainAllImageCache>,
    visible: BTreeSet<String>,
}
impl EmojiCache {
    pub(super) fn new(cx: &mut App) -> Entity<Self> {
        let images = gpui::RetainAllImageCache::new(cx);
        cx.new(|_| Self {
            images,
            visible: BTreeSet::new(),
        })
    }
    fn retain(&mut self, visible: BTreeSet<String>, window: &mut Window, cx: &mut Context<Self>) {
        for source in self.visible.difference(&visible) {
            self.images.update(cx, |images, cx| {
                images.remove(&gpui::Resource::Uri(source.clone().into()), window, cx)
            });
        }
        self.visible = visible;
    }
}
pub(super) fn emoji_images(
    context: MessagePaintContext<'_>,
    options: EmojiImageOptions<'_>,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let MessagePaintContext {
        document,
        revealed,
        layout,
        bounds,
        scroll,
    } = context;
    let EmojiImageOptions {
        cache,
        reduced_motion,
        hovered,
    } = options;
    if !valid(document) {
        return Vec::new();
    }
    let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size())
        .intersect(&window.content_mask().bounds);
    let image_cache = cache.read(cx).images.clone();
    let mut retained = BTreeSet::new();
    let mut images = Vec::new();
    for span in &document.spans {
        let Some(emoji) = &span.custom_emoji else {
            continue;
        };
        if span.style.code || concealed(span, revealed) {
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
        let height = width;
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
        let animated = emoji.animated && (hovered || document.emoji_autoplay && !reduced_motion);
        let mut element = if let Some(url) = emoji.url.as_ref().and_then(|url| {
            crate::emoji_presentation::image_url(url, animated, document.jumbo_emojis)
        }) {
            retained.insert(url.clone());
            if emoji.animated
                && let Some(other) = emoji.url.as_ref().and_then(|raw| {
                    crate::emoji_presentation::image_url(raw, !animated, document.jumbo_emojis)
                })
            {
                retained.insert(other);
            }
            let cache = image_cache.clone();
            crate::visible_image::VisibleImage::new(
                crate::media_animation::image_id(
                    &format!("inline-emoji-{}", span.range.start),
                    &url,
                ),
                width.into(),
                height.into(),
                move || {
                    img(SharedString::from(url.clone()))
                        .image_cache(&cache)
                        .object_fit(ObjectFit::Contain)
                        .with_loading(emoji_fallback)
                        .with_fallback(emoji_fallback)
                },
            )
            .enabled(animated)
            .autoplay(true)
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
    cache.update(cx, |cache, cx| cache.retain(retained, window, cx));
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
    fn hanging_rows_preserve_unicode_byte_ranges_and_marker_indent() {
        let text = "• 雪 one two three";
        let split = text.find("two").unwrap();
        let end = text.find("three").unwrap();
        let rows = hanging_rows(0..text.len(), split, &[end - split], px(16.), px(12.));
        assert_eq!(
            rows.iter()
                .map(|(r, _)| &text[r.clone()])
                .collect::<String>(),
            text
        );
        assert_eq!(rows[0].1, px(16.));
        assert!(rows[1..].iter().all(|(_, indent)| *indent == px(28.)));
        assert!(
            rows.iter()
                .all(|(r, _)| text.is_char_boundary(r.start) && text.is_char_boundary(r.end))
        );
    }
    #[test]
    fn first_heading_ignores_only_the_compact_author_prefix() {
        let doc = document("**prior** text\n# heading");
        let heading = doc
            .blocks
            .iter()
            .find(|b| matches!(b.kind, MessageBlockKind::Heading { .. }))
            .unwrap();
        assert!(!heading_is_first(&doc, heading.range.start));
        let msg = Message::text(
            100.into(),
            10.into(),
            User {
                username: "É".into(),
                ..Default::default()
            },
            "# heading",
        );
        let compact = document(&msg.content).with_author(&msg);
        let heading = compact
            .blocks
            .iter()
            .find(|b| matches!(b.kind, MessageBlockKind::Heading { .. }))
            .unwrap();
        assert!(heading_is_first(&compact, heading.range.start));
    }

    #[test]
    fn alert_title_uses_its_rule_color() {
        let doc = document("> [!WARNING]\n> Check this");
        let alert = doc
            .blocks
            .iter()
            .find(|b| matches!(b.kind, MessageBlockKind::Alert { .. }))
            .unwrap();
        let styled = runs(&doc, &base(), &BTreeSet::new());
        let title = clip(
            &styled,
            &(alert.range.start..alert.range.start + "Warning".len()),
        );
        assert!(title.iter().all(|run| run.color
            == rgb(alert_color(fluxer_markdown_parser::ast::AlertType::Warning)).into()));
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
            code_for_copy(&doc, &block.range, &BTreeSet::new()),
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
    fn narrow_table_reserves_intrinsic_timestamp_width_in_its_own_column() {
        let doc = document(
            "first | date | code\n--- | --- | ---\né | updated <t:1700000000:F> local | `✨`",
        );
        let (rows, columns) = doc
            .blocks
            .iter()
            .find_map(|block| {
                if let MessageBlockKind::Table { rows, .. } = &block.kind {
                    Some((rows, rows.iter().map(Vec::len).max().unwrap()))
                } else {
                    None
                }
            })
            .unwrap();
        let timestamp = &doc.timestamps[0];
        let tokens = display_tokens(&doc, 0);
        let label = &tokens
            .iter()
            .find(|token| token.source == timestamp.range)
            .unwrap()
            .label;
        assert!(!label.contains("<t:"));
        assert_eq!(
            doc.copy_range(timestamp.range.clone(), &BTreeSet::new()),
            *label
        );
        // The real layout passes the shaped atomic label width here. A narrow
        // viewport must grow just its column and expose it via table scrolling.
        let measured = px(310.);
        let widths = table_column_widths(
            rows,
            columns,
            px(240.),
            &[(timestamp.range.clone(), measured)],
        );
        assert_eq!(
            widths,
            vec![px(80.), measured + px(TABLE_CELL_PADDING), px(80.)]
        );
        assert!(widths.iter().copied().fold(px(0.), |a, b| a + b) > px(240.));
        let timestamp_left = widths[0] + px(12.);
        let next_column = widths[0] + widths[1];
        assert_eq!(timestamp_left + measured + px(12.), next_column);
        assert!(inline_emoji_height(&doc, &rows[1][2], px(16.)).is_none());
        assert!(
            doc.copy_range(0..doc.text.len(), &BTreeSet::new())
                .contains('✨')
        );
    }
    #[test]
    fn timestamp_widget_limit_counts_visible_tokens_after_later_scroll_positions() {
        let source = (0..260)
            .map(|index| {
                let token = format!("<t:{}:R>", 1_700_000_000 + index);
                if index == 241 {
                    format!("||{token}||")
                } else {
                    token
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let doc = document(&source);
        assert_eq!(doc.timestamps.len(), 260);
        let rect = |timestamp: &crate::message_timestamp::Timestamp| {
            if hidden_at(&doc, &BTreeSet::new(), timestamp.range.start).is_some() {
                return None;
            }
            let row = (timestamp.seconds - 1_700_000_000) as f32;
            Some(Bounds::new(
                point(px(0.), px(row * 20.)),
                size(px(120.), px(20.)),
            ))
        };
        let later = Bounds::new(point(px(0.), px(240. * 20.)), size(px(200.), px(100.)));
        let visible: Vec<_> = visible_timestamps(&doc.timestamps, later, rect)
            .map(|(timestamp, _)| timestamp.seconds - 1_700_000_000)
            .collect();
        assert_eq!(visible, vec![240, 242, 243, 244]);
        let all = Bounds::new(point(px(0.), px(0.)), size(px(200.), px(10_000.)));
        assert_eq!(
            visible_timestamps(&doc.timestamps, all, rect).count(),
            MAX_VISIBLE_TIMESTAMPS
        );
        let offscreen = Bounds::new(point(px(0.), px(20_000.)), size(px(200.), px(100.)));
        assert_eq!(
            visible_timestamps(&doc.timestamps, offscreen, rect).count(),
            0
        );
        assert_eq!(doc.source, source);
    }
    #[test]
    fn code_unicode_remains_literal_while_table_and_body_keep_bundled_art_dimensions() {
        let source = "Inline `let s=\"✨\";` and ✨\n\n```rust\nlet s=\"✨\";\n```\n\nemoji | text\n--- | ---\n✨ | ordinary";
        let doc = document(source);
        let tokens = display_tokens(&doc, 0);
        let mut code_emoji = 0;
        let mut ordinary_emoji = 0;
        for (index, grapheme) in doc
            .text
            .grapheme_indices(true)
            .filter(|(_, g)| crate::chat_helpers::unicode_catalog_contains(g))
        {
            let range = index..index + grapheme.len();
            let code = doc
                .spans
                .iter()
                .find(|span| span.range.contains(&index))
                .unwrap()
                .style
                .code;
            if code {
                code_emoji += 1;
                assert!(!unicode_image_at(&doc, index));
                assert!(inline_emoji_height(&doc, &range, px(16.)).is_none());
                assert!(!tokens.iter().any(|token| token.source == range));
            } else {
                ordinary_emoji += 1;
                assert!(unicode_image_at(&doc, index));
                assert_eq!(inline_emoji_height(&doc, &range, px(16.)), Some(px(24.)));
                assert!(tokens.iter().any(|token| token.source == range));
            }
            assert_eq!(doc.copy_range(range, &BTreeSet::new()), grapheme);
        }
        assert_eq!(code_emoji, 2);
        assert_eq!(ordinary_emoji, 2);
        assert_eq!(doc.source, source);
        assert!(doc.text.contains("let s=\"✨\";"));
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

struct TimestampTooltip(String);
impl gpui::Render for TimestampTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .max_w(px(480.))
            .bg(rgb(0x15121c))
            .text_size(px(12.))
            .text_color(rgb(0xece9f6))
            .child(self.0.clone())
    }
}

#[derive(Default)]
struct TimestampClock(std::collections::HashSet<gpui::WindowId>);
impl gpui::Global for TimestampClock {}

/// One pending refresh per window, independent of timestamp/message count.
/// Only visible and unconcealed relative widgets re-arm this one-shot timer.
fn refresh_relative(window: &Window, cx: &mut App) {
    let handle = window.window_handle();
    let id = handle.window_id();
    if !cx.has_global::<TimestampClock>() {
        cx.set_global(TimestampClock::default());
    }
    if !cx.global_mut::<TimestampClock>().0.insert(id) {
        return;
    }
    cx.spawn(async move |cx| {
        gpui::Timer::after(std::time::Duration::from_secs(1)).await;
        let _ = cx.update(|cx| {
            cx.global_mut::<TimestampClock>().0.remove(&id);
            let _ = handle.update(cx, |_, window, _| window.refresh());
        });
    })
    .detach();
}

const MAX_VISIBLE_TIMESTAMPS: usize = 128;
fn visible_timestamps<'a>(
    timestamps: &'a [crate::message_timestamp::Timestamp],
    mask: Bounds<Pixels>,
    mut rect: impl FnMut(&crate::message_timestamp::Timestamp) -> Option<Bounds<Pixels>> + 'a,
) -> impl Iterator<Item = (&'a crate::message_timestamp::Timestamp, Bounds<Pixels>)> + 'a {
    timestamps
        .iter()
        .filter_map(move |timestamp| {
            let bounds = rect(timestamp)?;
            let visible = bounds.intersect(&mask);
            (visible.size.width > px(0.) && visible.size.height > px(0.))
                .then_some((timestamp, bounds))
        })
        .take(MAX_VISIBLE_TIMESTAMPS)
}
pub(super) fn timestamp_widgets(
    document: &MessageDocument,
    revealed: &BTreeSet<u32>,
    layout: &DocumentLayout,
    bounds: Bounds<Pixels>,
    scroll: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let mask = bounds
        .intersect(&window.content_mask().bounds)
        .intersect(&Bounds::new(point(px(0.), px(0.)), window.viewport_size()));
    let mut widgets = Vec::new();
    let visible = visible_timestamps(&document.timestamps, mask, |timestamp| {
        if hidden_at(document, revealed, timestamp.range.start).is_some() {
            return None;
        }
        let line = layout
            .lines
            .iter()
            .find(|line| line.range.contains(&timestamp.range.start))?;
        let x = line.indent
            + line
                .layout
                .x_for_index(timestamp.range.start - line.range.start);
        let end = line.indent
            + line
                .layout
                .x_for_index(timestamp.range.end.min(line.range.end) - line.range.start);
        let rect = Bounds::new(
            point(
                bounds.left() + x - scroll.x,
                bounds.top() + line.y - scroll.y,
            ),
            size((end - x).max(px(1.)), line.height),
        );
        Some(rect)
    });
    for (timestamp, rect) in visible {
        if timestamp.style == fluxer_markdown_parser::ast::TimestampStyle::RelativeTime {
            refresh_relative(window, cx);
        }
        let timestamp = timestamp.clone();
        let preferences = document.timestamp_preferences.clone();
        let mut widget = div()
            .id(SharedString::from(format!(
                "timestamp-{}",
                timestamp.range.start
            )))
            .w(rect.size.width)
            .h(rect.size.height)
            .tooltip(move |_, cx| {
                cx.new(|_| {
                    TimestampTooltip(crate::message_timestamp::tooltip(
                        &timestamp,
                        &preferences,
                        crate::message_timestamp::now(),
                    ))
                })
                .into()
            })
            .into_any_element();
        widget.layout_as_root(
            size(
                AvailableSpace::Definite(rect.size.width),
                AvailableSpace::Definite(rect.size.height),
            ),
            window,
            cx,
        );
        widget.prepaint_at(rect.origin, window, cx);
        widgets.push(widget);
    }
    widgets
}
