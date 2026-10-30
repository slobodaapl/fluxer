// SPDX-License-Identifier: AGPL-3.0-or-later

//! Language tags: which installed dictionary serves a requested tag, the languages a state
//! asks for, the dictionary chosen for an auto-detected language, and the dictionary catalogue
//! the renderer's language pickers list.
//!
//! Electron resolved tags against its bundled catalogue (SpellcheckDictionaries.ts) and, on
//! Linux, loaded the system Hunspell dictionary for the chosen catalogue tag. This shell has no
//! bundled dictionaries, so the installed system dictionaries take the catalogue's place.

use std::collections::HashSet;

use serde::Serialize;

use super::state::SpellcheckState;

/// `getNativeLocale()` before the renderer reported a locale. Electron's main process stored
/// the renderer's locale with the native strings; that channel is not part of this shell yet.
const NATIVE_LOCALE_FALLBACK: &str = "en-US";
/// The renderer uses `package` only as a list key; every entry here is a system dictionary.
const SYSTEM_DICTIONARY_PACKAGE: &str = "system";

/// Display names from `BUNDLED_DICTIONARIES` in SpellcheckDictionaries.ts.
const CATALOGUE_NAMES: [(&str, &str, &str); 41] = [
    ("bg", "Bulgarian", "Български"),
    ("ca", "Catalan", "Català"),
    ("cs", "Czech", "Čeština"),
    ("da", "Danish", "Dansk"),
    ("de", "German", "Deutsch"),
    ("el", "Greek", "Ελληνικά"),
    ("en", "English (US)", "English (US)"),
    ("en-us", "English (US)", "English (US)"),
    ("en-gb", "English (UK)", "English (UK)"),
    ("eo", "Esperanto", "Esperanto"),
    ("es", "Spanish", "Español"),
    ("et", "Estonian", "Eesti"),
    ("eu", "Basque", "Euskara"),
    ("fa", "Persian", "فارسی"),
    ("fo", "Faroese", "Føroyskt"),
    ("fr", "French", "Français"),
    ("gl", "Galician", "Galego"),
    ("he", "Hebrew", "עברית"),
    ("hr", "Croatian", "Hrvatski"),
    ("hu", "Hungarian", "Magyar"),
    ("is", "Icelandic", "Íslenska"),
    ("it", "Italian", "Italiano"),
    ("ko", "Korean", "한국어"),
    ("lt", "Lithuanian", "Lietuvių"),
    ("lv", "Latvian", "Latviešu"),
    ("nb", "Norwegian Bokmål", "Norsk Bokmål"),
    ("no", "Norwegian", "Norsk"),
    ("nl", "Dutch", "Nederlands"),
    ("pl", "Polish", "Polski"),
    ("pt", "Portuguese", "Português"),
    ("pt-br", "Portuguese (Brazil)", "Português (Brasil)"),
    ("ro", "Romanian", "Română"),
    ("ru", "Russian", "Русский"),
    ("sk", "Slovak", "Slovenčina"),
    ("sl", "Slovenian", "Slovenščina"),
    ("sr", "Serbian", "Српски"),
    ("sv", "Swedish", "Svenska"),
    ("sv-se", "Swedish", "Svenska"),
    ("tr", "Turkish", "Türkçe"),
    ("uk", "Ukrainian", "Українська"),
    ("vi", "Vietnamese", "Tiếng Việt"),
];

/// `SpellcheckBundledDictionary` in DesktopBridgeTypes.ts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueEntry {
    pub tag: String,
    pub package: &'static str,
    pub display_name: String,
    pub native_name: String,
}

fn norm(tag: &str) -> String {
    tag.to_lowercase()
}

/// `languageBase`.
pub fn language_base(tag: &str) -> String {
    let lower = norm(tag);
    match lower.split_once('-') {
        Some((base, _)) => base.to_owned(),
        None => lower,
    }
}

/// The installed tag serving `tag`: the same tag, else the first with the same base language
/// (`loadFromSystemHunspell`).
pub fn match_installed<'a>(tag: &str, installed: &'a [String]) -> Option<&'a str> {
    let wanted = norm(tag);
    let base = language_base(tag);
    installed
        .iter()
        .find(|candidate| norm(candidate) == wanted)
        .or_else(|| {
            installed
                .iter()
                .find(|candidate| language_base(candidate) == base)
        })
        .map(String::as_str)
}

