// SPDX-License-Identifier: AGPL-3.0-or-later
//! Composer labels are a visual projection of wire text, never a draft rewrite.
use std::{collections::BTreeMap, ops::Range};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub source: Range<usize>,
    pub label: String,
}
pub fn tokens(text: &str, labels: &BTreeMap<String, String>) -> Vec<Token> {
    if labels.is_empty() {
        return vec![];
    }
    let mut result = vec![];
    let mut index = 0;
    let mut code = 0;
    while index < text.len() {
        let ch = text[index..].chars().next().unwrap();
        if ch == '\\' {
            index += 1;
            if index < text.len() {
                index += text[index..].chars().next().unwrap().len_utf8();
            }
            continue;
        }
        if ch == '`' {
            let count = text[index..]
                .bytes()
                .take_while(|byte| *byte == b'`')
                .count();
            if code == 0 {
                code = count;
            } else if code == count {
                code = 0;
            }
            index += count;
            continue;
        }
        if ch == '<'
            && code == 0
            && let Some(end) = text[index..].find('>')
        {
            let end = index + end + 1;
            if let Some(label) = labels.get(&text[index..end]) {
                result.push(Token {
                    source: index..end,
                    label: label.clone(),
                });
                index = end;
                continue;
            }
        }
        index += ch.len_utf8();
    }
    result
}
pub fn clip(tokens: &[Token], range: &Range<usize>) -> Vec<Token> {
    tokens
        .iter()
        .filter(|token| token.source.start >= range.start && token.source.end <= range.end)
        .map(|token| Token {
            source: token.source.start - range.start..token.source.end - range.start,
            label: token.label.clone(),
        })
        .collect()
}
pub fn snap(tokens: &[Token], offset: usize) -> usize {
    tokens
        .iter()
        .find(|token| offset > token.source.start && offset < token.source.end)
        .map_or(offset, |token| {
            if offset - token.source.start < (token.source.len() / 2) {
                token.source.start
            } else {
                token.source.end
            }
        })
}
pub fn edit_range(tokens: &[Token], range: Range<usize>) -> Range<usize> {
    if range.is_empty() {
        let offset = snap(tokens, range.start);
        return offset..offset;
    }
    let mut result = range;
    for token in tokens {
        if token.source.start < result.end && token.source.end > result.start {
            result.start = result.start.min(token.source.start);
            result.end = result.end.max(token.source.end);
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn labels() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("<@103>".into(), "@雪 River".into()),
            ("<#11>".into(), "#general".into()),
        ])
    }
    #[test]
    fn wire_labels_preserve_unicode_offsets_and_skip_literal_code() {
        let text = "é <@103> <#11> ` <@103> ` ```\n<#11>\n``` \\<@103>";
        let spans = tokens(text, &labels());
        assert_eq!(spans.len(), 2);
        assert_eq!(&text[spans[0].source.clone()], "<@103>");
        assert_eq!(spans[0].label, "@雪 River");
        assert_eq!(clip(&spans, &(3..text.len()))[0].source, 0..6);
    }
    #[test]
    fn narrow_deletion_and_selection_expand_only_intersected_token() {
        let spans = tokens("a<@103>b<#11>c", &labels());
        assert_eq!(edit_range(&spans, 6..7), 1..7);
        assert_eq!(edit_range(&spans, 7..8), 7..8);
        assert_eq!(edit_range(&spans, 3..4), 1..7);
        assert_eq!(snap(&spans, 2), 1);
        assert_eq!(snap(&spans, 5), 7);
        assert_eq!(edit_range(&spans, 8..9), 8..13);
    }
}
