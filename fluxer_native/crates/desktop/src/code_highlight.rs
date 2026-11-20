//! Arborium spans are computed on a bounded background queue.
use super::*;
use std::{
    collections::VecDeque,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Debug)]
pub(super) struct Token {
    pub range: Range<usize>,
    pub color: u32,
}
// The queue stores only tiny tickets; code remains in the owning input until
// admission. One parser at a time also bounds retained code to 128 KiB.
const MAX_WAITERS: usize = 8192;
const MAX_CODE_BYTES: usize = 128 * 1024;
const QUEUE_TIMEOUT: Duration = Duration::from_secs(12);
#[derive(Default)]
struct GateState {
    active: bool,
    next_id: u64,
    waiting: VecDeque<(u64, futures::channel::oneshot::Sender<Permit>)>,
}
#[derive(Default)]
struct Gate(Mutex<GateState>);
struct Permit(Arc<Gate>);
pub(super) struct Ticket {
    id: u64,
    gate: Arc<Gate>,
}
impl Gate {
    fn enqueue(self: &Arc<Self>) -> Option<(Ticket, futures::channel::oneshot::Receiver<Permit>)> {
        let (sender, receiver) = futures::channel::oneshot::channel();
        let id = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if state.waiting.len() >= MAX_WAITERS {
                return None;
            }
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            state.waiting.push_back((id, sender));
            id
        };
        self.pump();
        Some((
            Ticket {
                id,
                gate: self.clone(),
            },
            receiver,
        ))
    }
    fn pump(self: &Arc<Self>) {
        let sender = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if state.active {
                return;
            }
            let Some((_, sender)) = state.waiting.pop_front() else {
                return;
            };
            state.active = true;
            sender
        };
        // On a cancelled receiver, send returns the permit. Its drop releases
        // the slot and pumps the next waiter outside the mutex.
        let _ = sender.send(Permit(self.clone()));
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = self.gate.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = state.waiting.iter().position(|(id, _)| *id == self.id) {
            state.waiting.remove(index);
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.0.lock().unwrap_or_else(|e| e.into_inner()).active = false;
        self.0.pump();
    }
}
static GATE: OnceLock<Arc<Gate>> = OnceLock::new();

