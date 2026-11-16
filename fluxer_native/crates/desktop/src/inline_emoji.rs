// SPDX-License-Identifier: AGPL-3.0-or-later
//! Emoji occupy one em in layout. The backing string stays untouched: all
//! selection, IME, clipboard and editing offsets remain source UTF-8 bytes.
use super::*;

#[derive(Clone, Debug)]
struct Replacement {
    source: Range<usize>,
    display: Range<usize>,
    named: bool,
}
#[derive(Clone, Debug)]
struct Projection {
    text: SharedString,
    runs: Vec<TextRun>,
    replacements: Vec<Replacement>,
}
impl Projection {
    #[cfg(test)]
    fn new(text: &str, runs: &[TextRun]) -> Self {
        Self::with_tokens(text, runs, &[])
    }
    #[cfg(test)]
    fn with_tokens(text: &str, runs: &[TextRun], tokens: &[composer_tokens::Token]) -> Self {
        Self::with_tokens_mode(text, runs, tokens, true)
    }
    fn with_tokens_mode(
        text: &str,
        runs: &[TextRun],
        tokens: &[composer_tokens::Token],
        style_tokens: bool,
    ) -> Self {
        let mut display = String::new();
        let mut display_runs = Vec::new();
        let mut replacements = Vec::new();
        let mut copied = 0;
        let mut spans: Vec<_> = tokens
            .iter()
            .map(|token| (token.source.clone(), token.label.clone(), true))
            .collect();
        spans.extend(
            text.grapheme_indices(true)
                .filter(|(start, grapheme)| {
                    style_tokens
                        && crate::chat_helpers::unicode_catalog_contains(grapheme)
                        && !tokens.iter().any(|token| token.source.contains(start))
                })
                .map(|(start, grapheme)| (start..start + grapheme.len(), "\u{2003}".into(), false)),
        );
        spans.sort_by_key(|(range, _, _)| range.start);
        for (source, label, named) in spans {
            display.push_str(&text[copied..source.start]);
            display_runs.extend(message_text::clip(runs, &(copied..source.start)));
            let clipped = message_text::clip(runs, &source);
            copied = source.end;
            let from = display.len();
            display.push_str(&label);
            replacements.push(Replacement {
                source,
                display: from..display.len(),
                named,
            });
            if let Some(mut run) = clipped.into_iter().next() {
                run.len = label.len();
                let timestamp = named
                    && !style_tokens
                    && text[replacements.last().unwrap().source.clone()].starts_with("<t:");
                if !timestamp {
                    run.underline = None;
                    run.strikethrough = None;
                }
                if named && style_tokens {
                    run.color = rgb(0xc8b7ff).into();
                    run.background_color = Some(rgba(0x7560a844).into());
                } else if !named {
                    run.color = rgba(0x00000000).into();
                }
                if named
                    && !style_tokens
                    && text[replacements.last().unwrap().source.clone()].starts_with("<t:")
                    && run.color.a > 0.
                {
                    run.background_color = Some(rgba(0x8a849b30).into());
                }
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
    /// Wrap measured graphemes, grouping every replacement label as one source
    /// unit. Unlike collapsing already-computed text wraps, this reflows the
    /// suffix after a label moved to the next row. Advances come from the same
    /// shaped runs used for painting, including bold/mono and CJK glyphs.
    fn atomic_wrap_breaks(
        &self,
        width: Pixels,
        mut x_for_index: impl FnMut(usize) -> Pixels,
    ) -> Vec<usize> {
        struct Unit {
            source: usize,
            left: Pixels,
            right: Pixels,
            ch: char,
            atomic: bool,
        }
        let mut units = Vec::new();
        let mut cursor = 0;
        let mut replacements = self.replacements.iter().peekable();
        while cursor < self.text.len() {
            let atomic = replacements
                .peek()
                .is_some_and(|replacement| replacement.display.start == cursor);
            let end = if atomic {
                replacements.next().unwrap().display.end
            } else {
                cursor + self.text[cursor..].graphemes(true).next().unwrap().len()
            };
            units.push(Unit {
                source: self.source_index(cursor),
                left: x_for_index(cursor),
                right: x_for_index(end),
                ch: self.text[cursor..].chars().next().unwrap(),
                atomic,
            });
            cursor = end;
        }
        // Same word/CJK candidate policy as GPUI's LineLayout wrapper; atomic
        // widgets additionally permit a break on either side of the widget.
        let mut breaks = Vec::new();
        let mut first_non_space = false;
        let mut last_candidate = None;
        let mut last_boundary = 0;
        let mut last_x = px(0.);
        let mut previous = '\0';
        let mut previous_atomic = false;
        for (index, unit) in units.iter().enumerate() {
            let space = unit.ch == ' ' && !unit.atomic;
            if first_non_space
                && !space
                && (unit.atomic || previous_atomic || !wrap_word_char(unit.ch) || previous == ' ')
            {
                last_candidate = Some(index);
            }
            if !space {
                first_non_space = true;
            }
            if unit.right - last_x > width && index > last_boundary {
                let boundary = last_candidate
                    .take()
                    .filter(|candidate| *candidate > last_boundary)
                    .unwrap_or(index);
                breaks.push(units[boundary].source);
                last_boundary = boundary;
                last_x = units[boundary].left;
            }
            previous = unit.ch;
            previous_atomic = unit.atomic;
        }
        breaks
    }
    fn source_index(&self, index: usize) -> usize {
        let mut source_end = 0;
        let mut display_end = 0;
        for replacement in &self.replacements {
            if index < replacement.display.start {
                break;
            }
            if index < replacement.display.end {
                return if replacement.named
                    && index - replacement.display.start >= replacement.display.len() / 2
                {
                    replacement.source.end
                } else {
                    replacement.source.start
                };
            }
            source_end = replacement.source.end;
            display_end = replacement.display.end;
        }
        source_end + index.saturating_sub(display_end)
    }
    fn source_break(&self, index: usize) -> usize {
        self.replacements
            .iter()
            .find(|replacement| {
                replacement.named
                    && index > replacement.display.start
                    && index < replacement.display.end
            })
            .map_or_else(
                || self.source_index(index),
                |replacement| replacement.source.start,
            )
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

fn wrap_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(ch,'\u{00c0}'..='\u{024f}'|'\u{0400}'..='\u{04ff}'|'-'|'_'|'.'|'\''|'$'|'%'|'@'|'#'|'^'|'~'|','|'='|':'|'⋯')
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
    shape_line_with_tokens(text, font_size, runs, &[], window)
}
pub(super) fn shape_line_with_tokens(
    text: &str,
    font_size: Pixels,
    runs: &[TextRun],
    tokens: &[composer_tokens::Token],
    window: &mut Window,
) -> EmojiLine {
    shape_with_mode(text, font_size, runs, tokens, true, window)
}
pub(super) fn shape_message_line(
    text: &str,
    font_size: Pixels,
    runs: &[TextRun],
    tokens: &[composer_tokens::Token],
    window: &mut Window,
) -> EmojiLine {
    shape_with_mode(text, font_size, runs, tokens, false, window)
}
fn shape_with_mode(
    text: &str,
    font_size: Pixels,
    runs: &[TextRun],
    tokens: &[composer_tokens::Token],
    styled: bool,
    window: &mut Window,
) -> EmojiLine {
    let projection = Projection::with_tokens_mode(text, runs, tokens, styled);
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
    wrap_breaks_with_tokens(text, font_size, runs, width, &[], window)
}
pub(super) fn wrap_breaks_with_tokens(
    text: &str,
    font_size: Pixels,
    runs: &[TextRun],
    width: Option<Pixels>,
    tokens: &[composer_tokens::Token],
    window: &mut Window,
) -> Vec<usize> {
    wrap_with_mode(text, font_size, runs, width, tokens, true, window)
}
pub(super) fn wrap_message_breaks(
    text: &str,
    font_size: Pixels,
    runs: &[TextRun],
    width: Option<Pixels>,
    tokens: &[composer_tokens::Token],
    window: &mut Window,
) -> Vec<usize> {
    wrap_with_mode(text, font_size, runs, width, tokens, false, window)
}
fn wrap_with_mode(
    text: &str,
    font_size: Pixels,
    runs: &[TextRun],
    width: Option<Pixels>,
    tokens: &[composer_tokens::Token],
    styled: bool,
    window: &mut Window,
) -> Vec<usize> {
    let projection = Projection::with_tokens_mode(text, runs, tokens, styled);
    if !styled && !projection.replacements.is_empty() {
        let Some(width) = width else {
            return vec![];
        };
        let shaped = window.text_system().shape_line(
            projection.text.clone(),
            font_size,
            &projection.runs,
            None,
        );
        return projection.atomic_wrap_breaks(width, |index| shaped.x_for_index(index));
    }
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
                        .source_break(line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
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
    for line in &layout.lines {
        let y = bounds.top() + line.y - scroll.y;
        if y + line.height < bounds.top() || y > bounds.bottom() {
            continue;
        }
        for (local, grapheme) in text[line.range.clone()].grapheme_indices(true) {
            if !crate::chat_helpers::unicode_catalog_contains(grapheme) {
                continue;
            }
            let index = line.range.start + local;
            if document.is_some_and(|doc| {
                !message_text::unicode_image_at(doc, index)
                    || message_text::hidden_at(doc, revealed, index).is_some()
            }) {
                continue;
            }
            let x = line.indent + line.layout.x_for_index(local);
            let width = (line.layout.x_for_index(local + grapheme.len())
                - line.layout.x_for_index(local))
            .max(px(1.));
            // Pick the source sheet for the rendered pixel footprint, so
            // 48px jumbo emoji use 64px art even on a 1x display.
            let Some(source) = crate::emoji_sprites::image_source(
                grapheme,
                width * window.scale_factor() > px(32.),
                cx,
            ) else {
                continue;
            };
            let height = width;
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

/// Jumbo is an image width, never the surrounding text's font size. Reserve
/// fixed 3rem boxes while keeping inter-emoji whitespace at the account font.
pub(super) fn shape_jumbo_line(
    text: &str,
    font_size: Pixels,
    runs: &[TextRun],
    tokens: &[composer_tokens::Token],
    emoji_size: Pixels,
    window: &mut Window,
) -> EmojiLine {
    let projection = Projection::with_tokens_mode(text, runs, tokens, false);
    let mut shaped =
        window
            .text_system()
            .shape_line(projection.text.clone(), font_size, &projection.runs, None);
    let shifts: Vec<_> = projection
        .replacements
        .iter()
        .map(|replacement| {
            let width = shaped.x_for_index(replacement.display.end)
                - shaped.x_for_index(replacement.display.start);
            (replacement.display.end, emoji_size - width)
        })
        .collect();
    let mut glyph_runs = shaped.runs.clone();
    for run in &mut glyph_runs {
        for glyph in &mut run.glyphs {
            for (end, shift) in &shifts {
                if glyph.index >= *end {
                    glyph.position.x += *shift;
                }
            }
        }
    }
    let layout = gpui::LineLayout {
        font_size: shaped.font_size,
        width: shifts
            .iter()
            .fold(shaped.width, |width, (_, shift)| width + *shift),
        ascent: shaped.ascent,
        descent: shaped.descent,
        runs: glyph_runs,
        len: shaped.len(),
    };
    *std::ops::DerefMut::deref_mut(&mut shaped) = Arc::new(layout);
    EmojiLine { shaped, projection }
}
pub(super) fn wrap_jumbo_breaks(
    text: &str,
    font_size: Pixels,
    runs: &[TextRun],
    width: Option<Pixels>,
    tokens: &[composer_tokens::Token],
    emoji_size: Pixels,
    window: &mut Window,
) -> Vec<usize> {
    let Some(width) = width else {
        return vec![];
    };
    let Some(run) = runs.first() else {
        return vec![];
    };
    let projection = Projection::with_tokens_mode(text, runs, tokens, false);
    let mut fragments = Vec::new();
    let mut cursor = 0;
    for replacement in &projection.replacements {
        if cursor < replacement.display.start {
            fragments.push(gpui::LineFragment::text(
                &projection.text[cursor..replacement.display.start],
            ));
        }
        fragments.push(gpui::LineFragment::element(
            emoji_size,
            replacement.display.len(),
        ));
        cursor = replacement.display.end;
    }
    if cursor < projection.text.len() {
        fragments.push(gpui::LineFragment::text(&projection.text[cursor..]));
    }
    window
        .text_system()
        .line_wrapper(run.font.clone(), font_size)
        .wrap_line(&fragments, width)
        .map(|boundary| projection.source_break(boundary.ix))
        .collect()
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
    fn timestamp_labels_are_atomic_utf8_safe_and_preserve_concealed_run_style() {
        let text = "é <t:1700000000:D> 雪";
        let start = text.find("<t:").unwrap();
        let end = text.find('>').unwrap() + 1;
        let token = composer_tokens::Token {
            source: start..end,
            label: "14 novembre 2023".into(),
        };
        let base = TextRun {
            len: text.len(),
            font: gpui::font("sans-serif"),
            color: rgba(0x00000000).into(),
            background_color: Some(rgb(0x34313b).into()),
            underline: None,
            strikethrough: None,
        };
        let projection =
            Projection::with_tokens_mode(text, std::slice::from_ref(&base), &[token], false);
        assert_eq!(projection.text.as_ref(), "é 14 novembre 2023 雪");
        assert_eq!(projection.source_break(start + 3), start);
        assert_eq!(projection.source_index(projection.display_index(end)), end);
        assert_eq!(projection.source_index(projection.text.len()), text.len());
        assert!(
            projection
                .runs
                .iter()
                .all(|run| run.color.a == 0. && run.background_color == base.background_color)
        );
    }
    #[test]
    fn measured_atomic_wrap_reflows_prefix_and_suffix_without_splitting_labels() {
        let text = "éx <t:1700000000:F> local 中文e\u{301}";
        let start = text.find("<t:").unwrap();
        let end = text.find('>').unwrap() + 1;
        let projection = Projection::with_tokens_mode(
            text,
            &[],
            &[composer_tokens::Token {
                source: start..end,
                label: "Tuesday, November 14, 2023 10:13 PM".into(),
            }],
            false,
        );
        // Simulate unequal shaped glyph advances, including CJK and a combining
        // grapheme. Production obtains these positions from the styled ShapedLine.
        let mut positions = std::collections::BTreeMap::new();
        let mut advance = px(0.);
        for (index, grapheme) in projection.text.grapheme_indices(true) {
            positions.insert(index, advance);
            advance += px(match grapheme {
                "中" | "文" => 13.,
                "é" => 8.,
                "e\u{301}" => 7.,
                _ => 6.,
            });
        }
        positions.insert(projection.text.len(), advance);
        let replacement = &projection.replacements[0];
        let width = positions[&replacement.display.end] - positions[&replacement.display.start];
        let breaks = projection.atomic_wrap_breaks(width, |index| positions[&index]);
        assert_eq!(breaks, vec![start, end]);
        let rows = visual_ranges(0..text.len(), &breaks);
        assert_eq!(&text[rows[0].clone()], "éx ");
        assert_eq!(&text[rows[1].clone()], "<t:1700000000:F>");
        assert_eq!(&text[rows[2].clone()], " local 中文e\u{301}");
        assert!(breaks.iter().all(|index| text.is_char_boundary(*index)));
        assert!(breaks.iter().all(|index| !(*index > start && *index < end)));
        for row in rows {
            let from = projection.display_index(row.start);
            let to = projection.display_index(row.end);
            assert!(positions[&to] - positions[&from] <= width);
        }
    }
    #[test]
    fn measured_atomic_wrap_keeps_cjk_and_combining_source_boundaries() {
        let text = "中文e\u{301}";
        let projection = Projection::with_tokens_mode(text, &[], &[], false);
        let positions = std::collections::BTreeMap::from([
            (0, px(0.)),
            (3, px(13.)),
            (6, px(26.)),
            (text.len(), px(33.)),
        ]);
        let breaks = projection.atomic_wrap_breaks(px(15.), |index| positions[&index]);
        assert_eq!(breaks, vec![3, 6]);
        assert_eq!(&text[breaks[1]..], "e\u{301}");
        assert_eq!(
            projection.source_index(projection.display_index(text.len())),
            text.len()
        );
    }
    #[test]
    fn message_projection_does_not_reintroduce_literal_code_emoji() {
        let text = "let s=\"✨\";";
        let message = Projection::with_tokens_mode(text, &[], &[], false);
        assert_eq!(message.text.as_ref(), text);
        assert!(message.replacements.is_empty());
        let composer = Projection::with_tokens_mode(text, &[], &[], true);
        assert_eq!(composer.replacements.len(), 1);
        assert_ne!(composer.text.as_ref(), text);
        let start = text.find('✨').unwrap();
        let tokens = [composer_tokens::Token {
            source: start..start + "✨".len(),
            label: "\u{2003}\u{2002}".into(),
        }];
        let explicit = Projection::with_tokens_mode(text, &[], &tokens, false);
        assert_eq!(explicit.replacements.len(), 1);
        assert_eq!(explicit.source_index(explicit.display_index(start)), start);
        assert_eq!(explicit.source_index(explicit.text.len()), text.len());
    }
    #[test]
    fn unknown_emoji_sequence_is_not_partially_replaced() {
        let text = "👨\u{200d}🦕";
        let projection = Projection::new(text, &[]);
        assert_eq!(projection.text.as_ref(), text);
        assert!(projection.replacements.is_empty());
    }
    #[test]
    fn named_projection_keeps_wire_offsets_atomic_beside_unicode_art() {
        let text = "é <@103> 🙂 <#11>";
        let labels = std::collections::BTreeMap::from([
            ("<@103>".into(), "@River".into()),
            ("<#11>".into(), "#general".into()),
        ]);
        let tokens = composer_tokens::tokens(text, &labels);
        let projection = Projection::with_tokens(text, &[], &tokens);
        assert_eq!(projection.text.as_ref(), "é @River \u{2003} #general");
        for token in &tokens {
            let start = projection.display_index(token.source.start);
            let end = projection.display_index(token.source.end);
            assert_eq!(projection.source_index(start), token.source.start);
            assert_eq!(projection.source_index(end), token.source.end);
            assert_eq!(projection.source_break(start + 1), token.source.start);
        }
        assert_eq!(projection.source_index(projection.text.len()), text.len());
        assert_eq!(text, "é <@103> 🙂 <#11>");
    }
}
