// SPDX-License-Identifier: AGPL-3.0-or-later
use fluxer_core::{ClientState, Emoji, Message, Snowflake};
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

/// Immutable choices are shared by catalog searches. The picker caches these
/// handles; rendering or keyboard navigation never rebuilds the catalog.
#[derive(Clone, Debug)]
pub struct EmojiChoice {
    pub key: String,
    /// Primary shortcode without colons, including a tone suffix for variants.
    pub name: String,
    pub category: String,
    pub label: String,
    pub insert: String,
    pub reaction: String,
    pub id: Option<Snowflake>,
    pub animated: bool,
    pub aliases: Arc<[String]>,
    pub keywords: Arc<[String]>,
    /// Bundled artwork only. Mixed-tone extras have no tile in the six sheets.
    pub sprite: Option<EmojiSprite>,
}

/// UnicodeEmojis.ts sprite indices, independent of the visible picker order.
/// Sheet 0 is spritesheet-emoji; sheets 1–5 are 1f3fb through 1f3ff.
/// Cells are 32 pixels in 1x assets, 64 pixels in @2x assets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EmojiSprite {
    pub sheet: u8,
    pub index: u32,
}
impl EmojiSprite {
    pub fn columns(self) -> u32 {
        if self.sheet == 0 { 42 } else { 10 }
    }
    pub fn rows(self) -> u32 {
        let count = if self.sheet == 0 {
            unicode_index().base_sprite_count
        } else {
            unicode_index().skin_sprite_count
        };
        count.div_ceil(self.columns())
    }
}

