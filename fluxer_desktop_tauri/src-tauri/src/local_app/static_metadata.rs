// SPDX-License-Identifier: AGPL-3.0-or-later

use ::url::Url;

use crate::js::is_js_whitespace;

pub const STATIC_CDN_ENDPOINT_PLACEHOLDER: &str = "{{STATIC_CDN_ENDPOINT}}";

const REWRITTEN_FILE_NAMES: [&str; 2] = ["manifest.json", "browserconfig.xml"];
const WEB_ASSET_PATH_PREFIX: &str = "/web/";
const WEB_SEGMENT: &str = "/web/";

pub fn should_rewrite(file_name: &str) -> bool {
    REWRITTEN_FILE_NAMES.contains(&file_name)
}

/// Returns `None` for file names that are not rewritten static metadata.
pub fn rewrite(file_name: &str, content: &str) -> Option<String> {
    if !should_rewrite(file_name) {
        return None;
    }
    Some(rewrite_absolute_web_asset_urls(
        &content.replace(STATIC_CDN_ENDPOINT_PLACEHOLDER, ""),
    ))
}

/// Class `[^"'<>\s]`.
fn is_url_character(character: char) -> bool {
    !matches!(character, '"' | '\'' | '<' | '>') && !is_js_whitespace(character)
}

/// Class `[^"'<>\s)]`.
fn is_tail_character(character: char) -> bool {
    character != ')' && is_url_character(character)
}

/// End offset of a match of `https?://[^"'<>\s]+/web/[^"'<>\s)]+` anchored at `start`.
fn match_absolute_web_asset_url(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    let after_scheme = rest
        .strip_prefix("https://")
        .or_else(|| rest.strip_prefix("http://"))?;
    let authority_start = text.len() - after_scheme.len();
    let run_end = authority_start
        + after_scheme
            .char_indices()
            .find(|&(_, character)| !is_url_character(character))
            .map_or(after_scheme.len(), |(offset, _)| offset);
    // Greedy `+` backtracks from the longest run; the first end where `/web/` and one tail character follow wins.
    let mut candidate = run_end;
    while candidate > authority_start {
        if !text.is_char_boundary(candidate) {
            candidate -= 1;
            continue;
        }
        if let Some(tail) = text[candidate..].strip_prefix(WEB_SEGMENT)
            && tail.chars().next().is_some_and(is_tail_character)
        {
            let tail_len = tail
                .char_indices()
                .find(|&(_, character)| !is_tail_character(character))
                .map_or(tail.len(), |(offset, _)| offset);
            return Some(text.len() - tail.len() + tail_len);
        }
        candidate -= 1;
    }
    None
}

fn rewrite_absolute_web_asset_urls(content: &str) -> String {
    let mut output = String::with_capacity(content.len());
    let mut cursor = 0;
    let mut index = 0;
    while index < content.len() {
        if content.as_bytes()[index] == b'h'
            && let Some(end) = match_absolute_web_asset_url(content, index)
        {
            output.push_str(&content[cursor..index]);
            output.push_str(&rewrite_absolute_web_asset_url(&content[index..end]));
            cursor = end;
            index = end;
            continue;
        }
        index += content[index..].chars().next().map_or(1, char::len_utf8);
    }
    output.push_str(&content[cursor..]);
    output
}

fn rewrite_absolute_web_asset_url(source: &str) -> String {
    let Ok(parsed) = Url::parse(source) else {
        return source.to_owned();
    };
    if !parsed.path().starts_with(WEB_ASSET_PATH_PREFIX) {
        return source.to_owned();
    }
    let mut rewritten = parsed.path().to_owned();
    if let Some(query) = parsed.query().filter(|query| !query.is_empty()) {
        rewritten.push('?');
        rewritten.push_str(query);
    }
    if let Some(fragment) = parsed.fragment().filter(|fragment| !fragment.is_empty()) {
        rewritten.push('#');
        rewritten.push_str(fragment);
    }
    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_manifest_and_browserconfig_are_rewritten() {
        assert!(should_rewrite("manifest.json"));
        assert!(should_rewrite("browserconfig.xml"));
        assert!(!should_rewrite("index.html"));
        assert!(!should_rewrite("dir/manifest.json"));
        assert_eq!(rewrite("index.html", "x"), None);
    }

    #[test]
    fn removes_cdn_placeholder_everywhere() {
        assert_eq!(
            rewrite(
                "manifest.json",
                "{{STATIC_CDN_ENDPOINT}}/a {{STATIC_CDN_ENDPOINT}}/b"
            )
            .as_deref(),
            Some("/a /b")
        );
    }

    #[test]
    fn rewrites_absolute_web_urls_to_paths() {
        let input = r#"{"src":"https://fluxerstatic.com/web/icon.png?v=2#frag","x":"http://a.test/web/b.svg"}"#;
        assert_eq!(
            rewrite("manifest.json", input).as_deref(),
            Some(r#"{"src":"/web/icon.png?v=2#frag","x":"/web/b.svg"}"#)
        );
    }

    #[test]
    fn leaves_non_web_urls_alone() {
        let input = r#"{"a":"https://example.com/other/icon.png","b":"https://example.com/web/"}"#;
        assert_eq!(rewrite("manifest.json", input).as_deref(), Some(input));
    }

    #[test]
    fn greedy_match_uses_last_web_segment_and_stops_at_close_paren() {
        let input = "url(https://cdn.test/web/a/web/b.png) and https://x.test/web/c)d";
        assert_eq!(
            rewrite("browserconfig.xml", input).as_deref(),
            Some("url(/web/a/web/b.png) and /web/c)d")
        );
    }

    #[test]
    fn pathname_not_starting_with_web_keeps_original() {
        let input = "https://cdn.test/x/web/y.png";
        assert_eq!(rewrite("manifest.json", input).as_deref(), Some(input));
    }

    #[test]
    fn requires_scheme_and_non_empty_segments() {
        for input in [
            "ftp://a/web/b",
            "https:///web/b",
            "https://a/web/",
            "xhttps://a.test/web/b",
        ] {
            let expected = if input.starts_with("xhttps") {
                "x/web/b"
            } else {
                input
            };
            assert_eq!(
                rewrite("manifest.json", input).as_deref(),
                Some(expected),
                "{input}"
            );
        }
    }

    #[test]
    fn handles_multibyte_text() {
        assert_eq!(
            rewrite("manifest.json", "é https://a.test/web/ü.png ü").as_deref(),
            Some("é /web/%C3%BC.png ü")
        );
    }
}