fn dedupe(tags: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.into_iter()
        .filter(|tag| seen.insert(norm(tag)))
        .collect()
}

/// `requestedLanguages`.
pub fn requested_languages(
    state: &SpellcheckState,
    detected: Option<&str>,
    preferred: &[String],
) -> Vec<String> {
    if !state.auto_detect {
        return state.languages.clone();
    }
    dedupe(
        detected
            .map(str::to_owned)
            .into_iter()
            .chain(preferred.iter().cloned()),
    )
}

/// `chooseBundledTagForLanguage` over the installed dictionaries. `base` is the dictionary
/// base the bridge mapped the detected ISO 639-3 code to.
pub fn choose_tag_for_language(
    base: &str,
    preferred: &[String],
    installed: &[String],
) -> Option<String> {
    let base = language_base(base);
    let preferred_match = |accept: &dyn Fn(&str) -> bool| {
        preferred
            .iter()
            .filter_map(|tag| match_installed(tag, installed))
            .find(|tag| accept(tag))
            .map(str::to_owned)
    };
    if base == "en" {
        return preferred_match(&|tag| norm(tag) == "en-gb").or_else(|| {
            match_installed("en-US", installed)
                .or_else(|| match_installed("en", installed))
                .map(str::to_owned)
        });
    }
    preferred_match(&|tag| language_base(tag) == base)
        .or_else(|| match_installed(&base, installed).map(str::to_owned))
}

/// One POSIX locale value (`de_DE.UTF-8@euro`) as a language tag (`de-DE`).
fn locale_tag(raw: &str) -> Option<String> {
    let name = raw.split(['.', '@']).next().unwrap_or_default().trim();
    if name.is_empty() || name == "C" || name == "POSIX" {
        return None;
    }
    Some(name.replace('_', "-"))
}

/// `detectPreferredLanguages`: the native locale, the system's preferred languages
/// (`LANGUAGE`, then `LC_ALL`, `LC_MESSAGES` and `LANG`), deduplicated case-insensitively.
pub fn preferred_languages(env: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut tags = vec![NATIVE_LOCALE_FALLBACK.to_owned()];
    if let Some(list) = env("LANGUAGE") {
        tags.extend(list.split(':').filter_map(locale_tag));
    }
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        tags.extend(env(name).as_deref().and_then(locale_tag));
    }
    dedupe(tags)
}

/// BCP 47 casing for display: `en-us` → `en-US`, `sr-latn` → `sr-Latn`.
fn display_tag(tag: &str) -> String {
    tag.split('-')
        .enumerate()
        .map(|(index, part)| {
            let alphabetic = part.chars().all(|ch| ch.is_ascii_alphabetic());
            if index == 0 {
                part.to_lowercase()
            } else if (part.len() == 2 && alphabetic)
                || (part.len() == 3 && part.chars().all(|ch| ch.is_ascii_digit()))
            {
                part.to_uppercase()
            } else if part.len() == 4 && alphabetic {
                let mut chars = part.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_ascii_uppercase().to_string() + &chars.as_str().to_lowercase()
                })
            } else {
                part.to_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

fn catalogue_names(tag: &str) -> Option<(&'static str, &'static str)> {
    let wanted = norm(tag);
    CATALOGUE_NAMES
        .iter()
        .find(|(candidate, _, _)| *candidate == wanted)
        .map(|(_, display, native)| (*display, *native))
}

/// A catalogue entry for an installed dictionary. Tags the catalogue knows keep its names; a
/// regional variant it does not know gets the base language's name and the region.
pub fn catalogue_entry(tag: &str) -> CatalogueEntry {
    let shown = display_tag(tag);
    let (display_name, native_name) = match catalogue_names(tag) {
        Some((display, native)) => (display.to_owned(), native.to_owned()),
        None => match (catalogue_names(&language_base(tag)), shown.split_once('-')) {
            (Some((display, native)), Some((_, region))) => {
                let strip = |name: &str| name.split(" (").next().unwrap_or(name).to_owned();
                (
                    format!("{} ({region})", strip(display)),
                    format!("{} ({region})", strip(native)),
                )
            }
            _ => (shown.clone(), shown.clone()),
        },
    };
    CatalogueEntry {
        tag: shown,
        package: SYSTEM_DICTIONARY_PACKAGE,
        display_name,
        native_name,
    }
}