/// Match the existing FE0F-insensitive Unicode lookup. No images are decoded.
pub fn unicode_sprite(surrogate: &str) -> Option<EmojiSprite> {
    unicode_index()
        .sprites
        .get(&surrogate.replace('\u{fe0f}', ""))
        .copied()
}
#[derive(Deserialize)]
struct UnicodeEmoji {
    names: Vec<String>,
    surrogates: String,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    skins: Vec<UnicodeEmoji>,
    #[serde(default, rename = "hasSkinTones")]
    has_skin_tones: bool,
    #[serde(skip)]
    sprite: Option<EmojiSprite>,
    #[serde(skip)]
    skin_sprite_index: Option<u32>,
}
#[derive(Deserialize)]
struct EmojiData {
    #[serde(deserialize_with = "ordered_categories")]
    categories: Vec<(String, Vec<UnicodeEmoji>)>,
}
fn ordered_categories<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<(String, Vec<UnicodeEmoji>)>, D::Error> {
    struct Categories;
    impl<'de> Visitor<'de> for Categories {
        type Value = Vec<(String, Vec<UnicodeEmoji>)>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("emoji categories in source JSON order")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut entries = Vec::new();
            while let Some(entry) = map.next_entry()? {
                entries.push(entry);
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_map(Categories)
}
const CATEGORY_ORDER: [&str; 8] = [
    "people", "nature", "food", "activity", "travel", "objects", "symbols", "flags",
];
const TONES: [&str; 6] = ["", "🏻", "🏼", "🏽", "🏾", "🏿"];

struct IndexedChoice {
    choice: Arc<EmojiChoice>,
    terms: Vec<String>,
    tone: Option<char>,
}
impl IndexedChoice {
    fn new(choice: EmojiChoice) -> Self {
        let terms = choice
            .aliases
            .iter()
            .chain(choice.keywords.iter())
            .map(|term| term.to_lowercase())
            .chain(std::iter::once(choice.insert.clone()))
            .collect();
        Self::with_terms(choice, terms)
    }
    fn with_terms(choice: EmojiChoice, terms: Vec<String>) -> Self {
        let mut tones = choice
            .insert
            .chars()
            .filter(|c| ('\u{1f3fb}'..='\u{1f3ff}').contains(c));
        let tone = tones
            .next()
            .filter(|first| tones.all(|tone| tone == *first));
        Self {
            choice: Arc::new(choice),
            terms,
            tone,
        }
    }
    fn matches(&self, query: &str) -> bool {
        self.terms.iter().any(|term| term.contains(query))
    }
}
struct UnicodeGroup {
    category: String,
    base: IndexedChoice,
    variants: Vec<IndexedChoice>,
}
impl UnicodeGroup {
    fn preferred(&self, tone: usize) -> &IndexedChoice {
        TONES[tone]
            .chars()
            .next()
            .and_then(|tone| {
                self.variants
                    .iter()
                    .find(|variant| variant.tone == Some(tone))
            })
            .unwrap_or(&self.base)
    }
}
type ChoiceList = Arc<[Arc<EmojiChoice>]>;
struct UnicodeIndex {
    categories: Vec<String>,
    groups: Vec<UnicodeGroup>,
    /// Six precomputed base grids per category, including the complete grid.
    /// Empty search never scans aliases or all 2,030 skin variants.
    grids: BTreeMap<String, [ChoiceList; 6]>,
    sprites: BTreeMap<String, EmojiSprite>,
    base_sprite_count: u32,
    skin_sprite_count: u32,
}
fn unicode_choice(
    category: &str,
    emoji: &UnicodeEmoji,
    base: &UnicodeEmoji,
    keywords: Arc<[String]>,
    sprite: Option<EmojiSprite>,
) -> EmojiChoice {
    let name = emoji
        .names
        .first()
        .cloned()
        .unwrap_or_else(|| emoji.surrogates.clone());
    let mut aliases = emoji.names.clone();
    for alias in &base.names {
        if !aliases.contains(alias) {
            aliases.push(alias.clone());
        }
    }
    EmojiChoice {
        key: format!("unicode:{category}:{name}"),
        name,
        category: category.into(),
        label: emoji.surrogates.clone(),
        insert: emoji.surrogates.clone(),
        reaction: emoji.surrogates.clone(),
        id: None,
        animated: false,
        aliases: aliases.into(),
        keywords,
        sprite,
    }
}
fn unicode_index() -> &'static UnicodeIndex {
    static INDEX: OnceLock<UnicodeIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut data: EmojiData = serde_json::from_str(include_str!(
            "../../../../fluxer_app/src/media/data/emojis.json"
        ))
        .expect("bundled emoji data");
        // UnicodeEmojis.buildEmojiIndex counts in JSON object insertion order.
        // Assign positions before applying the picker's category sorting.
        let (mut base_sprite_count, mut skin_sprite_count) = (0, 0);
        for (_, emojis) in &mut data.categories {
            for emoji in emojis {
                emoji.sprite = Some(EmojiSprite {
                    sheet: 0,
                    index: base_sprite_count,
                });
                base_sprite_count += 1;
                if emoji.has_skin_tones || !emoji.skins.is_empty() {
                    emoji.skin_sprite_index = Some(skin_sprite_count);
                    skin_sprite_count += 1;
                }
            }
        }
        let mut source_categories: BTreeMap<_, _> = data.categories.into_iter().collect();
        let mut ordered = Vec::new();
        for category in CATEGORY_ORDER {
            if let Some(emojis) = source_categories.remove(category) {
                ordered.push((category.to_owned(), emojis));
            }
        }
        // Unknown future categories stay visible, after the source's known order.
        ordered.extend(source_categories);
        let categories = ordered
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let mut groups = Vec::new();
        for (category, emojis) in ordered {
            for emoji in emojis {
                let keywords: Arc<[String]> = emoji.keywords.clone().into();
                let base = IndexedChoice::new(unicode_choice(
                    &category,
                    &emoji,
                    &emoji,
                    keywords.clone(),
                    emoji.sprite,
                ));
                let variants = emoji
                    .skins
                    .iter()
                    .enumerate()
                    .map(|(tone, variant)| {
                        let terms = variant
                            .names
                            .iter()
                            .map(|name| name.to_lowercase())
                            .chain(std::iter::once(variant.surrogates.clone()))
                            .collect();
                        IndexedChoice::with_terms(
                            unicode_choice(
                                &category,
                                variant,
                                &emoji,
                                keywords.clone(),
                                emoji.skin_sprite_index.filter(|_| tone < 5).map(|index| {
                                    EmojiSprite {
                                        sheet: tone as u8 + 1,
                                        index,
                                    }
                                }),
                            ),
                            terms,
                        )
                    })
                    .collect();
                groups.push(UnicodeGroup {
                    category: category.clone(),
                    base,
                    variants,
                });
            }
        }
        let mut grids: BTreeMap<String, [Vec<Arc<EmojiChoice>>; 6]> = BTreeMap::new();
        let mut sprites = BTreeMap::new();
        grids.insert("all".into(), std::array::from_fn(|_| Vec::new()));
        for category in &categories {
            grids.insert(category.clone(), std::array::from_fn(|_| Vec::new()));
        }
        for group in &groups {
            for entry in std::iter::once(&group.base).chain(&group.variants) {
                if let Some(sprite) = entry.choice.sprite {
                    sprites.insert(entry.choice.insert.replace('\u{fe0f}', ""), sprite);
                }
            }
            for tone in 0..6 {
                let choice = group.preferred(tone).choice.clone();
                grids.get_mut("all").expect("all category")[tone].push(choice.clone());
                grids
                    .get_mut(&group.base.choice.category)
                    .expect("emoji category")[tone]
                    .push(choice);
            }
        }
        UnicodeIndex {
            categories,
            groups,
            sprites,
            base_sprite_count,
            skin_sprite_count,
            grids: grids
                .into_iter()
                .map(|(category, lists)| (category, lists.map(Arc::from)))
                .collect(),
        }
    })
}

