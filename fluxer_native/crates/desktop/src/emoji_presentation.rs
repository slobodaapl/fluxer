// SPDX-License-Identifier: AGPL-3.0-or-later
//! Direct top-level AST rule from JumboDetector.ts. Formatting, spoilers,
//! blocks, and reply/profile contexts deliberately do not recurse into jumbo.
use fluxer_markdown_parser::Node;
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;

pub fn jumbo(nodes: &[Node], standard: bool, settings: &Value) -> bool {
    if !standard || fluxer_core::compact_messages(settings) {
        return false;
    }
    let mut count = 0;
    for node in nodes {
        match node {
            Node::Emoji { .. } => count += 1,
            Node::Text { content } => {
                // Native parser contexts contain shared shortcode records; the
                // web EmojiProvider supplies per-message Unicode match ranges.
                // Count the same catalog graphemes when they remain Text here.
                for grapheme in content.graphemes(true) {
                    if grapheme.chars().all(char::is_whitespace) {
                        continue;
                    }
                    if !crate::chat_helpers::unicode_catalog_contains(grapheme) {
                        return false;
                    }
                    count += 1;
                    if count > 30 {
                        return false;
                    }
                }
            }
            _ => return false,
        }
        if count > 30 {
            return false;
        }
    }
    count > 0
}

pub fn autoplay(settings: &Value, reduced_motion: bool) -> bool {
    let animate = settings
        .get("animate_emoji")
        .or_else(|| settings.get("animateEmoji"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    animate && !reduced_motion
}

pub fn image_url(raw: &str, animated: bool, jumbo: bool) -> Option<String> {
    let mut url = reqwest::Url::parse(&crate::image_gallery::safe_image_url(raw)?).ok()?;
    let pairs: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "animated" && key != "size")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs)
        .append_pair("animated", if animated { "true" } else { "false" })
        .append_pair("size", if jumbo { "240" } else { "128" });
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxer_markdown_parser::{MarkdownParser, ParserFlags};
    #[test]
    fn source_top_level_count_and_context_are_respected() {
        let mut parser = MarkdownParser::new(ParserFlags::ALL, Default::default());
        let mut parse = |s: &str| parser.parse(s).unwrap();
        // Unicode catalog context is installed in message_markup; custom nodes
        // exercise the same AST detector without a separate emoji recognizer.
        assert!(jumbo(&parse("<:x:123> \n <:x:124>"), true, &Value::Null));
        assert!(!jumbo(&parse("**<:x:123>**"), true, &Value::Null));
        assert!(!jumbo(&parse("||<:x:123>||"), true, &Value::Null));
        assert!(!jumbo(&parse("<:x:123>"), false, &Value::Null));
        assert!(!jumbo(&parse(&"<:x:123> ".repeat(31)), true, &Value::Null));
        assert!(jumbo(&parse(&"<:x:123> ".repeat(30)), true, &Value::Null));
    }
    #[test]
    fn emoji_policy_is_independent_of_gif_autoplay_and_preserves_signed_url_fields() {
        assert!(autoplay(&serde_json::json!({"gif_auto_play":false}), false));
        assert!(!autoplay(
            &serde_json::json!({"animate_emoji":false}),
            false
        ));
        assert!(!autoplay(&Value::Null, true));
        assert_eq!(
            image_url(
                "https://host/emojis/1.webp?sig=a%2Fb&animated=true&size=128",
                false,
                true
            )
            .unwrap(),
            "https://host/emojis/1.webp?sig=a%2Fb&animated=false&size=240"
        );
    }
}
