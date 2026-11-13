// SPDX-License-Identifier: AGPL-3.0-or-later
//! Static avatar/icon URLs follow AvatarUtils.ts. Images use the application's
//! credential-free GPUI image client; built-in defaults never require HTTP.
use fluxer_core::{ClientState, Guild, Snowflake, User, VoiceParticipant};
use gpui::{http_client::Url, prelude::*, *};
use serde_json::Value;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use unicode_segmentation::UnicodeSegmentation;

const REQUEST_SIZE: &str = "160";
const DEFAULT_COLORS: [u32; 6] = [0x4641d9, 0xf0b100, 0x00bba7, 0x2b7fff, 0xad46ff, 0x6a7282];
static NEUTRAL_DEFAULTS: AtomicBool = AtomicBool::new(false);

/// There is one selected instance in this process. Call with its raw discovery
/// document on change, and with None on logout/reset, before rendering avatars.
pub fn configure_instance(document: Option<&Value>) {
    NEUTRAL_DEFAULTS.store(uses_neutral_defaults(document), Ordering::Relaxed);
}

fn uses_neutral_defaults(document: Option<&Value>) -> bool {
    let Some(document) = document else {
        return false;
    };
    document
        .pointer("/features/self_hosted")
        .and_then(Value::as_bool)
        == Some(true)
        && ["icon_url", "symbol_url"].iter().any(|key| {
            document
                .pointer(&format!("/app_public/branding/{key}"))
                .and_then(Value::as_str)
                .is_some_and(|url| !url.is_empty())
        })
}

fn default_index(user_id: Snowflake) -> usize {
    (user_id.get() % 6) as usize
}

fn default_image(index: usize, neutral: bool) -> Arc<Image> {
    static IMAGES: OnceLock<[[Arc<Image>; 6]; 2]> = OnceLock::new();
    let images = IMAGES.get_or_init(|| {
        let colored: [&[u8]; 6] = [
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/0.png"),
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/1.png"),
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/2.png"),
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/3.png"),
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/4.png"),
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/5.png"),
        ];
        let neutral: [&[u8]; 6] = [
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/neutral/0.png"),
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/neutral/1.png"),
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/neutral/2.png"),
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/neutral/3.png"),
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/neutral/4.png"),
            include_bytes!("../../../../fluxer_app/src/media/images/avatars/neutral/5.png"),
        ];
        [
            colored.map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()))),
            neutral.map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()))),
        ]
    });
    images[usize::from(neutral)][index].clone()
}

