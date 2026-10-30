// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs;
use std::path::{Path, PathBuf};

use fluxer_system_hunspell::{EnvSnapshot, Hunspell, HunspellError, discover_dictionaries};

fn fixture_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("fixture dir");
    dir
}

fn write_dictionary(dir: &Path, stem: &str, encoding: &str, words: &[&str]) -> (PathBuf, PathBuf) {
    let aff = dir.join(format!("{stem}.aff"));
    let dic = dir.join(format!("{stem}.dic"));
    fs::write(
        &aff,
        format!("SET {encoding}\nTRY esianrtolcdugmphbyfvkwz\n"),
    )
    .expect("aff");
    fs::write(&dic, format!("{}\n{}\n", words.len(), words.join("\n"))).expect("dic");
    (aff, dic)
}

#[test]
fn spells_suggests_and_edits_the_runtime_word_list() {
    let dir = fixture_dir("spell");
    let (aff, dic) = write_dictionary(&dir, "en_US", "UTF-8", &["hello", "world", "again"]);
    let mut hunspell = Hunspell::new(&aff, &dic).expect("load");

    assert_eq!(hunspell.spell("world"), Ok(true));
    assert_eq!(hunspell.spell("wrold"), Ok(false));
    let suggestions = hunspell.suggest("wrold", Some(5)).expect("suggest");
    assert!(
        suggestions.iter().any(|word| word == "world"),
        "{suggestions:?}"
    );

    hunspell.add("Fluxer").expect("add");
    assert_eq!(hunspell.spell("Fluxer"), Ok(true));
    hunspell.remove("Fluxer").expect("remove");
    assert_eq!(hunspell.spell("Fluxer"), Ok(false));

    hunspell.close();
    assert_eq!(hunspell.spell("world"), Ok(false));
    assert_eq!(hunspell.suggest("wrold", None), Ok(Vec::new()));
}

#[test]
fn rejects_dictionaries_that_are_not_utf8_or_missing() {
    let dir = fixture_dir("reject");
    let (aff, dic) = write_dictionary(&dir, "de_DE", "ISO8859-1", &["hallo"]);
    assert_eq!(
        Hunspell::new(&aff, &dic).unwrap_err(),
        HunspellError::Load("DictionaryNotUtf8")
    );
    let missing = dir.join("missing.aff");
    assert_eq!(
        Hunspell::new(&missing, &dic).unwrap_err().to_string(),
        "Hunspell load failed: LoadFailed"
    );
    assert_eq!(
        Hunspell::new(Path::new(""), &dic).unwrap_err(),
        HunspellError::InvalidArgument("path must be non-empty".to_owned())
    );
}

#[test]
fn rejects_words_with_nul_bytes() {
    let dir = fixture_dir("nul");
    let (aff, dic) = write_dictionary(&dir, "en", "UTF-8", &["hello"]);
    let hunspell = Hunspell::new(&aff, &dic).expect("load");
    assert_eq!(
        hunspell.spell("he\0llo"),
        Err(HunspellError::InvalidArgument(
            "word must not contain NUL bytes".to_owned()
        ))
    );
}

#[test]
fn discovery_canonicalizes_tags_and_keeps_the_first_directory_per_tag() {
    let first = fixture_dir("discover-first");
    let second = fixture_dir("discover-second");
    write_dictionary(&first, "en_US", "UTF-8", &["hello"]);
    write_dictionary(&second, "en-US", "UTF-8", &["hello"]);
    write_dictionary(&second, "pt_BR", "UTF-8", &["olá"]);
    fs::write(second.join("orphan.dic"), "1\nword\n").expect("orphan");

    let found = discover_dictionaries(&EnvSnapshot {
        hunspell_dict_dir: Some(format!("{}:{}", first.display(), second.display())),
        ..EnvSnapshot::default()
    });
    let mut tags: Vec<(&str, &Path)> = found
        .iter()
        .filter(|dict| dict.dic_path.starts_with(&first) || dict.dic_path.starts_with(&second))
        .map(|dict| (dict.tag.as_str(), dict.aff_path.parent().expect("parent")))
        .collect();
    tags.sort();
    assert_eq!(
        tags,
        vec![("en-us", first.as_path()), ("pt-br", second.as_path())]
    );
}
