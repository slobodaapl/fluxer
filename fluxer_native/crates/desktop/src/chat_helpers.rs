// SPDX-License-Identifier: AGPL-3.0-or-later
use fluxer_core::{ClientState, Emoji, Message, Snowflake};
use serde::Deserialize;
use std::{collections::BTreeMap, sync::OnceLock};

#[derive(Clone)]
pub struct EmojiChoice {
    pub label: String,
    pub insert: String,
    pub reaction: String,
    pub id: Option<Snowflake>,
}
#[derive(Deserialize)]
struct UnicodeEmoji {
    names: Vec<String>,
    surrogates: String,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    skins: Vec<UnicodeEmoji>,
}
#[derive(Deserialize)]
struct EmojiData {
    categories: BTreeMap<String, Vec<UnicodeEmoji>>,
}
fn emoji_data() -> &'static EmojiData {
    static DATA: OnceLock<EmojiData> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../../fluxer_app/src/media/data/emojis.json"
        ))
        .expect("bundled emoji data")
    })
}
pub fn categories() -> Vec<&'static str> {
    let mut categories = vec!["all", "community"];
    categories.extend(emoji_data().categories.keys().map(String::as_str));
    categories
}
pub fn emoji_choices(
    state: &ClientState,
    channel: Option<Snowflake>,
    query: &str,
    category: &str,
) -> Vec<EmojiChoice> {
    let query = query.trim().trim_matches(':').to_lowercase();
    let mut choices = Vec::new();
    if matches!(category, "all" | "community") {
        let guild = channel
            .and_then(|channel| state.channels.get(&channel))
            .and_then(|channel| channel.guild_id)
            .and_then(|guild| state.guilds.get(&guild));
        if let Some(emojis) = guild
            .and_then(|guild| guild.extra.get("emojis"))
            .and_then(serde_json::Value::as_array)
        {
            for value in emojis {
                if value.get("available").and_then(serde_json::Value::as_bool) == Some(false) {
                    continue;
                }
                if let Ok(emoji) = serde_json::from_value::<Emoji>(value.clone())
                    && emoji.name.to_lowercase().contains(&query)
                    && let Some(id) = emoji.id
                {
                    choices.push(EmojiChoice {
                        label: format!(":{}:", emoji.name),
                        insert: format!(
                            "<{}:{}:{}>",
                            if emoji.animated { "a" } else { "" },
                            emoji.name,
                            id
                        ),
                        reaction: reaction_key(&emoji),
                        id: Some(id),
                    });
                }
            }
        }
    }
    for (name, emojis) in &emoji_data().categories {
        if category != "all" && category != name {
            continue;
        }
        for emoji in emojis {
            let base_matches = query.is_empty()
                || emoji.surrogates.contains(&query)
                || emoji
                    .names
                    .iter()
                    .chain(&emoji.keywords)
                    .any(|name| name.to_lowercase().contains(&query));
            for variant in std::iter::once(emoji).chain(&emoji.skins) {
                if base_matches
                    || variant.surrogates.contains(&query)
                    || variant
                        .names
                        .iter()
                        .any(|name| name.to_lowercase().contains(&query))
                {
                    choices.push(EmojiChoice {
                        label: variant.surrogates.clone(),
                        insert: variant.surrogates.clone(),
                        reaction: variant.surrogates.clone(),
                        id: None,
                    });
                }
            }
        }
    }
    choices
}
pub fn reaction_key(emoji: &Emoji) -> String {
    emoji
        .id
        .map_or_else(|| emoji.name.clone(), |id| format!("{}:{id}", emoji.name))
}
pub fn reference_summary(state: &ClientState, message: &Message) -> Option<String> {
    let reference = message.message_reference.as_ref()?;
    if reference.kind == 1 {
        return Some("↪ Forwarded message".into());
    }
    let source = reference.message_id.and_then(|id| {
        state
            .channel_messages(reference.channel_id)
            .find(|message| message.id == id)
    });
    let source = source.cloned().or_else(|| {
        message
            .extra
            .get("referenced_message")
            .and_then(|value| serde_json::from_value::<Message>(value.clone()).ok())
    });
    Some(source.map_or_else(
        || "↳ Original message unavailable".into(),
        |message| format!("↳ {}: {}", message.author.display_name(), preview(&message)),
    ))
}
pub fn preview(message: &Message) -> String {
    let text = display_text(message);
    if text.is_empty() {
        if !message.attachments.is_empty() {
            "[Attachment]".into()
        } else {
            "[Message]".into()
        }
    } else {
        text.lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(160)
            .collect()
    }
}

/// Forward snapshots are flat objects on the Fluxer wire, without an author
/// or message ID. Keep their text selectable even when the outer body is empty.
pub fn display_text(message: &Message) -> String {
    let mut parts = Vec::new();
    if !message.content.is_empty() {
        parts.push(message.content.as_str());
    }
    if let Some(snapshots) = message
        .extra
        .get("message_snapshots")
        .and_then(serde_json::Value::as_array)
    {
        parts.extend(
            snapshots
                .iter()
                .filter_map(|snapshot| snapshot.get("content").and_then(serde_json::Value::as_str))
                .filter(|text| !text.is_empty()),
        );
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_catalog_searches_aliases_keywords_and_categories() {
        let state = ClientState::default();
        assert!(
            emoji_choices(&state, None, "joy", "all")
                .iter()
                .any(|emoji| emoji.insert == "😂")
        );
        assert!(
            emoji_choices(&state, None, "banana", "food")
                .iter()
                .any(|emoji| emoji.insert == "🍌")
        );
        assert!(emoji_choices(&state, None, "banana", "community").is_empty());
        assert!(emoji_choices(&state, None, "zzzz_nonexistent", "all").is_empty());
    }
    #[test]
    fn skin_tones_are_searchable_by_base_keyword_and_variant_alias() {
        let state = ClientState::default();
        let thumbs = emoji_choices(&state, None, "thumbsup", "people");
        for variant in ["👍", "👍🏻", "👍🏼", "👍🏽", "👍🏾", "👍🏿"] {
            assert!(
                thumbs.iter().any(|emoji| emoji.insert == variant),
                "missing {variant}"
            );
        }
        assert!(
            emoji_choices(&state, None, "thumbsup_tone3", "all")
                .iter()
                .any(|emoji| emoji.insert == "👍🏽")
        );
        assert!(
            emoji_choices(&state, None, "👍🏽", "all")
                .iter()
                .any(|emoji| emoji.insert == "👍🏽")
        );
    }
    #[test]
    fn flat_forward_snapshot_text_preserves_multiline_content_and_ignores_missing_content() {
        let mut message = Message {
            content: "Optional comment".into(),
            ..Message::default()
        };
        message.extra.insert(
            "message_snapshots".into(),
            serde_json::json!([
                {"content":"Original\nSecond line\n👋🏽"},
                {"content":null,"attachments":null},
                {"attachments":[]},
            ]),
        );
        assert_eq!(
            display_text(&message),
            "Optional comment\n\nOriginal\nSecond line\n👋🏽"
        );
        assert_eq!(preview(&message), "Optional comment");
        message.content.clear();
        assert_eq!(display_text(&message), "Original\nSecond line\n👋🏽");
        assert_eq!(preview(&message), "Original");
    }
    #[test]
    fn custom_reactions_use_name_and_id() {
        let emoji = Emoji {
            id: Some(42.into()),
            name: "party".into(),
            animated: true,
        };
        assert_eq!(reaction_key(&emoji), "party:42");
        assert_eq!(reaction_key(&Emoji::unicode("👍")), "👍");
    }
}
