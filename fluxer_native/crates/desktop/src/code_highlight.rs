//! Arborium spans are computed on a bounded background queue.
use super::*;
use std::{
    collections::VecDeque,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone, Debug)]
pub(super) struct Token {
    pub range: Range<usize>,
    pub color: u32,
}
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
const MAX_JOBS: usize = 8;

fn language(label: &str) -> Option<String> {
    let token = label.split_whitespace().next()?.to_ascii_lowercase();
    if token.len() > 64 {
        return None;
    }
    Some(
        match token.as_str() {
            "adoc" => "asciidoc",
            "ansible" => "yaml",
            "bat" => "batch",
            "cjs" => "javascript",
            "cl" => "commonlisp",
            "clj" => "clojure",
            "cljc" => "clojure",
            "cljs" => "clojure",
            "cmd" => "batch",
            "comp" => "glsl",
            "conf" => "ini",
            "config" => "ini",
            "c#" => "c-sharp",
            "cs" => "c-sharp",
            "csharp" => "c-sharp",
            "c++" => "cpp",
            "cc" => "cpp",
            "cfg" => "ini",
            "cts" => "typescript",
            "cxx" => "cpp",
            "diff" => "diff",
            "docker" => "dockerfile",
            "dts" => "devicetree",
            "dtsi" => "devicetree",
            "ecmascript" => "javascript",
            "erl" => "erlang",
            "ex" => "elixir",
            "exs" => "elixir",
            "f#" => "fsharp",
            "frag" => "glsl",
            "fs" => "fsharp",
            "fsi" => "fsharp",
            "fsx" => "fsharp",
            "gql" => "graphql",
            "graphqls" => "graphql",
            "h" => "cpp",
            "hrl" => "erlang",
            "hs" => "haskell",
            "hh" => "cpp",
            "htm" => "html",
            "hpp" => "cpp",
            "hxx" => "cpp",
            "j2" => "jinja2",
            "jinja" => "jinja2",
            "js" => "javascript",
            "json5" => "json",
            "jsonc" => "json",
            "jl" => "julia",
            "jsx" => "tsx",
            "kt" => "kotlin",
            "kts" => "kotlin",
            "ksh" => "bash",
            "lisp" => "commonlisp",
            "log" => "text",
            "md" => "markdown",
            "mdown" => "markdown",
            "mjs" => "javascript",
            "mkdn" => "markdown",
            "ml" => "ocaml",
            "mli" => "ocaml",
            "mm" => "objc",
            "mts" => "typescript",
            "nasm" => "x86asm",
            "node" => "javascript",
            "obj-c" => "objc",
            "objective-c" => "objc",
            "patch" => "diff",
            "pbtxt" => "textproto",
            "pgsql" => "sql",
            "plain" => "text",
            "plaintext" => "text",
            "pl" => "perl",
            "plist" => "xml",
            "pm" => "perl",
            "postgres" => "sql",
            "postgresql" => "sql",
            "proto" => "textproto",
            "protobuf" => "textproto",
            "ps" => "postscript",
            "ps1" => "powershell",
            "psd1" => "powershell",
            "psm1" => "powershell",
            "psql" => "sql",
            "pwsh" => "powershell",
            "py" => "python",
            "py3" => "python",
            "python3" => "python",
            "rb" => "ruby",
            "res" => "rescript",
            "resi" => "rescript",
            "rq" => "sparql",
            "rs" => "rust",
            "scm" => "scheme",
            "sh" => "bash",
            "shell" => "bash",
            "shellscript" => "bash",
            "ss" => "scheme",
            "svg" => "xml",
            "sv" => "verilog",
            "svh" => "verilog",
            "terraform" => "hcl",
            "tf" => "hcl",
            "tfvars" => "hcl",
            "tla" => "tlaplus",
            "ts" => "typescript",
            "txt" => "text",
            "typ" => "typst",
            "vbnet" => "vb",
            "vhd" => "vhdl",
            "viml" => "vim",
            "vimscript" => "vim",
            "vert" => "glsl",
            "xhtml" => "html",
            "yml" => "yaml",
            "zshell" => "zsh",
            _ => return Some(token),
        }
        .into(),
    )
}

fn color(tag: &str) -> Option<u32> {
    static THEME: OnceLock<arborium_theme::Theme> = OnceLock::new();
    let theme = THEME.get_or_init(arborium_theme::builtin::github_dark);
    let mut tag = tag;
    for _ in 0..4 {
        let index = arborium_theme::HIGHLIGHTS
            .iter()
            .position(|h| h.tag == tag)?;
        if let Some(c) = &theme.styles[index].fg {
            return Some(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32);
        }
        tag = arborium_theme::HIGHLIGHTS[index].parent_tag;
        if tag.is_empty() {
            break;
        }
    }
    None
}