/// Hashes are data, never arbitrary URLs or path components. Static requests
/// remove a_ exactly once, as parseMediaHashForRequest(hash, false) does.
fn static_hash(hash: &str) -> Option<&str> {
    let hash = hash.strip_prefix("a_").unwrap_or(hash);
    (!hash.is_empty()
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(hash)
}
fn media_url(media: Option<&str>, path: &[&str], hash: &str) -> Option<String> {
    let hash = static_hash(hash)?;
    let mut url = Url::parse(media?).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    url.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .extend(path)
        .push(&format!("{hash}.webp"));
    url.query_pairs_mut().append_pair("size", REQUEST_SIZE);
    Some(url.into())
}
pub(super) fn user_avatar_url(user: &User, media: Option<&str>) -> Option<String> {
    media_url(
        media,
        &["avatars", &user.id.to_string()],
        user.avatar.as_deref()?,
    )
}
fn guild_icon_url(guild: &Guild, media: Option<&str>) -> Option<String> {
    media_url(
        media,
        &["icons", &guild.id.to_string()],
        guild.icon.as_deref()?,
    )
}
fn member_avatar_url(
    user_id: Snowflake,
    guild: Option<Snowflake>,
    global: Option<&str>,
    member: Option<&str>,
    unset: bool,
    media: Option<&str>,
) -> Option<String> {
    if unset {
        return None;
    }
    if let Some((guild, hash)) = guild.zip(member.filter(|hash| !hash.is_empty())) {
        media_url(
            media,
            &[
                "guilds",
                &guild.to_string(),
                "users",
                &user_id.to_string(),
                "avatars",
            ],
            hash,
        )
    } else {
        media_url(media, &["avatars", &user_id.to_string()], global?)
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .take(2)
        .filter_map(|part| part.graphemes(true).next())
        .collect::<String>()
}
fn emergency_avatar(user: Snowflake, name: &str, size: f32) -> AnyElement {
    let label = initials(name);
    div()
        .size_full()
        .rounded_full()
        .bg(rgb(DEFAULT_COLORS[default_index(user)]))
        .text_color(rgb(0xffffff))
        .text_size(px((size * 0.35).max(8.)))
        .flex()
        .items_center()
        .justify_center()
        .child(if label.is_empty() { "?".into() } else { label })
        .into_any_element()
}
fn default_avatar(user: Snowflake, name: &str, size: f32, neutral: bool) -> AnyElement {
    let name = name.to_owned();
    img(default_image(default_index(user), neutral))
        .size(px(size))
        .rounded_full()
        .object_fit(ObjectFit::Cover)
        .with_fallback(move || emergency_avatar(user, &name, size))
        .into_any_element()
}
fn render_avatar(
    user: Snowflake,
    name: &str,
    url: Option<String>,
    speaking: bool,
    size: f32,
) -> AnyElement {
    let neutral = NEUTRAL_DEFAULTS.load(Ordering::Relaxed);
    let body = if let Some(url) = url {
        let loading_name = name.to_owned();
        let fallback_name = name.to_owned();
        img(url)
            .size(px(size))
            .rounded_full()
            .object_fit(ObjectFit::Cover)
            .with_loading(move || default_avatar(user, &loading_name, size, neutral))
            .with_fallback(move || default_avatar(user, &fallback_name, size, neutral))
            .into_any_element()
    } else {
        default_avatar(user, name, size, neutral)
    };
    // Draw the ring above the image, without resizing its face or reflowing a
    // row when speaking changes. The overlay has no event handlers.
    div()
        .relative()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .child(body)
        .when(speaking, |view| {
            view.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_full()
                    .border_2()
                    .border_color(rgb(0x4fc58a)),
            )
        })
        .into_any_element()
}
pub fn user_avatar(user: &User, media: Option<&str>, size: f32) -> AnyElement {
    render_avatar(
        user.id,
        user.display_name(),
        user_avatar_url(user, media),
        false,
        size,
    )
}
pub fn voice_avatar(
    participant: &VoiceParticipant,
    guild: Option<Snowflake>,
    media: Option<&str>,
    speaking: bool,
    size: f32,
) -> AnyElement {
    render_avatar(
        participant.user_id,
        &participant.display_name,
        member_avatar_url(
            participant.user_id,
            guild,
            participant.avatar.as_deref(),
            participant.guild_avatar.as_deref(),
            participant.avatar_unset,
            media,
        ),
        speaking,
        size,
    )
}
pub fn member_avatar(
    state: &ClientState,
    user_id: Snowflake,
    guild: Option<Snowflake>,
    display_name: &str,
    media: Option<&str>,
    speaking: bool,
    size: f32,
) -> AnyElement {
    let member = guild.and_then(|guild| state.members.get(&(guild, user_id)));
    let user = state
        .current_user
        .as_ref()
        .filter(|user| user.id == user_id)
        .or_else(|| state.users.get(&user_id))
        .or_else(|| member.map(|member| &member.user));
    let unset = member
        .and_then(|member| member.profile_flags)
        .is_some_and(|flags| flags & 1 != 0);
    render_avatar(
        user_id,
        display_name,
        member_avatar_url(
            user_id,
            guild,
            user.and_then(|user| user.avatar.as_deref()),
            member.and_then(|member| member.avatar.as_deref()),
            unset,
            media,
        ),
        speaking,
        size,
    )
}

/// GuildAcronymUtils preserves punctuation/emoji while abbreviating Unicode
/// letter/number/underscore runs; possessive 's before whitespace is removed.
fn guild_acronym(name: &str) -> String {
    let mut normalized = String::new();
    let chars: Vec<_> = name.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if matches!(chars[index], '\'' | '’')
            && chars.get(index + 1) == Some(&'s')
            && chars.get(index + 2).is_some_and(|ch| ch.is_whitespace())
        {
            index += 2;
            continue;
        }
        normalized.push(chars[index]);
        index += 1;
    }
    let mut result = String::new();
    let mut in_word = false;
    for grapheme in normalized.graphemes(true) {
        let first = grapheme.chars().next().unwrap();
        if first.is_whitespace() {
            in_word = false;
            continue;
        }
        if first.is_alphanumeric() || first == '_' {
            if !in_word {
                result.push_str(grapheme);
            }
            in_word = true;
        } else if (first == '\u{200c}' || first == '\u{200d}') && in_word {
            continue;
        } else {
            result.push_str(grapheme);
            in_word = false;
        }
    }
    if result.is_empty() {
        "?".into()
    } else {
        result
    }
}
fn community_fallback(name: &str, size: f32) -> AnyElement {
    let acronym = guild_acronym(name);
    let count = acronym.graphemes(true).count().clamp(1, 6);
    let font_size = [18., 18., 16., 14., 12., 10.][count - 1] * size / 48.;
    div()
        .size_full()
        .rounded(px(size * 0.3))
        .bg(rgb(0x302d3b))
        .text_color(rgb(0xece9f6))
        .text_size(px(font_size))
        .font_weight(FontWeight::MEDIUM)
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .child(acronym)
        .into_any_element()
}
pub fn community_icon(guild: &Guild, media: Option<&str>, size: f32) -> AnyElement {
    let name = guild.name.clone();
    let body = if let Some(url) = guild_icon_url(guild, media) {
        let loading_name = name.clone();
        img(url)
            .size(px(size))
            .rounded(px(size * 0.3))
            .object_fit(ObjectFit::Cover)
            .with_loading(move || community_fallback(&loading_name, size))
            .with_fallback(move || community_fallback(&name, size))
            .into_any_element()
    } else {
        community_fallback(&name, size)
    };
    div()
        .size(px(size))
        .flex_shrink_0()
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn static_user_and_community_urls_preserve_discovered_prefix_and_source_size() {
        let user = User {
            id: 123.into(),
            avatar: Some("a_abc123".into()),
            ..Default::default()
        };
        let guild = Guild {
            id: 456.into(),
            icon: Some("a_def456".into()),
            ..Default::default()
        };
        assert_eq!(
            user_avatar_url(&user, Some("https://self.example/media/")),
            Some("https://self.example/media/avatars/123/abc123.webp?size=160".into())
        );
        assert_eq!(
            guild_icon_url(&guild, Some("https://self.example/media")),
            Some("https://self.example/media/icons/456/def456.webp?size=160".into())
        );
        assert_eq!(user_avatar_url(&user, None), None);
        assert_eq!(
            guild_icon_url(&Guild::default(), Some("https://self.example/media")),
            None
        );
    }

    #[test]
    fn member_override_and_avatar_unset_have_source_precedence() {
        let media = Some("https://self.example/media");
        assert_eq!(
            member_avatar_url(
                12.into(),
                Some(34.into()),
                Some("a_global"),
                Some("a_member"),
                false,
                media
            ),
            Some(
                "https://self.example/media/guilds/34/users/12/avatars/member.webp?size=160".into()
            )
        );
        assert_eq!(
            member_avatar_url(
                12.into(),
                Some(34.into()),
                Some("a_global"),
                None,
                false,
                media
            ),
            Some("https://self.example/media/avatars/12/global.webp?size=160".into())
        );
        assert_eq!(
            member_avatar_url(
                12.into(),
                None,
                Some("a_global"),
                Some("a_member"),
                false,
                media
            ),
            Some("https://self.example/media/avatars/12/global.webp?size=160".into())
        );
        // Clearing a guild avatar deliberately suppresses the global picture.
        assert_eq!(
            member_avatar_url(
                12.into(),
                Some(34.into()),
                Some("global"),
                Some("member"),
                true,
                media
            ),
            None
        );
        assert_eq!(
            member_avatar_url(12.into(), Some(34.into()), None, None, false, media),
            None
        );
    }

    #[test]
    fn malformed_hashes_and_credentialed_or_non_http_endpoints_never_become_images() {
        for hash in [
            "",
            "a_",
            "../escape",
            "hash?query=1",
            "https://elsewhere/image",
            "a_%2F",
        ] {
            assert_eq!(
                media_url(Some("https://self.example/media"), &["avatars", "12"], hash),
                None,
                "{hash}"
            );
        }
        for endpoint in [
            "file:///tmp/image",
            "data:image/png,abc",
            "https://name:secret@self.example/media",
        ] {
            assert_eq!(
                media_url(Some(endpoint), &["avatars", "12"], "valid"),
                None,
                "{endpoint}"
            );
        }
        assert_eq!(
            media_url(
                Some("https://self.example/media?old=query#fragment"),
                &["avatars", "12"],
                "hash"
            ),
            Some("https://self.example/media/avatars/12/hash.webp?size=160".into())
        );
    }

    #[test]
    fn bundled_default_faces_use_full_snowflake_modulo_and_distinct_neutral_assets() {
        assert_eq!(default_index(Snowflake::new(u64::MAX)), 3);
        for index in 0..6 {
            assert_eq!(default_index((index as u64 + 6).into()), index);
            let normal = default_image(index, false);
            let neutral = default_image(index, true);
            assert!(Arc::ptr_eq(&normal, &default_image(index, false)));
            assert_ne!(normal.bytes, neutral.bytes);
            for image in [normal, neutral] {
                let decoded = image::load_from_memory(&image.bytes).unwrap();
                assert_eq!((decoded.width(), decoded.height()), (512, 512));
            }
        }
    }

    #[test]
    fn neutral_defaults_require_self_hosted_and_icon_or_symbol_branding() {
        assert!(!uses_neutral_defaults(None));
        assert!(!uses_neutral_defaults(Some(
            &json!({"features":{"self_hosted":false},"app_public":{"branding":{"icon_url":"https://brand.example/icon"}}})
        )));
        assert!(!uses_neutral_defaults(Some(
            &json!({"features":{"self_hosted":true},"app_public":{"branding":{"icon_url":null,"symbol_url":null,"logo_url":"https://brand.example/logo"}}})
        )));
        for key in ["icon_url", "symbol_url"] {
            assert!(uses_neutral_defaults(Some(
                &json!({"features":{"self_hosted":true},"app_public":{"branding":{key:"https://brand.example/icon"}}})
            )));
        }
    }

    #[test]
    fn community_acronyms_keep_unicode_punctuation_and_remove_possessive_suffix() {
        assert_eq!(guild_acronym("Alyx's Example"), "AE");
        assert_eq!(guild_acronym("Rust & GPUI"), "R&G");
        assert_eq!(guild_acronym("👩‍💻 Café"), "👩‍💻C");
        assert_eq!(guild_acronym("e\u{301}clair Club"), "e\u{301}C");
        assert_eq!(guild_acronym("  "), "?");
    }
}
