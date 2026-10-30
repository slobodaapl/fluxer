// SPDX-License-Identifier: AGPL-3.0-or-later

//! ECMAScript string semantics the ported TypeScript relied on.

/// ECMAScript `WhiteSpace` + `LineTerminator`, the set `String.prototype.trim` strips. Differs
/// from `char::is_whitespace`: U+FEFF is included, U+0085 is excluded.
pub(crate) fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `value.trim()`.
pub(crate) fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

/// `value.length` of a JavaScript string.
pub(crate) fn utf16_length(value: &str) -> usize {
    value.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_whitespace_set() {
        assert!(is_js_whitespace('\u{feff}'));
        assert!(is_js_whitespace('\u{a0}'));
        assert!(!is_js_whitespace('\u{85}'));
        assert!(!is_js_whitespace('\u{200b}'));
    }

    #[test]
    fn trim_strips_feff_and_ideographic_space_but_keeps_nel() {
        assert_eq!(js_trim("\u{feff}\u{3000} a b \u{3000}\u{feff}"), "a b");
        assert_eq!(js_trim("\u{85}a\u{85}"), "\u{85}a\u{85}");
    }

    #[test]
    fn utf16_length_counts_code_units() {
        assert_eq!(utf16_length("a"), 1);
        assert_eq!(utf16_length("\u{1f600}"), 2);
    }
}
