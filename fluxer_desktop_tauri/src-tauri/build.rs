// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::{Path, PathBuf};

const BRIDGE_BUNDLE: &str = "../dist/bridge.js";
/// The English native strings live in the Electron shell's source; they are read from there so
/// the two shells cannot drift apart.
const MAIN_I18N_SOURCE: &str = "../../fluxer_desktop/src/main/MainI18n.ts";
const FALLBACK_STRINGS_MARKER: &str = "const FALLBACK_STRINGS";
const FALLBACK_STRINGS_OPEN: &str = "Object.freeze({";

fn main() {
    println!("cargo:rerun-if-changed={BRIDGE_BUNDLE}");
    if !Path::new(BRIDGE_BUNDLE).is_file() {
        panic!("{BRIDGE_BUNDLE} is missing; run `pnpm build:bridge` in fluxer_desktop_tauri first");
    }
    generate_fallback_strings();
    tauri_build::build();
}

/// Writes `FALLBACK_STRINGS` of MainI18n.ts as a Rust slice of `(key, template)` pairs.
fn generate_fallback_strings() {
    println!("cargo:rerun-if-changed={MAIN_I18N_SOURCE}");
    let source = std::fs::read_to_string(MAIN_I18N_SOURCE)
        .unwrap_or_else(|error| panic!("cannot read {MAIN_I18N_SOURCE}: {error}"));
    let entries = parse_fallback_strings(&source)
        .unwrap_or_else(|error| panic!("cannot parse FALLBACK_STRINGS of {MAIN_I18N_SOURCE}: {error}"));
    let mut generated = String::from("pub(super) const FALLBACK_STRINGS: &[(&str, &str)] = &[\n");
    for (key, value) in &entries {
        generated.push_str(&format!("    ({key:?}, {value:?}),\n"));
    }
    generated.push_str("];\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("fallback_strings.rs"), generated)
        .expect("the generated fallback strings are written");
}

/// Reads the object literal after `FALLBACK_STRINGS ... Object.freeze({`: quoted keys, quoted
/// values, commas, up to the closing brace. Anything else fails the build.
fn parse_fallback_strings(source: &str) -> Result<Vec<(String, String)>, String> {
    let start = source
        .find(FALLBACK_STRINGS_MARKER)
        .ok_or("no FALLBACK_STRINGS declaration")?;
    let open = source[start..]
        .find(FALLBACK_STRINGS_OPEN)
        .ok_or("no Object.freeze({ after FALLBACK_STRINGS")?;
    let mut chars = source[start + open + FALLBACK_STRINGS_OPEN.len()..]
        .chars()
        .peekable();
    let mut entries = Vec::new();
    loop {
        while chars.next_if(|c| c.is_whitespace() || *c == ',').is_some() {}
        match chars.peek() {
            Some('}') => return Ok(entries),
            Some('\'' | '"') => {}
            other => return Err(format!("expected a key, found {other:?}")),
        }
        let key = string_literal(&mut chars)?;
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.next() != Some(':') {
            return Err(format!("expected ':' after {key:?}"));
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let value = string_literal(&mut chars)?;
        entries.push((key, value));
    }
}

fn string_literal(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<String, String> {
    let quote = match chars.next() {
        Some(quote @ ('\'' | '"')) => quote,
        other => return Err(format!("expected a string literal, found {other:?}")),
    };
    let mut value = String::new();
    loop {
        match chars.next() {
            Some(c) if c == quote => return Ok(value),
            Some('\\') => match chars.next() {
                Some(escaped @ ('\\' | '\'' | '"')) => value.push(escaped),
                Some('n') => value.push('\n'),
                other => return Err(format!("unsupported escape \\{other:?} in {value:?}")),
            },
            Some('\n') | None => return Err(format!("unterminated string {value:?}")),
            Some(c) => value.push(c),
        }
    }
}