pub fn categories() -> Vec<&'static str> {
    let mut result = vec!["all", "community"];
    result.extend(unicode_index().categories.iter().map(String::as_str));
    result
}

/// One picker snapshot. Rebuild when its channel/guild emoji data changes;
/// query/category/tone changes reuse both its custom entries and Unicode index.
/// Rendering should consume the UI's cached search results instead of searching.
pub struct EmojiCatalog {
    unicode: &'static UnicodeIndex,
    custom: Vec<IndexedChoice>,
}
impl EmojiCatalog {
    pub fn new(state: &ClientState, channel: Option<Snowflake>) -> Self {
        let mut custom = Vec::new();
        let guild = channel
            .and_then(|id| state.channels.get(&id))
            .and_then(|channel| channel.guild_id)
            .and_then(|id| state.guilds.get(&id));
        if let Some(emojis) = guild
            .and_then(|guild| guild.extra.get("emojis"))
            .and_then(serde_json::Value::as_array)
        {
            for value in emojis {
                if value.get("available").and_then(serde_json::Value::as_bool) == Some(false) {
                    continue;
                }
                if let Ok(emoji) = serde_json::from_value::<Emoji>(value.clone())
                    && let Some(id) = emoji.id
                {
                    custom.push(IndexedChoice::new(EmojiChoice {
                        key: format!("custom:{id}"),
                        name: emoji.name.clone(),
                        category: "community".into(),
                        label: format!(":{}:", emoji.name),
                        insert: format!(
                            "<{}:{}:{id}>",
                            if emoji.animated { "a" } else { "" },
                            emoji.name
                        ),
                        reaction: reaction_key(&emoji),
                        id: Some(id),
                        animated: emoji.animated,
                        aliases: vec![emoji.name].into(),
                        keywords: Arc::from([]),
                        sprite: None,
                    }));
                }
            }
        }
        Self {
            unicode: unicode_index(),
            custom,
        }
    }
    pub fn search(&self, query: &str, category: &str) -> Vec<Arc<EmojiChoice>> {
        self.search_with_tone(query, category, None)
    }
    /// Empty queries return one base/preferred-tone choice per Unicode group.
    /// Unqualified searches retain all matching variants. With a preferred tone,
    /// base-keyword matches use that tone; explicit variant aliases/Unicode still
    /// return the exact searched variant, including mixed-tone sequences.
    pub fn search_with_tone(
        &self,
        query: &str,
        category: &str,
        tone: Option<&str>,
    ) -> Vec<Arc<EmojiChoice>> {
        let query = query.trim().trim_matches(':').to_lowercase();
        let tone = tone
            .and_then(|tone| TONES.iter().position(|candidate| *candidate == tone))
            .unwrap_or(0);
        let mut choices = Vec::new();
        if matches!(category, "all" | "community") {
            choices.extend(
                self.custom
                    .iter()
                    .filter(|entry| query.is_empty() || entry.matches(&query))
                    .map(|entry| entry.choice.clone()),
            );
        }
        if query.is_empty() {
            if let Some(grids) = self.unicode.grids.get(category) {
                choices.extend(grids[tone].iter().cloned());
            }
            return choices;
        }
        for group in &self.unicode.groups {
            if category != "all" && category != group.category {
                continue;
            }
            let base_matches = group.base.matches(&query);
            if base_matches && tone != 0 {
                choices.push(group.preferred(tone).choice.clone());
            } else {
                if base_matches {
                    choices.push(group.base.choice.clone());
                }
                choices.extend(
                    group
                        .variants
                        .iter()
                        .filter(|variant| base_matches || variant.matches(&query))
                        .map(|variant| variant.choice.clone()),
                );
            }
        }
        choices
    }
}

