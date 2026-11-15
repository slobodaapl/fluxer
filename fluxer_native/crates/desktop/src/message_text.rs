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
                MessageTarget::Link(_) | MessageTarget::Channel(_) | MessageTarget::Author { .. }
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
    for block in &document.blocks {
        if block.range.start <= paragraph.start && block.range.end > paragraph.start {
            match block.kind {
                MessageBlockKind::Quote { depth } => quote = quote.max(depth.min(4)),
                MessageBlockKind::Code { .. } => code = true,
                _ => {}
            }
        }
    }
    px(quote as f32 * 10. + if code { 8. } else { 0. })
}
pub(super) fn layout(
    document: &MessageDocument,
    runs: &[TextRun],
    font_size: Pixels,
    line_height: Pixels,
    width: Option<Pixels>,
    window: &mut Window,
) -> DocumentLayout {
    let mut lines = Vec::new();
    for paragraph in logical_lines(&document.text) {
        let indent = paragraph_indent(document, &paragraph);
        let text: SharedString = document.text[paragraph.clone()].to_owned().into();
        let paragraph_runs = clip(runs, &paragraph);
        let breaks = inline_emoji::wrap_breaks(
            &text,
            font_size,
            &paragraph_runs,
            width.map(|width| (width - indent).max(px(1.))),
            window,
        );
        for range in visual_ranges(paragraph, &breaks) {
            let text: SharedString = document.text[range.clone()].to_owned().into();
            let layout = inline_emoji::shape_line(&text, font_size, &clip(runs, &range), window);
            lines.push(VisualLine {
                range,
                layout,
                indent,
            });
        }
    }
    DocumentLayout { lines, line_height }
}
pub(super) fn backgrounds(
    document: &MessageDocument,
    layout: &DocumentLayout,
    bounds: Bounds<Pixels>,
    scroll: Point<Pixels>,
) -> Vec<PaintQuad> {
    let mut quads = Vec::new();
    for (index, line) in layout.lines.iter().enumerate() {
        let y = bounds.top() + layout.line_height * index - scroll.y;
        if y + layout.line_height < bounds.top() || y > bounds.bottom() {
            continue;
        }
        for block in &document.blocks {
            if block.range.start <= line.range.start && block.range.end > line.range.start {
                match block.kind {
                    MessageBlockKind::Code { .. } => quads.push(fill(
                        Bounds::new(
                            point(bounds.left(), y),
                            size(bounds.size.width, layout.line_height),
                        ),
                        rgb(0x25222b),
                    )),
                    MessageBlockKind::Quote { depth } => {
                        for level in 0..depth.min(4) {
                            quads.push(fill(
                                Bounds::new(
                                    point(bounds.left() + px(level as f32 * 10.), y),
                                    size(px(2.), layout.line_height),
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
        let height = (layout.line_height - px(2.)).max(px(1.));
        let origin = point(
            bounds.left() + x - scroll.x,
            bounds.top() + layout.line_height * row - scroll.y + px(1.),
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
        assert!(target_at(&doc, &BTreeSet::new(), 0).is_none());
        doc.spans[0].target = Some(MessageTarget::Role(34.into()));
        assert!(target_at(&doc, &BTreeSet::new(), 0).is_none());
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
