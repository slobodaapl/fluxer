// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::json;

use super::languages::{
    catalogue_entry, choose_tag_for_language, match_installed, preferred_languages,
    requested_languages,
};
use super::launch::LaunchMode;
use super::state::{Engine, SpellcheckState};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn a_patch_changes_only_the_fields_it_sets() {
    let previous = SpellcheckState {
        languages: strings(&["de"]),
        ..SpellcheckState::default()
    };
    let next = previous.patched(&json!({"enabled": false, "autoDetect": null}));
    assert_eq!(
        next,
        SpellcheckState {
            enabled: false,
            ..previous.clone()
        }
    );
    assert!(previous.patched(&json!({"enabled": "no"})).enabled);
    assert_eq!(previous.patched(&json!(null)), previous);
}

#[test]
fn an_unknown_engine_falls_back_to_auto_not_to_the_previous_engine() {
    let previous = SpellcheckState {
        engine: Engine::System,
        ..SpellcheckState::default()
    };
    assert_eq!(
        previous.patched(&json!({"engine": "hunspell"})).engine,
        Engine::Hunspell
    );
    assert_eq!(
        previous.patched(&json!({"engine": "chromium"})).engine,
        Engine::Auto
    );
    assert_eq!(previous.patched(&json!({"engine": 3})).engine, Engine::Auto);
    assert_eq!(previous.patched(&json!({})).engine, Engine::System);
}

#[test]
fn languages_and_personal_words_are_cleaned_and_deduplicated() {
    let next = SpellcheckState::default().patched(&json!({
        "languages": ["en-US", "", 7, "EN-us", "de"],
        "personalDictionary": ["  Fluxer ", "Fluxer", "fluxer", "\u{feff}", null, "x"],
    }));
    assert_eq!(next.languages, strings(&["en-US", "de"]));
    assert_eq!(
        next.personal_dictionary,
        strings(&["Fluxer", "fluxer", "x"])
    );
    let kept = next.patched(&json!({"languages": "de", "personalDictionary": {}}));
    assert_eq!(kept, next);
}

#[test]
fn launch_modes_override_the_renderer_state() {
    let state = SpellcheckState {
        enabled: false,
        engine: Engine::Hunspell,
        ..SpellcheckState::default()
    };
    assert_eq!(state.clone().with_launch_mode(LaunchMode::Default), state);
    let forced = state
        .clone()
        .with_launch_mode(LaunchMode::Engine(Engine::System));
    assert!(forced.enabled);
    assert_eq!(forced.engine, Engine::System);
    assert!(
        !SpellcheckState::default()
            .with_launch_mode(LaunchMode::Off)
            .enabled
    );
}

#[test]
fn a_tag_is_served_by_the_same_tag_or_else_its_base_language() {
    let installed = strings(&["en-au", "en-us", "de-de"]);
    assert_eq!(match_installed("en-US", &installed), Some("en-us"));
    assert_eq!(match_installed("en-CA", &installed), Some("en-au"));
    assert_eq!(match_installed("de", &installed), Some("de-de"));
    assert_eq!(match_installed("fr", &installed), None);
}

#[test]
fn auto_detect_requests_the_detected_dictionary_then_the_preferred_languages() {
    let preferred = strings(&["en-US", "de-DE"]);
    let mut state = SpellcheckState {
        languages: strings(&["fr"]),
        ..SpellcheckState::default()
    };
    assert_eq!(
        requested_languages(&state, Some("de-de"), &preferred),
        strings(&["de-de", "en-US"])
    );
    assert_eq!(requested_languages(&state, None, &preferred), preferred);
    state.auto_detect = false;
    assert_eq!(
        requested_languages(&state, Some("de-de"), &preferred),
        strings(&["fr"])
    );
}

#[test]
fn a_detected_language_prefers_the_users_regional_dictionary() {
    let installed = strings(&["en-gb", "en-us", "de-de", "de-ch"]);
    assert_eq!(
        choose_tag_for_language("en", &strings(&["en-US", "en-GB"]), &installed),
        Some("en-gb".to_owned())
    );
    assert_eq!(
        choose_tag_for_language("en", &strings(&["en-US"]), &installed),
        Some("en-us".to_owned())
    );
    assert_eq!(
        choose_tag_for_language("de", &strings(&["en-US", "de-CH"]), &installed),
        Some("de-ch".to_owned())
    );
    assert_eq!(
        choose_tag_for_language("de", &strings(&["en-US"]), &installed),
        Some("de-de".to_owned())
    );
    assert_eq!(
        choose_tag_for_language("fr", &strings(&["en-US"]), &installed),
        None
    );
}