/// Transitional convenience API. Interactive pickers should keep an
/// EmojiCatalog and cache Arc results rather than cloning fields each render.
#[allow(dead_code)]
pub fn emoji_choices(
    state: &ClientState,
    channel: Option<Snowflake>,
    query: &str,
    category: &str,
) -> Vec<EmojiChoice> {
    EmojiCatalog::new(state, channel)
        .search(query, category)
        .into_iter()
        .map(|choice| (*choice).clone())
        .collect()
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
    #[ignore = "diagnostic timing of the actual bundled catalog; run explicitly"]
    fn catalog_hot_path_timing() {
        let state = ClientState::default();
        let start = std::time::Instant::now();
        let catalog = EmojiCatalog::new(&state, None);
        let empty = catalog.search("", "all");
        println!(
            "catalog cold empty: {} choices in {:?}",
            empty.len(),
            start.elapsed()
        );
        for query in [
            "",
            "thumbsup",
            "thumbsup_tone3",
            "👍🏽",
            "banana",
            "zzzz_nonexistent",
        ] {
            let start = std::time::Instant::now();
            let mut count = 0;
            for _ in 0..100 {
                let result = catalog.search(query, "all");
                count = result.len();
                std::hint::black_box(result);
            }
            println!(
                "catalog query {query:?}: {count} choices, {:?}/query",
                start.elapsed() / 100
            );
        }
    }
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
    fn default_grid_has_faces_first_and_one_preferred_tone_per_base() {
        assert_eq!(
            categories(),
            [
                "all",
                "community",
                "people",
                "nature",
                "food",
                "activity",
                "travel",
                "objects",
                "symbols",
                "flags"
            ]
        );
        let catalog = EmojiCatalog::new(&ClientState::default(), None);
        let base = catalog.search("", "all");
        assert_eq!(base.first().unwrap().insert, "😀");
        assert_eq!(base.first().unwrap().name, "grinning");
        assert_eq!(base.first().unwrap().category, "people");
        assert!(!base.iter().any(|choice| choice.insert == "👍🏽"));
        assert_eq!(
            base.iter()
                .filter(|choice| choice.aliases.iter().any(|name| name == "thumbsup"))
                .count(),
            1
        );
        let toned = catalog.search_with_tone("", "all", Some("🏽"));
        assert_eq!(toned.len(), base.len());
        let thumb = toned
            .iter()
            .find(|choice| choice.name == "thumbsup_tone3")
            .unwrap();
        assert_eq!(thumb.insert, "👍🏽");
        assert_eq!(thumb.reaction, "👍🏽");
        assert!(thumb.aliases.iter().any(|alias| alias == "+1"));
        assert!(thumb.keywords.iter().any(|keyword| keyword == "good"));
        assert_eq!(
            toned.first().unwrap().insert,
            "😀",
            "no-tone emoji falls back to base"
        );
    }

    #[test]
    fn bundled_sprite_coordinates_match_real_unicode_index_and_skin_sheet_rules() {
        for (unicode, index) in [
            ("😀", 0),
            ("👍", 198),
            ("🍌", 725),
            ("🚀", 1072),
            ("🇺🇸", 1919),
        ] {
            let sprite = unicode_sprite(unicode).unwrap();
            assert_eq!(sprite, EmojiSprite { sheet: 0, index });
            assert_eq!((sprite.columns(), sprite.rows()), (42, 47));
        }
        for (tone, unicode) in ["👍🏻", "👍🏼", "👍🏽", "👍🏾", "👍🏿"].into_iter().enumerate()
        {
            let sprite = unicode_sprite(unicode).unwrap();
            assert_eq!(
                sprite,
                EmojiSprite {
                    sheet: tone as u8 + 1,
                    index: 27
                }
            );
            assert_eq!((sprite.columns(), sprite.rows()), (10, 33));
        }
        assert_eq!(
            unicode_sprite("✋🏽"),
            Some(EmojiSprite { sheet: 3, index: 3 })
        );
        assert_eq!(unicode_sprite("☺"), unicode_sprite("☺️"));
        assert!(unicode_sprite("not an emoji").is_none());
        // A base and its skin do not use the same sheet row or index.
        let base = unicode_sprite("👍").unwrap();
        let tone = unicode_sprite("👍🏽").unwrap();
        assert_eq!(
            (base.index % base.columns(), base.index / base.columns()),
            (30, 4)
        );
        assert_eq!(
            (tone.index % tone.columns(), tone.index / tone.columns()),
            (7, 2)
        );
    }

    #[test]
    fn sprite_metadata_matches_bundled_png_grid_dimensions_without_decoding_tiles() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fluxer_app/src/media/images/emoji-sprites");
        for (sheet, name) in ["emoji", "1f3fb", "1f3fc", "1f3fd", "1f3fe", "1f3ff"]
            .into_iter()
            .enumerate()
        {
            let sprite = EmojiSprite {
                sheet: sheet as u8,
                index: 0,
            };
            // image_dimensions reads the PNG header; no sheet or tile is decoded.
            let size =
                image::image_dimensions(directory.join(format!("spritesheet-{name}.png"))).unwrap();
            assert_eq!(size, (sprite.columns() * 32, sprite.rows() * 32));
            let retina =
                image::image_dimensions(directory.join(format!("spritesheet-{name}@2x.png")))
                    .unwrap();
            assert_eq!(retina, (sprite.columns() * 64, sprite.rows() * 64));
        }
    }

    #[test]
    fn explicit_tone_and_mixed_tone_choices_keep_exact_unicode_wire_and_correct_art_coverage() {
        let catalog = EmojiCatalog::new(&ClientState::default(), None);
        let thumb = catalog.search_with_tone("thumbsup", "people", Some("🏽"))[0].clone();
        assert_eq!(
            thumb.sprite,
            Some(EmojiSprite {
                sheet: 3,
                index: 27
            })
        );
        assert_eq!(
            (thumb.insert.as_str(), thumb.reaction.as_str()),
            ("👍🏽", "👍🏽")
        );
        let mixed = catalog.search("handshake_tone1_tone2", "people")[0].clone();
        assert_eq!(mixed.insert, "🫱🏻‍🫲🏼");
        assert_eq!(mixed.reaction, mixed.insert);
        assert!(
            mixed.sprite.is_none(),
            "mixed tones beyond the first five have no bundled tile"
        );
        assert!(unicode_sprite(&mixed.insert).is_none());
        assert!(
            EmojiCatalog::new(&custom_state(), Some(10.into()))
                .search("", "community")
                .iter()
                .all(|choice| choice.sprite.is_none()),
            "community art comes from its own emoji asset, not Unicode sheets"
        );
    }

    #[test]
    fn preferred_tone_does_not_override_explicit_variant_alias_or_unicode() {
        let catalog = EmojiCatalog::new(&ClientState::default(), None);
        assert_eq!(
            catalog
                .search_with_tone(" :THUMBSUP: ", "people", Some("🏽"))
                .iter()
                .map(|c| c.insert.as_str())
                .collect::<Vec<_>>(),
            ["👍🏽"]
        );
        for query in ["thumbsup_tone1", "👍🏻"] {
            assert_eq!(
                catalog
                    .search_with_tone(query, "people", Some("🏽"))
                    .iter()
                    .map(|c| c.insert.as_str())
                    .collect::<Vec<_>>(),
                ["👍🏻"]
            );
        }
        assert!(
            catalog
                .search("+1", "all")
                .iter()
                .any(|choice| choice.insert == "👍🏿")
        );
        assert!(
            catalog
                .search("good", "people")
                .iter()
                .any(|choice| choice.insert == "👍🏽")
        );
        assert!(
            catalog
                .search_with_tone("", "not-a-category", Some("🏽"))
                .is_empty()
        );
    }

    fn custom_state() -> ClientState {
        use fluxer_core::{Channel, Guild};
        let mut state = ClientState::default();
        for (guild, channel, emojis) in [
            (
                1,
                10,
                serde_json::json!([
                    {"id":"42","name":"Party","animated":true,"available":true},
                    {"id":"43","name":"still","animated":false},
                    {"id":"44","name":"hidden","available":false},
                    {"name":"missing_id"},
                    {"id":45,"name":"numeric_id"},
                    {"id":"invalid","name":"broken_id"},
                ]),
            ),
            (
                2,
                20,
                serde_json::json!([{"id":"99","name":"other_community"}]),
            ),
        ] {
            state.guilds.insert(
                guild.into(),
                Guild {
                    id: guild.into(),
                    name: "Community".into(),
                    extra: BTreeMap::from([("emojis".into(), emojis)]),
                    ..Guild::default()
                },
            );
            state.channels.insert(
                channel.into(),
                Channel {
                    id: channel.into(),
                    guild_id: Some(guild.into()),
                    ..Channel::default()
                },
            );
        }
        state.channels.insert(
            30.into(),
            Channel {
                id: 30.into(),
                kind: fluxer_core::ChannelType::Dm,
                ..Channel::default()
            },
        );
        state
    }

    #[test]
    fn custom_availability_and_context_preserve_actual_reaction_and_composer_wire() {
        let state = custom_state();
        let catalog = EmojiCatalog::new(&state, Some(10.into()));
        let choices = catalog.search("", "community");
        assert_eq!(choices.len(), 2);
        let party = &catalog.search(" :pArTy: ", "community")[0];
        assert_eq!(party.id, Some(42.into()));
        assert_eq!(party.key, "custom:42");
        assert_eq!(party.name, "Party");
        assert_eq!(party.category, "community");
        assert_eq!(party.label, ":Party:");
        assert!(party.animated);
        assert_eq!(party.insert, "<a:Party:42>");
        assert_eq!(party.reaction, "Party:42");
        assert_eq!(
            catalog.search("still", "community")[0].insert,
            "<:still:43>"
        );
        assert!(
            catalog
                .search("Party", "people")
                .iter()
                .all(|choice| choice.id.is_none())
        );
        assert!(
            !catalog
                .search("", "all")
                .iter()
                .any(|choice| choice.id == Some(99.into()))
        );
        assert_eq!(
            EmojiCatalog::new(&state, Some(20.into())).search("", "community")[0].reaction,
            "other_community:99"
        );
        assert!(
            EmojiCatalog::new(&state, Some(30.into()))
                .search("", "community")
                .is_empty()
        );
        assert!(
            EmojiCatalog::new(&state, None)
                .search("", "community")
                .is_empty()
        );
    }

    #[test]
    fn cached_handles_survive_search_navigation_and_new_snapshot_refreshes_availability() {
        let mut state = custom_state();
        let catalog = EmojiCatalog::new(&state, Some(10.into()));
        let first = catalog.search("Party", "community")[0].clone();
        let again = catalog
            .search("Party", "all")
            .into_iter()
            .find(|choice| choice.id.is_some())
            .unwrap();
        assert!(
            Arc::ptr_eq(&first, &again),
            "search results share choices rather than cloning their fields"
        );
        let grin = catalog.search("grinning", "people")[0].clone();
        let other = EmojiCatalog::new(&state, None).search("grinning", "people")[0].clone();
        assert!(
            Arc::ptr_eq(&grin, &other),
            "Unicode catalog is shared across picker instances"
        );
        state
            .guilds
            .get_mut(&1.into())
            .unwrap()
            .extra
            .get_mut("emojis")
            .unwrap()[0]["available"] = serde_json::json!(false);
        assert_eq!(
            catalog.search("Party", "community").len(),
            1,
            "existing picker snapshot is immutable"
        );
        assert!(
            EmojiCatalog::new(&state, Some(10.into()))
                .search("Party", "community")
                .is_empty()
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
