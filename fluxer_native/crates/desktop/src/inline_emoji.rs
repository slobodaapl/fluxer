// SPDX-License-Identifier: AGPL-3.0-or-later
//! Emoji occupy one em in layout. The backing string stays untouched: all
//! selection, IME, clipboard and editing offsets remain source UTF-8 bytes.
use super::*;

#[derive(Clone, Debug)]
struct Replacement {
    source: Range<usize>,
    display: Range<usize>,
}
#[derive(Clone, Debug)]
struct Projection {
    text: SharedString,
    runs: Vec<TextRun>,
    replacements: Vec<Replacement>,
}
impl Projection {
    fn new(text: &str, runs: &[TextRun]) -> Self {
        let mut display = String::new();
        let mut display_runs = Vec::new();
        let mut replacements = Vec::new();
        let mut copied = 0;
        for (start, grapheme) in text.grapheme_indices(true) {
            if !crate::chat_helpers::unicode_catalog_contains(grapheme) {
                continue;
            }
            display.push_str(&text[copied..start]);
            display_runs.extend(message_text::clip(runs, &(copied..start)));
            let source = start..start + grapheme.len();
            let clipped = message_text::clip(runs, &source);
            copied = source.end;
            let from = display.len();
            display.push('\u{2003}');
            replacements.push(Replacement {
                source,
                display: from..display.len(),
            });
            if let Some(mut run) = clipped.into_iter().next() {
                run.len = '\u{2003}'.len_utf8();
                run.color = rgba(0x00000000).into();
                run.underline = None;
                run.strikethrough = None;
                display_runs.push(run);
            }
        }
        display.push_str(&text[copied..]);
        display_runs.extend(message_text::clip(runs, &(copied..text.len())));
        Self {
            text: display.into(),
            runs: display_runs,
            replacements,
        }
    }
    fn source_index(&self, index: usize) -> usize {
        let mut source_end = 0;
        let mut display_end = 0;
        for replacement in &self.replacements {
            if index < replacement.display.start {
                break;
            }
            if index < replacement.display.end {
                return replacement.source.start;
            }
            source_end = replacement.source.end;
            display_end = replacement.display.end;
        }
        source_end + index.saturating_sub(display_end)
    }
    fn display_index(&self, index: usize) -> usize {
        let mut source_end = 0;
        let mut display_end = 0;
        for replacement in &self.replacements {
            if index < replacement.source.start {
                break;
            }
            if index < replacement.source.end {
                return replacement.display.start;
            }
            source_end = replacement.source.end;
            display_end = replacement.display.end;
        }
        display_end + index.saturating_sub(source_end)
    }
}

#[derive(Clone)]
pub(super) struct EmojiLine {
    shaped: ShapedLine,
    projection: Projection,
}
impl std::ops::Deref for EmojiLine {
    type Target = ShapedLine;
    fn deref(&self) -> &ShapedLine {
        &self.shaped
    }
}
impl EmojiLine {
    pub fn x_for_index(&self, index: usize) -> Pixels {
        self.shaped
            .x_for_index(self.projection.display_index(index))
    }
    pub fn closest_index_for_x(&self, x: Pixels) -> usize {
        self.projection
            .source_index(self.shaped.closest_index_for_x(x))
    }
    pub fn index_for_x(&self, x: Pixels) -> Option<usize> {
        self.shaped
            .index_for_x(x)
            .map(|index| self.projection.source_index(index))
    }
}

pub(super) fn shape_line(
    text: &str,
    font_size: Pixels,
    runs: &[TextRun],
    window: &mut Window,
) -> EmojiLine {
    let projection = Projection::new(text, runs);
    let shaped =
        window
            .text_system()
            .shape_line(projection.text.clone(), font_size, &projection.runs, None);
    EmojiLine { shaped, projection }
}

pub(super) fn wrap_breaks(
    text: &str,
    font_size: Pixels,
    runs: &[TextRun],
    width: Option<Pixels>,
    window: &mut Window,
) -> Vec<usize> {
    let projection = Projection::new(text, runs);
    window
        .text_system()
        .shape_text(
            projection.text.clone(),
            font_size,
            &projection.runs,
            width,
            None,
        )
        .expect("measure emoji text")
        .first()
        .map(|line| {
            line.wrap_boundaries
                .iter()
                .map(|boundary| {
                    projection
                        .source_index(line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn images(
    text: &str,
    document: Option<&MessageDocument>,
    revealed: &BTreeSet<u32>,
    layout: &DocumentLayout,
    viewport: (Bounds<Pixels>, Point<Pixels>),
    window: &mut Window,
    cx: &mut App,
) -> Vec<gpui::AnyElement> {
    use gpui::{AvailableSpace, ObjectFit, img};
    let (bounds, scroll) = viewport;
    let mut images = Vec::new();
    for (row, line) in layout.lines.iter().enumerate() {
        let y = bounds.top() + layout.line_height * row - scroll.y;
        if y + layout.line_height < bounds.top() || y > bounds.bottom() {
            continue;
        }
        for (local, grapheme) in text[line.range.clone()].grapheme_indices(true) {
            if !crate::chat_helpers::unicode_catalog_contains(grapheme) {
                continue;
            }
            let index = line.range.start + local;
            if document.is_some_and(|doc| message_text::hidden_at(doc, revealed, index).is_some()) {
                continue;
            }
            let Some(source) =
                crate::emoji_sprites::image_source(grapheme, window.scale_factor() > 1., cx)
            else {
                continue;
            };
            let x = line.indent + line.layout.x_for_index(local);
            let width = (line.layout.x_for_index(local + grapheme.len())
                - line.layout.x_for_index(local))
            .max(px(1.));
            let height = (layout.line_height - px(2.)).max(px(1.));
            let origin = point(bounds.left() + x - scroll.x, y + px(1.));
            if !Bounds::new(origin, size(width, height)).intersects(&bounds) {
                continue;
            }
            let mut image = img(source)
                .w(width)
                .h(height)
                .object_fit(ObjectFit::Contain)
                .into_any_element();
            image.layout_as_root(
                size(
                    AvailableSpace::Definite(width),
                    AvailableSpace::Definite(height),
                ),
                window,
                cx,
            );
            image.prepaint_at(origin, window, cx);
            images.push(image);
        }
    }
    images
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_keeps_graphemes_atomic_and_round_trips_source_boundaries() {
        let text = "a☺️👨‍👩‍👧‍👦👍🏽🇺🇸🫱🏻‍🫲🏼z";
        let base = TextRun {
            len: text.len(),
            font: gpui::font("sans-serif"),
            color: rgb(0xffffff).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let projection = Projection::new(text, &[base]);
        assert_eq!(
            projection.text.as_ref(),
            "a\u{2003}\u{2003}\u{2003}\u{2003}\u{2003}z"
        );
        for index in text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .chain([text.len()])
        {
            assert_eq!(
                projection.source_index(projection.display_index(index)),
                index
            );
        }
        assert_eq!(
            projection.runs.iter().map(|run| run.len).sum::<usize>(),
            projection.text.len()
        );
        assert_eq!(
            projection
                .runs
                .iter()
                .filter(|run| run.color.a == 0.)
                .count(),
            5
        );
    }
    #[test]
    fn unknown_emoji_sequence_is_not_partially_replaced() {
        let text = "👨\u{200d}🦕";
        let projection = Projection::new(text, &[]);
        assert_eq!(projection.text.as_ref(), text);
        assert!(projection.replacements.is_empty());
    }
}