type CacheEntry = (String, String, Vec<Token>);
static CACHE: OnceLock<Mutex<VecDeque<CacheEntry>>> = OnceLock::new();
fn highlighted(highlighter: &mut arborium::Highlighter, lang: &str, code: &str) -> Vec<Token> {
    let cache = CACHE.get_or_init(|| Mutex::new(VecDeque::new()));
    if let Ok(mut entries) = cache.lock() {
        if let Some(index) = entries.iter().position(|(l, s, _)| l == lang && s == code) {
            let entry = entries.remove(index).unwrap();
            let result = entry.2.clone();
            entries.push_back(entry);
            return result;
        }
    }
    let Ok(spans) = highlighter.highlight_spans(lang, code) else {
        return Vec::new();
    };
    if spans.len() > 200_000 {
        return Vec::new();
    }
    let result: Vec<_> = arborium_highlight::spans_to_flat_tokens(code, spans)
        .into_iter()
        .filter_map(|token| {
            let start = token.start as usize;
            let end = token.end as usize;
            if start >= end
                || end > code.len()
                || !code.is_char_boundary(start)
                || !code.is_char_boundary(end)
            {
                return None;
            }
            color(token.tag).map(|color| Token {
                range: start..end,
                color,
            })
        })
        .collect();
    if code.len() <= 16 * 1024 && result.len() * std::mem::size_of::<Token>() <= 32 * 1024 {
        if let Ok(mut entries) = cache.lock() {
            entries.push_back((lang.to_owned(), code.to_owned(), result.clone()));
            while entries.len() > 128 {
                entries.pop_front();
            }
        }
    }
    result
}

pub(super) fn schedule(
    input: &mut TextInput,
    document: Arc<MessageDocument>,
    cx: &mut Context<TextInput>,
) {
    input.highlight_generation = input.highlight_generation.wrapping_add(1);
    let generation = input.highlight_generation;
    input.highlighted.clear();
    let mut jobs = Vec::new();
    let mut bytes = 0usize;
    for block in &document.blocks {
        let crate::message_markup::MessageBlockKind::Code {
            language: Some(label),
        } = &block.kind
        else {
            continue;
        };
        let Some(lang) = language(label) else {
            continue;
        };
        let Some(code) = document.text.get(block.range.clone()) else {
            continue;
        };
        if code.encode_utf16().count() > 50_000 {
            continue;
        }
        bytes = bytes.saturating_add(code.len());
        if bytes > 128 * 1024 || jobs.len() >= 8 {
            break;
        }
        jobs.push((block.range.start, lang, code.to_owned()));
    }
    if jobs.is_empty()
        || IN_FLIGHT
            .try_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                (v < MAX_JOBS).then_some(v + 1)
            })
            .is_err()
    {
        return;
    }
    let executor = cx.background_executor().clone();
    cx.spawn(async move |entity, cx| {
        let tokens = executor
            .spawn(async move {
                struct Permit;
                impl Drop for Permit {
                    fn drop(&mut self) {
                        IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                let _permit = Permit;
                std::panic::catch_unwind(|| {
                    let mut highlighter = arborium::Highlighter::new();
                    let mut result = Vec::new();
                    for (offset, lang, code) in jobs {
                        result.extend(highlighted(&mut highlighter, &lang, &code).into_iter().map(
                            |token| Token {
                                range: offset + token.range.start..offset + token.range.end,
                                color: token.color,
                            },
                        ));
                    }
                    result
                })
                .unwrap_or_default()
            })
            .await;
        let _ = entity.update(cx, |input, cx| {
            if input.highlight_generation == generation {
                input.highlighted = tokens;
                cx.notify();
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arborium_rust_tokens_use_utf8_byte_offsets_and_theme() {
        let text = "let 雪 = \"✨\"; // comment\n";
        let mut highlighter = arborium::Highlighter::new();
        let spans = highlighter.highlight_spans("rust", text).unwrap();
        let tokens = arborium_highlight::spans_to_flat_tokens(text, spans);
        assert!(
            tokens.iter().any(
                |token| &text[token.start as usize..token.end as usize] == "let"
                    && color(token.tag).is_some()
            )
        );
        assert!(tokens.iter().any(|token| {
            text[token.start as usize..token.end as usize].contains("✨")
                && color(token.tag).is_some()
        }));
        assert!(
            tokens
                .iter()
                .all(|token| text.is_char_boundary(token.start as usize)
                    && text.is_char_boundary(token.end as usize))
        );
        assert_eq!(language("RS extra").as_deref(), Some("rust"));
        assert_eq!(language("jsx").as_deref(), Some("tsx"));
    }
}
