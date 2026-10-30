// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fmt::Write as _;

use super::static_metadata::STATIC_CDN_ENDPOINT_PLACEHOLDER;
use crate::js::is_js_whitespace;

const CSP_NONCE_PLACEHOLDER: &str = "{{CSP_NONCE_PLACEHOLDER}}";
const MEDIA_PRECONNECT_TAG: &str = r#"<link rel="preconnect" href="{{MEDIA_ENDPOINT}}">"#;
const CSP_NONCE_BYTES: usize = 16;
const THEME_CLASS_PREFIX: &str = "theme-";
const THEME_TOKEN_MAX_LENGTH: usize = 64;
const INSTANCE_PRODUCT_NAME_MAX_LENGTH: usize = 80;

pub struct IndexHtmlInput<'a> {
    pub html: &'a str,
    pub nonce: &'a str,
    pub preboot_theme: Option<&'a str>,
    pub product_name: Option<&'a str>,
}

pub fn random_csp_nonce() -> String {
    let mut bytes = [0_u8; CSP_NONCE_BYTES];
    getrandom::fill(&mut bytes).expect("operating system randomness is unavailable");
    let mut nonce = String::with_capacity(CSP_NONCE_BYTES * 2);
    for byte in bytes {
        write!(nonce, "{byte:02x}").expect("writing to a String cannot fail");
    }
    nonce
}

pub fn rewrite_index_html(input: &IndexHtmlInput) -> String {
    let substituted = input
        .html
        .replace(CSP_NONCE_PLACEHOLDER, input.nonce)
        .replace(STATIC_CDN_ENDPOINT_PLACEHOLDER, "")
        .replace(MEDIA_PRECONNECT_TAG, "");
    stamp_product_name(
        &stamp_preboot_theme(&substituted, input.preboot_theme),
        input.product_name,
    )
}

pub(crate) fn contains_control_character(value: &str) -> bool {
    value
        .chars()
        .any(|character| matches!(character as u32, 0..=0x1f | 0x7f..=0x9f))
}

