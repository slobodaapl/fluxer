// SPDX-License-Identifier: AGPL-3.0-or-later
//! Byte-indexed composer matching; the classic trigger grammar requires whitespace.
use std::ops::Range;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mention,
    Channel,
    Emoji,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub kind: Kind,
    pub range: Range<usize>,
    pub query: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub label: String,
    pub detail: String,
    pub glyph: String,
    pub insert: String,
    pub user: Option<fluxer_core::User>,
    pub media: Option<String>,
    pub image: Option<String>,
    pub status: Option<u32>,
}
pub fn trigger(text: &str, selection: Range<usize>, composing: bool) -> Option<Trigger> {
    if composing || !selection.is_empty() || !text.is_char_boundary(selection.end) {
        return None;
    }
    let prefix = text.get(..selection.end)?;
    let start = prefix
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let token = &prefix[start..];
    let (kind, query) = match token.as_bytes().first()? {
        b'@' => (Kind::Mention, &token[1..]),
        b'#' => (Kind::Channel, &token[1..]),
        b':' => (Kind::Emoji, &token[1..]),
        _ => return None,
    };
    // Never offer replacements inside inline/fenced code or unrevealed spoilers.
    let before = &prefix[..start];
    if before.matches("```").count() % 2 != 0
        || before.matches('`').count() % 2 != 0
        || before.matches("||").count() % 2 != 0
    {
        return None;
    }
    if kind == Kind::Emoji
        && (query.len() < 2
            || !query
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_+~-".contains(c)))
    {
        return None;
    }
    Some(Trigger {
        kind,
        range: start..selection.end,
        query: query.to_lowercase(),
    })
}
pub fn rank(label: &str, query: &str) -> Option<u8> {
    let label = label.to_lowercase();
    if label == query {
        Some(0)
    } else if label.starts_with(query) {
        Some(1)
    } else if label.contains(query) {
        Some(2)
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_caret_and_suffix() {
        let text = "雪 @él suffix";
        let t = trigger(text, 8..8, false).unwrap();
        assert_eq!(&text[t.range], "@él");
        assert_eq!(t.query, "él");
    }
    #[test]
    fn suppress_selection_ime_and_markup() {
        for text in ["\\@ab", "a@ab", "```\n@ab", "` @ab", "|| @ab", ":a", ":ab:"] {
            assert!(
                trigger(text, text.len()..text.len(), false).is_none(),
                "{text}"
            );
        }
        assert!(trigger("@a", 2..2, true).is_none());
        assert!(trigger("@a", 0..2, false).is_none());
    }
    #[test]
    fn boundaries_and_ranking() {
        assert!(trigger("hello\n#gen", 10..10, false).is_some());
        assert_eq!(rank("General", "gen"), Some(1));
        assert_eq!(rank("General", "era"), Some(2));
        assert_eq!(rank("General", "no"), None);
    }
}