#[test]
fn preferred_languages_come_from_the_native_locale_and_the_environment() {
    let env = |name: &str| match name {
        "LANGUAGE" => Some("de_AT:en_GB".to_owned()),
        "LC_ALL" => Some("C.UTF-8".to_owned()),
        "LANG" => Some("de_AT.UTF-8@euro".to_owned()),
        _ => None,
    };
    assert_eq!(
        preferred_languages(env),
        strings(&["en-US", "de-AT", "en-GB"])
    );
    assert_eq!(preferred_languages(|_| None), strings(&["en-US"]));
}

#[test]
fn catalogue_entries_name_installed_dictionaries() {
    let entry = catalogue_entry("en-us");
    assert_eq!(
        (
            entry.tag.as_str(),
            entry.display_name.as_str(),
            entry.native_name.as_str()
        ),
        ("en-US", "English (US)", "English (US)")
    );
    let entry = catalogue_entry("en-au");
    assert_eq!(
        (entry.display_name.as_str(), entry.native_name.as_str()),
        ("English (AU)", "English (AU)")
    );
    let entry = catalogue_entry("de-ch-frami");
    assert_eq!(
        (entry.tag.as_str(), entry.native_name.as_str()),
        ("de-CH-frami", "Deutsch (CH-frami)")
    );
    let entry = catalogue_entry("tlh");
    assert_eq!(
        (entry.tag.as_str(), entry.display_name.as_str()),
        ("tlh", "tlh")
    );
}

#[cfg(target_os = "linux")]
mod service {
    use std::fs;
    use std::path::Path;

    use serde_json::json;
    use tempfile::TempDir;

    use super::strings;
    use crate::spellcheck::dictionaries::Installed;
    use crate::spellcheck::launch::LaunchMode;
    use crate::spellcheck::state::Engine;
    use crate::spellcheck::{ResolvedEngine, ResolvedMode, Spellcheck, tags};

    fn dictionary(dir: &Path, stem: &str, tag: &str, words: &[&str]) -> Installed {
        let aff_path = dir.join(format!("{stem}.aff"));
        let dic_path = dir.join(format!("{stem}.dic"));
        fs::write(&aff_path, "SET UTF-8\nTRY esianrtolcdugmphbyfvkwz\n").expect("aff");
        fs::write(
            &dic_path,
            format!("{}\n{}\n", words.len(), words.join("\n")),
        )
        .expect("dic");
        Installed {
            tag: tag.to_owned(),
            aff_path,
            dic_path,
        }
    }

    fn fixture() -> (TempDir, Vec<Installed>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let installed = vec![
            dictionary(dir.path(), "en_US", "en-us", &["hello", "world", "again"]),
            dictionary(dir.path(), "de_DE", "de-de", &["hallo", "welt"]),
        ];
        (dir, installed)
    }

    fn apply(spellcheck: &Spellcheck, installed: &[Installed]) -> ResolvedEngine {
        let mut out = None;
        spellcheck.apply(installed, |_, resolved| out = Some(resolved.clone()));
        out.expect("apply hands on its result")
    }

    fn hunspell(langs: &[&str]) -> ResolvedEngine {
        ResolvedEngine {
            mode: ResolvedMode::Hunspell,
            hunspell_langs: strings(langs),
            system_langs: Vec::new(),
        }
    }

    fn off() -> ResolvedEngine {
        ResolvedEngine {
            mode: ResolvedMode::Off,
            hunspell_langs: Vec::new(),
            system_langs: Vec::new(),
        }
    }