pub(crate) fn read_instance_product_name(value: Option<&str>) -> Option<&str> {
    let trimmed = value?.trim_matches(is_js_whitespace);
    let length: usize = trimmed.chars().map(char::len_utf16).sum();
    if length == 0 || length > INSTANCE_PRODUCT_NAME_MAX_LENGTH {
        return None;
    }
    (!contains_control_character(trimmed)).then_some(trimmed)
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Byte offset of the first ASCII-case-insensitive occurrence of `needle` in `haystack[from..]`.
fn find_ignore_case(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let needle = needle.as_bytes();
    (from..bytes.len().checked_sub(needle.len())? + 1)
        .find(|&index| bytes[index..index + needle.len()].eq_ignore_ascii_case(needle))
}

fn starts_with_ignore_case(haystack: &str, index: usize, needle: &str) -> bool {
    haystack
        .as_bytes()
        .get(index..index + needle.len())
        .is_some_and(|slice| slice.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Offset past the whitespace run starting at `index`, or `index` when there is none.
fn skip_whitespace(text: &str, index: usize) -> usize {
    index
        + text[index..]
            .find(|character| !is_js_whitespace(character))
            .unwrap_or(text.len() - index)
}

/// Matches `(?=[\s>])` at `index`.
fn at_tag_boundary(text: &str, index: usize) -> bool {
    text[index..]
        .chars()
        .next()
        .is_some_and(|character| character == '>' || is_js_whitespace(character))
}

/// `<name(?=[\s>])[^>]*>` starting at `start` (which must hold `<name`): the offset one past the first `>`.
fn tag_open_end(text: &str, start: usize, name_length: usize) -> Option<usize> {
    let after_name = start + name_length;
    if !at_tag_boundary(text, after_name) {
        return None;
    }
    text[after_name..]
        .find('>')
        .map(|offset| after_name + offset + 1)
}

fn find_html_open_tag(html: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(start) = find_ignore_case(html, "<html", from) {
        if let Some(end) = tag_open_end(html, start, "<html".len()) {
            return Some((start, end));
        }
        from = start + 1;
    }
    None
}

fn has_class_attribute(tag: &str) -> bool {
    tag.char_indices().any(|(index, character)| {
        if !is_js_whitespace(character) {
            return false;
        }
        let after = index + character.len_utf8();
        starts_with_ignore_case(tag, after, "class")
            && tag[skip_whitespace(tag, after + "class".len())..].starts_with('=')
    })
}

fn is_safe_theme_token(theme: &str) -> bool {
    let mut characters = theme.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && theme.len() <= THEME_TOKEN_MAX_LENGTH
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

fn stamp_preboot_theme(html: &str, theme: Option<&str>) -> String {
    let Some(theme) = theme.filter(|theme| is_safe_theme_token(theme)) else {
        return html.to_owned();
    };
    let Some((start, end)) = find_html_open_tag(html) else {
        return html.to_owned();
    };
    let tag = &html[start..end];
    if has_class_attribute(tag) {
        return html.to_owned();
    }
    format!(
        "{}{} class=\"{THEME_CLASS_PREFIX}{theme}\">{}",
        &html[..start],
        &tag[..tag.len() - 1],
        &html[end..]
    )
}

/// `<title(?=[\s>])[^>]*>[^<]*<\/title>`: returns (offset one past the opening tag's `>`, end).
fn find_title_element(html: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(start) = find_ignore_case(html, "<title", from) {
        if let Some(open_end) = tag_open_end(html, start, "<title".len())
            && let Some(offset) = html[open_end..].find('<')
        {
            let close_start = open_end + offset;
            if starts_with_ignore_case(html, close_start, "</title>") {
                return Some((open_end, close_start + "</title>".len()));
            }
        }
        from = start + 1;
    }
    None
}

/// `<meta\s+name="apple-mobile-web-app-title"\s+content="[^"]*"`: returns (start, end).
fn find_product_name_meta(html: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(start) = find_ignore_case(html, "<meta", from) {
        from = start + 1;
        let mut index = start + "<meta".len();
        let mut next = skip_whitespace(html, index);
        if next == index {
            continue;
        }
        index = next;
        let name = r#"name="apple-mobile-web-app-title""#;
        if !starts_with_ignore_case(html, index, name) {
            continue;
        }
        index += name.len();
        next = skip_whitespace(html, index);
        if next == index {
            continue;
        }
        index = next;
        let content = r#"content=""#;
        if !starts_with_ignore_case(html, index, content) {
            continue;
        }
        index += content.len();
        if let Some(offset) = html[index..].find('"') {
            return Some((start, index + offset + 1));
        }
    }
    None
}

fn stamp_product_name(html: &str, product_name: Option<&str>) -> String {
    let Some(name) = read_instance_product_name(product_name) else {
        return html.to_owned();
    };
    let escaped = escape_html(name);
    let mut stamped = html.to_owned();
    if let Some((open_end, end)) = find_title_element(&stamped) {
        stamped.replace_range(open_end..end, &format!("{escaped}</title>"));
    }
    if let Some((start, end)) = find_product_name_meta(&stamped) {
        stamped.replace_range(
            start..end,
            &format!(r#"<meta name="apple-mobile-web-app-title" content="{escaped}""#),
        );
    }
    stamped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(html: &str, theme: Option<&str>, product_name: Option<&str>) -> String {
        rewrite_index_html(&IndexHtmlInput {
            html,
            nonce: "abc123",
            preboot_theme: theme,
            product_name,
        })
    }

    #[test]
    fn nonce_is_32_lowercase_hex_and_unique() {
        let first = random_csp_nonce();
        assert_eq!(first.len(), 32);
        assert!(
            first
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_ne!(first, random_csp_nonce());
    }

    #[test]
    fn substitutes_placeholders_everywhere() {
        let html = r#"<script nonce="{{CSP_NONCE_PLACEHOLDER}}"></script><script nonce="{{CSP_NONCE_PLACEHOLDER}}"></script><link href="{{STATIC_CDN_ENDPOINT}}/a.css"><link rel="preconnect" href="{{MEDIA_ENDPOINT}}">"#;
        assert_eq!(
            rewrite(html, None, None),
            r#"<script nonce="abc123"></script><script nonce="abc123"></script><link href="/a.css">"#
        );
    }

    #[test]
    fn stamps_theme_class_on_first_html_tag() {
        assert_eq!(
            rewrite(
                "<!doctype html><HTML lang=\"en\"><body></body></html>",
                Some("dark"),
                None
            ),
            "<!doctype html><HTML lang=\"en\" class=\"theme-dark\"><body></body></html>"
        );
        assert_eq!(
            rewrite("<html>x", Some("Dark_1-x"), None),
            "<html class=\"theme-Dark_1-x\">x"
        );
        assert_eq!(
            rewrite("<html\n>x", Some("dark"), None),
            "<html\n class=\"theme-dark\">x"
        );
    }

    #[test]
    fn theme_is_skipped_when_unsafe_or_class_present() {
        for theme in ["", "-dark", "da rk", "da\"rk", "dark>", &"a".repeat(65)] {
            assert_eq!(rewrite("<html>", Some(theme), None), "<html>", "{theme:?}");
        }
        assert_eq!(
            rewrite("<html>", Some(&"a".repeat(64)), None),
            format!("<html class=\"theme-{}\">", "a".repeat(64))
        );
        assert_eq!(
            rewrite("<html class=\"x\">", Some("dark"), None),
            "<html class=\"x\">"
        );
        assert_eq!(
            rewrite("<html data-a=1 CLASS = \"x\">", Some("dark"), None),
            "<html data-a=1 CLASS = \"x\">"
        );
        assert_eq!(
            rewrite("<htmlx><html>", Some("dark"), None),
            "<htmlx><html class=\"theme-dark\">"
        );
        assert_eq!(
            rewrite("<head></head>", Some("dark"), None),
            "<head></head>"
        );
        assert_eq!(rewrite("<html", Some("dark"), None), "<html");
        assert_eq!(
            rewrite("<html data-classy=1>", Some("dark"), None),
            "<html data-classy=1 class=\"theme-dark\">"
        );
    }

    #[test]
    fn stamps_title_and_meta_with_escaped_name() {
        let html = r#"<head><title data-x="1">Old</title><meta name="apple-mobile-web-app-title" content="Old"><meta name="x" content="y"></head>"#;
        assert_eq!(
            rewrite(html, None, Some("  A&B <\"it's\">  ")),
            r#"<head><title data-x="1">A&amp;B &lt;&quot;it&#39;s&quot;&gt;</title><meta name="apple-mobile-web-app-title" content="A&amp;B &lt;&quot;it&#39;s&quot;&gt;"><meta name="x" content="y"></head>"#
        );
    }

    #[test]
    fn title_and_meta_only_first_match_and_case_insensitive() {
        let html = "<TITLE>a</TITLE><title>b</title><META  NAME=\"apple-mobile-web-app-title\"\n CONTENT=\"o\"><meta name=\"apple-mobile-web-app-title\" content=\"p\">";
        assert_eq!(
            rewrite(html, None, Some("N")),
            "<TITLE>N</title><title>b</title><meta name=\"apple-mobile-web-app-title\" content=\"N\"><meta name=\"apple-mobile-web-app-title\" content=\"p\">"
        );
    }

    #[test]
    fn title_with_nested_markup_is_not_matched() {
        let html = "<title>a<b></title><title>c</title>";
        assert_eq!(
            rewrite(html, None, Some("N")),
            "<title>a<b></title><title>N</title>"
        );
        assert_eq!(
            rewrite("<titlex>a</title>", None, Some("N")),
            "<titlex>a</title>"
        );
    }

    #[test]
    fn invalid_product_names_leave_html_untouched() {
        let html = "<title>Old</title>";
        let too_long = "x".repeat(81);
        for name in [
            None,
            Some(""),
            Some("   "),
            Some("a\nb"),
            Some("a\u{7f}b"),
            Some("a\u{85}b"),
            Some(too_long.as_str()),
        ] {
            assert_eq!(rewrite(html, None, name), html, "{name:?}");
        }
        assert_eq!(
            rewrite(html, None, Some(&"x".repeat(80))),
            format!("<title>{}</title>", "x".repeat(80))
        );
    }

    #[test]
    fn product_name_length_counts_utf16_units() {
        // 40 astral characters = 80 UTF-16 units (allowed); 41 = 82 (rejected).
        let ok = "\u{1F600}".repeat(40);
        let bad = "\u{1F600}".repeat(41);
        assert_eq!(read_instance_product_name(Some(&ok)), Some(ok.as_str()));
        assert_eq!(read_instance_product_name(Some(&bad)), None);
    }

    #[test]
    fn trim_uses_js_whitespace() {
        assert_eq!(
            read_instance_product_name(Some("\u{feff}\u{a0}Name\u{3000}")),
            Some("Name")
        );
        // U+0085 is a control character and not JS whitespace, so it survives trim and is rejected.
        assert_eq!(read_instance_product_name(Some("\u{85}Name")), None);
    }

    #[test]
    fn dollar_sequences_in_name_stay_literal() {
        assert_eq!(
            rewrite(
                r#"<meta name="apple-mobile-web-app-title" content="o">"#,
                None,
                Some("a$`b$&")
            ),
            r#"<meta name="apple-mobile-web-app-title" content="a$`b$&amp;">"#
        );
    }
}
