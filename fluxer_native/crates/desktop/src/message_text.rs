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
        .filter(|target| matches!(target, MessageTarget::Link(_) | MessageTarget::Channel(_)))
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
                run.font.family = "monospace".into();
                run.background_color = Some(rgb(0x25222b).into());
            }
            if span.style.subtext || span.style.quote_depth > 0 {
                run.color = rgb(0xb1acbd).into();
            }
            if let Some(target) = &span.target {
                run.color = rgb(match target {
                    MessageTarget::Link(_) => 0x83bdf5,
                    _ => 0xb5a7ef,
                })
                .into();
                if !matches!(target, MessageTarget::Link(_)) {
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
        let wrapped = window
            .text_system()
            .shape_text(
                text,
                font_size,
                &paragraph_runs,
                width.map(|width| (width - indent).max(px(1.))),
                None,
            )
            .expect("measure rich message paragraph");
        let breaks = wrapped
            .first()
            .map(|line| {
                line.wrap_boundaries
                    .iter()
                    .map(|boundary| line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for range in visual_ranges(paragraph, &breaks) {
            let text: SharedString = document.text[range.clone()].to_owned().into();
            let layout =
                window
                    .text_system()
                    .shape_line(text, font_size, &clip(runs, &range), None);
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
        let start = layout.position_for_index(span.range.start);
        let end = layout.position_for_index(span.range.end);
        let width = (end.x - start.x).max(px(1.));
        let height = (layout.line_height - px(2.)).max(px(1.));
        let origin = point(
            bounds.left() + start.x - scroll.x,
            bounds.top() + start.y - scroll.y + px(1.),
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