fn language(label: &str) -> Option<String> {
    let token = label.split_whitespace().next()?.to_ascii_lowercase();
    // Classic reserves these labels for KaTeX, including when it falls back
    // to plain code. Do not highlight them as ordinary source code.
    if token.len() > 64 || matches!(token.as_str(), "katex" | "latex" | "tex") {
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
static CACHE_EPOCH: AtomicU64 = AtomicU64::new(0);
pub(super) fn clear_cache() {
    CACHE_EPOCH.fetch_add(1, Ordering::AcqRel);
    if let Some(cache) = CACHE.get() {
        cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}
fn highlighted(
    highlighter: &mut arborium::Highlighter,
    lang: &str,
    code: &str,
    epoch: u64,
) -> Vec<Token> {
    if CACHE_EPOCH.load(Ordering::Acquire) != epoch {
        return Vec::new();
    }
    let cache = CACHE.get_or_init(|| Mutex::new(VecDeque::new()));
    if let Ok(mut entries) = cache.lock()
        && let Some(index) = entries.iter().position(|(l, s, _)| l == lang && s == code)
    {
        let entry = entries.remove(index).unwrap();
        let result = entry.2.clone();
        entries.push_back(entry);
        return result;
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
    if CACHE_EPOCH.load(Ordering::Acquire) != epoch {
        return Vec::new();
    }
    if code.len() <= 16 * 1024
        && result.len() * std::mem::size_of::<Token>() <= 32 * 1024
        && let Ok(mut entries) = cache.lock()
    {
        if CACHE_EPOCH.load(Ordering::Acquire) != epoch {
            return Vec::new();
        }
        entries.push_back((lang.to_owned(), code.to_owned(), result.clone()));
        while entries.len() > 128 {
            entries.pop_front();
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
    input.highlight_ticket = None;
    if !document.blocks.iter().any(|block| {
        matches!(
            block.kind,
            crate::message_markup::MessageBlockKind::Code { language: Some(_) }
        )
    }) {
        return;
    }
    let Some((ticket, receiver)) = GATE.get_or_init(|| Arc::new(Gate::default())).enqueue() else {
        return;
    };
    input.highlight_ticket = Some(ticket);
    let executor = cx.background_executor().clone();
    cx.spawn(async move |entity, cx| {
        let timeout = executor.timer(QUEUE_TIMEOUT);
        let permit = match futures::future::select(Box::pin(receiver), Box::pin(timeout)).await {
            futures::future::Either::Left((Ok(permit), _)) => permit,
            _ => {
                let _ = entity.update(cx, |input, _| {
                    if input.highlight_generation == generation {
                        input.highlight_ticket = None;
                    }
                });
                return;
            }
        };
        // Re-read after admission: a recycled or dropped input must never queue
        // a parse for its old source. Nothing copied the code while waiting.
        let document = entity
            .update(cx, |input, _| {
                if input.highlight_generation == generation {
                    input.highlight_ticket = None;
                }
                (input.highlight_generation == generation)
                    .then(|| input.message_document.clone())
                    .flatten()
            })
            .ok()
            .flatten();
        let Some(document) = document else {
            return;
        };
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
            if bytes > MAX_CODE_BYTES {
                break;
            }
            jobs.push((block.range.start, lang, code.to_owned()));
        }
        drop(document);
        if jobs.is_empty() {
            return;
        }
        let cache_epoch = CACHE_EPOCH.load(Ordering::Acquire);
        let tokens = executor
            .spawn(async move {
                let _permit = permit;
                std::panic::catch_unwind(|| {
                    let mut highlighter = arborium::Highlighter::new();
                    let mut result = Vec::new();
                    for (offset, lang, code) in jobs {
                        result.extend(
                            highlighted(&mut highlighter, &lang, &code, cache_epoch)
                                .into_iter()
                                .map(|token| Token {
                                    range: offset + token.range.start..offset + token.range.end,
                                    color: token.color,
                                }),
                        );
                    }
                    result
                })
                .unwrap_or_default()
            })
            .await;
        let _ = entity.update(cx, |input, cx| {
            if input.highlight_generation == generation
                && CACHE_EPOCH.load(Ordering::Acquire) == cache_epoch
            {
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
    fn tex_labels_use_plain_fallback_instead_of_syntax_highlighting() {
        for label in ["tex", "LaTeX extra", " KATEX "] {
            assert_eq!(language(label), None);
        }
        assert_eq!(language("rust extra").as_deref(), Some("rust"));
    }

    #[test]
    fn occupied_worker_services_more_than_eight_waiters_in_fifo_order() {
        let gate = Arc::new(Gate::default());
        let (_first_ticket, first_receiver) = gate.enqueue().unwrap();
        let first_permit = futures::executor::block_on(first_receiver).unwrap();
        let mut tickets = Vec::new();
        let mut receivers = Vec::new();
        for _ in 0..12 {
            let (ticket, receiver) = gate.enqueue().unwrap();
            tickets.push(Some(ticket));
            receivers.push(Some(receiver));
        }
        // A recycled input cancels its waiting job before the parser is free.
        drop(tickets[4].take());
        assert!(receivers[4].as_mut().unwrap().try_recv().is_err());
        assert!(matches!(
            receivers[0].as_mut().unwrap().try_recv(),
            Ok(None)
        ));
        drop(first_permit);
        let mut highlighter = arborium::Highlighter::new();
        let mut serviced = Vec::new();
        for index in 0..12 {
            if index == 4 {
                continue;
            }
            let permit = futures::executor::block_on(receivers[index].take().unwrap()).unwrap();
            // Each admitted request performs real syntax work, not merely a
            // counter check; the next request remains blocked until release.
            assert!(
                !highlighted(
                    &mut highlighter,
                    "rust",
                    &format!("let value_{index} = 1;"),
                    CACHE_EPOCH.load(Ordering::Acquire)
                )
                .is_empty()
            );
            if let Some(next) = (index + 1..12).find(|next| *next != 4) {
                assert!(matches!(
                    receivers[next].as_mut().unwrap().try_recv(),
                    Ok(None)
                ));
            }
            serviced.push(index);
            drop(permit);
        }
        assert_eq!(serviced, vec![0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11]);
        assert!(!gate.0.lock().unwrap().active);
    }

    #[test]
    fn arborium_rust_tokens_use_utf8_byte_offsets_and_theme() {
        let text = "let 雪 = \"✨\"; // comment\n";
        let mut highlighter = arborium::Highlighter::new();
        let stale_epoch = CACHE_EPOCH.load(Ordering::Acquire).wrapping_add(1);
        assert!(highlighted(&mut highlighter, "rust", text, stale_epoch).is_empty());
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