    #[test]
    fn the_preferred_languages_load_and_answer_the_context_menu() {
        let (_dir, installed) = fixture();
        let spellcheck = Spellcheck::with(LaunchMode::Default, strings(&["en-US"]));
        assert_eq!(apply(&spellcheck, &installed), hunspell(&["en-us"]));
        let suggestions = spellcheck.misspelling("wrold").expect("misspelled");
        assert!(
            suggestions.iter().any(|word| word == "world"),
            "{suggestions:?}"
        );
        assert!(suggestions.len() <= 12);
        assert_eq!(spellcheck.misspelling("world"), None);
        assert_eq!(spellcheck.misspelling("Fluxer"), None, "built-in allowlist");
        assert!(
            spellcheck.misspelling("welt").is_some(),
            "German is not loaded"
        );
    }

    #[test]
    fn personal_words_follow_the_state() {
        let (_dir, installed) = fixture();
        let spellcheck = Spellcheck::with(LaunchMode::Default, strings(&["en-US"]));
        assert!(spellcheck.add_word("Zorblax"));
        assert!(!spellcheck.add_word(""));
        apply(&spellcheck, &installed);
        assert_eq!(spellcheck.misspelling("Zorblax"), None);
        spellcheck.set_state(&json!({"personalDictionary": []}));
        apply(&spellcheck, &installed);
        assert!(spellcheck.misspelling("Zorblax").is_some());
    }

    #[test]
    fn manual_languages_load_without_auto_detect() {
        let (_dir, installed) = fixture();
        let spellcheck = Spellcheck::with(LaunchMode::Default, strings(&["en-US"]));
        spellcheck.set_state(&json!({"autoDetect": false, "languages": ["de-DE", "fr"]}));
        assert_eq!(apply(&spellcheck, &installed), hunspell(&["de-de"]));
        assert_eq!(spellcheck.misspelling("welt"), None);
        assert!(spellcheck.misspelling("world").is_some());
    }

    #[test]
    fn disabled_spellcheck_and_the_bundled_only_engine_resolve_off() {
        let (_dir, installed) = fixture();
        let spellcheck = Spellcheck::with(LaunchMode::Default, strings(&["en-US"]));
        apply(&spellcheck, &installed);
        spellcheck.set_state(&json!({"enabled": false}));
        assert_eq!(
            spellcheck.misspelling("wrold"),
            None,
            "the state gates before an apply"
        );
        assert_eq!(apply(&spellcheck, &installed), off());
        spellcheck.set_state(&json!({"enabled": true, "engine": "hunspell"}));
        assert_eq!(apply(&spellcheck, &installed), off());
        assert_eq!(spellcheck.misspelling("wrold"), None);
        assert!(spellcheck.available_languages(&tags(&installed)).is_empty());
        let forced = Spellcheck::with(LaunchMode::Engine(Engine::System), strings(&["en-US"]));
        forced.set_state(&json!({"engine": "hunspell"}));
        assert_eq!(
            forced.available_languages(&tags(&installed)),
            strings(&["en-us", "de-de"])
        );
        assert_eq!(apply(&forced, &installed), hunspell(&["en-us"]));
    }

    #[test]
    fn a_detected_language_is_loaded_first_until_it_changes() {
        let (_dir, installed) = fixture();
        let spellcheck = Spellcheck::with(LaunchMode::Default, strings(&["en-US"]));
        let installed_tags = tags(&installed);
        assert!(spellcheck.update_detected_language(Some("de"), &installed_tags));
        assert!(!spellcheck.update_detected_language(Some("de"), &installed_tags));
        assert_eq!(
            apply(&spellcheck, &installed),
            hunspell(&["de-de", "en-us"])
        );
        assert!(spellcheck.update_detected_language(None, &installed_tags));
        assert_eq!(apply(&spellcheck, &installed), hunspell(&["en-us"]));
        spellcheck.set_state(&json!({"autoDetect": false}));
        assert!(!spellcheck.update_detected_language(Some("de"), &installed_tags));
    }

    #[test]
    fn the_catalogue_lists_installed_dictionaries_by_tag() {
        let (_dir, installed) = fixture();
        let catalogue = Spellcheck::catalogue(&tags(&installed));
        let listed: Vec<(&str, &str)> = catalogue
            .iter()
            .map(|entry| (entry.tag.as_str(), entry.native_name.as_str()))
            .collect();
        assert_eq!(
            listed,
            vec![("de-DE", "Deutsch (DE)"), ("en-US", "English (US)")]
        );
    }
}
