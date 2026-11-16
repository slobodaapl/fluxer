// SPDX-License-Identifier: AGPL-3.0-or-later
//! Projection of the existing Fluxer Rust parser AST, not a second Markdown
//! parser. Byte ranges refer to rendered `text`; `source` remains the original
//! body for the source client's explicit Copy Message action.
//!
//! Source: MarkdownParserFlags.ts (context flags), MarkdownParseCache.ts
//! (source/context cache), MarkdownParserWasm.ts/UrlUtils.ts (URL cleanup),
//! MentionRenderer.tsx/NicknameUtils.ts (labels), EmojiDetector.ts and
//! CustomEmojiImageUrl.ts (registered metadata and image size), and
//! Markup.module.css (concealed spoilers are not selectable).
//!
//! All parser AST variants retain their text. Remaining presentation differences
//! are reported in `unsupported`: command/guild-navigation activation and
//! remaining block presentation details. Timestamp tokens keep source byte ranges
//! and use locale-aware presentation at layout time; Unicode uses bundled art.
use fluxer_core::{ClientState, Message, Snowflake};
use fluxer_markdown_parser::{
    EmojiContext, MarkdownParser, Node, ParserFlags,
    ast::{EmojiKind, MentionKind},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    hash::{Hash, Hasher},
    ops::Range,
    sync::{Arc, OnceLock},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MessageContext {
    #[default]
    Standard,
    InlineReply,
    InlinePreview,
}
impl MessageContext {
    pub fn flags(self) -> u32 {
        let reply = ParserFlags::ALL
            & !(ParserFlags::ALLOW_BLOCKQUOTES
                | ParserFlags::ALLOW_MULTILINE_BLOCKQUOTES
                | ParserFlags::ALLOW_TABLES
                | ParserFlags::ALLOW_ALERTS
                | ParserFlags::ALLOW_HEADINGS
                | ParserFlags::ALLOW_LISTS);
        match self {
            Self::Standard => ParserFlags::ALL,
            Self::InlineReply => reply,
            Self::InlinePreview => reply | ParserFlags::ALLOW_HEADINGS | ParserFlags::ALLOW_LISTS,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub quote_depth: u8,
    pub heading: u8,
    pub subtext: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MessageTarget {
    Link(String),
    Author {
        channel: Snowflake,
        message: Snowflake,
    },
    User(Snowflake),
    Channel(Snowflake),
    Role(Snowflake),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CustomEmoji {
    pub id: String,
    pub name: String,
    pub animated: bool,
    pub url: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageSpan {
    pub range: Range<usize>,
    pub style: MessageStyle,
    pub target: Option<MessageTarget>,
    pub spoilers: Vec<u32>,
    pub custom_emoji: Option<CustomEmoji>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageBlockKind {
    Code {
        language: Option<String>,
    },
    Quote {
        depth: u8,
    },
    Heading {
        level: u8,
    },
    List {
        ordered: bool,
        depth: u8,
    },
    ListItem {
        marker: Range<usize>,
    },
    Table {
        rows: Vec<Vec<Range<usize>>>,
        alignments: Vec<fluxer_markdown_parser::ast::TableAlignment>,
    },
    Alert {
        kind: fluxer_markdown_parser::ast::AlertType,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageBlock {
    pub range: Range<usize>,
    pub kind: MessageBlockKind,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageDocument {
    pub source: String,
    pub text: String,
    pub spans: Vec<MessageSpan>,
    pub blocks: Vec<MessageBlock>,
    pub unsupported: Vec<&'static str>,
    pub jumbo_emojis: bool,
    pub restricted_inline: bool,
    pub emoji_autoplay: bool,
    pub timestamp_preferences: crate::message_timestamp::Preferences,
    pub timestamps: Vec<crate::message_timestamp::Timestamp>,
}
impl MessageDocument {
    /// A compact author's name shares the first text line and wrapping. Keep
    /// source intact for Copy Message and shift all projected byte ranges.
    pub fn with_author(&self, message: &Message) -> Self {
        let name = message.author.display_name();
        // Block content starts on its own line, preserving code/quote/list
        // indentation and backgrounds instead of absorbing it into the author.
        let separator = if self.blocks.iter().any(|block| block.range.start == 0) {
            '\n'
        } else {
            ' '
        };
        let prefix = format!("{name}{separator}");
        let offset = prefix.len();
        let mut doc = self.clone();
        doc.text.insert_str(0, &prefix);
        for span in &mut doc.spans {
            span.range = span.range.start + offset..span.range.end + offset;
        }
        for timestamp in &mut doc.timestamps {
            timestamp.range = timestamp.range.start + offset..timestamp.range.end + offset;
        }
        // Compact authors never participate in emoji-only sizing.
        doc.jumbo_emojis = false;
        for block in &mut doc.blocks {
            block.range = block.range.start + offset..block.range.end + offset;
            if let MessageBlockKind::ListItem { marker } = &mut block.kind {
                *marker = marker.start + offset..marker.end + offset;
            }
            if let MessageBlockKind::Table { rows, .. } = &mut block.kind {
                for cell in rows.iter_mut().flatten() {
                    *cell = cell.start + offset..cell.end + offset;
                }
            }
        }
        doc.spans.splice(
            0..0,
            [
                MessageSpan {
                    range: 0..name.len(),
                    style: MessageStyle {
                        bold: true,
                        ..Default::default()
                    },
                    target: Some(MessageTarget::Author {
                        channel: message.channel_id,
                        message: message.id,
                    }),
                    spoilers: vec![],
                    custom_emoji: None,
                },
                MessageSpan {
                    range: name.len()..offset,
                    style: MessageStyle::default(),
                    target: None,
                    spoilers: vec![],
                    custom_emoji: None,
                },
            ],
        );
        doc
    }
    pub fn plain(source: &str) -> Self {
        let mut doc = Self {
            source: source.into(),
            text: String::new(),
            spans: vec![],
            blocks: vec![],
            unsupported: vec![],
            jumbo_emojis: false,
            restricted_inline: false,
            emoji_autoplay: true,
            timestamp_preferences: Default::default(),
            timestamps: vec![],
        };
        doc.push(source, &Run::default(), None);
        doc
    }
    /// Selection copy follows rendered text, not raw Markdown. Concealed
    /// spoilers have user-select:none in Markup.module.css:716-721.
    pub fn copy_range(&self, range: Range<usize>, revealed: &BTreeSet<u32>) -> String {
        let mut result = String::new();
        for span in &self.spans {
            if span.spoilers.iter().any(|id| !revealed.contains(id)) {
                continue;
            }
            let mut start = range.start.max(span.range.start).min(self.text.len());
            let mut end = range.end.min(span.range.end).min(self.text.len());
            while start < end && !self.text.is_char_boundary(start) {
                start += 1;
            }
            while end > start && !self.text.is_char_boundary(end) {
                end -= 1;
            }
            if start >= end {
                continue;
            }
            if let Some(emoji) = &span.custom_emoji {
                result.push(':');
                result.push_str(&emoji.name);
                result.push(':');
            } else {
                let mut cursor = start;
                for timestamp in self
                    .timestamps
                    .iter()
                    .filter(|timestamp| timestamp.range.start < end && timestamp.range.end > start)
                {
                    let from = timestamp.range.start.max(cursor);
                    result.push_str(&self.text[cursor..from]);
                    result.push_str(&crate::message_timestamp::format(
                        timestamp.seconds,
                        timestamp.style,
                        &self.timestamp_preferences,
                        crate::message_timestamp::now(),
                    ));
                    cursor = timestamp.range.end.min(end);
                }
                result.push_str(&self.text[cursor..end]);
            }
        }
        result
    }
    fn push(&mut self, text: &str, run: &Run, emoji: Option<CustomEmoji>) {
        if text.is_empty() {
            return;
        }
        let range = self.text.len()..self.text.len() + text.len();
        self.text.push_str(text);
        let span = MessageSpan {
            range,
            style: run.style.clone(),
            target: run.target.clone(),
            spoilers: run.spoilers.clone(),
            custom_emoji: emoji,
        };
        if let Some(last) = self.spans.last_mut()
            && last.range.end == span.range.start
            && last.style == span.style
            && last.target == span.target
            && last.spoilers == span.spoilers
            && last.custom_emoji.is_none()
            && span.custom_emoji.is_none()
        {
            last.range.end = span.range.end;
        } else {
            self.spans.push(span);
        }
    }
    fn note(&mut self, kind: &'static str) {
        if !self.unsupported.contains(&kind) {
            self.unsupported.push(kind);
        }
    }
}
#[derive(Clone, Default)]
struct Run {
    style: MessageStyle,
    target: Option<MessageTarget>,
    spoilers: Vec<u32>,
    list_depth: u8,
    ordered_depth: u8,
    unordered_depth: u8,
}
struct Resolver<'a> {
    message: &'a Message,
    state: &'a ClientState,
    media: Option<&'a str>,
}
impl Resolver<'_> {
    fn guild(&self) -> Option<Snowflake> {
        self.state
            .channels
            .get(&self.message.channel_id)
            .and_then(|c| c.guild_id)
            .or_else(|| {
                self.message
                    .extra
                    .get("guild_id")
                    .and_then(|v| v.as_str())
                    .and_then(|v| v.parse().ok())
            })
    }
    fn user_label(&self, id: &str) -> String {
        let Ok(user_id) = id.parse::<Snowflake>() else {
            return format!("@{id}");
        };
        let user = self
            .state
            .users
            .get(&user_id)
            .or_else(|| self.message.mentions.iter().find(|u| u.id == user_id))
            .or_else(|| self.state.current_user.as_ref().filter(|u| u.id == user_id));
        let mut name = user
            .map(|u| u.display_name().to_owned())
            .unwrap_or_else(|| id.into());
        if let Some(nick) = self
            .state
            .relationships
            .get(&user_id)
            .and_then(|r| r.nickname.as_deref())
            .filter(|n| !n.is_empty())
        {
            name = nick.into();
        }
        if let Some(guild) = self.guild() {
            if let Some(nick) = self
                .state
                .members
                .get(&(guild, user_id))
                .and_then(|m| m.nick.as_deref())
                .filter(|n| !n.is_empty())
            {
                name = nick.into();
            }
        } else if let Some(nick) = self
            .state
            .channels
            .get(&self.message.channel_id)
            .and_then(|c| c.extra.get("nicks"))
            .and_then(|n| n.get(id))
            .and_then(|n| n.as_str())
            .filter(|n| !n.is_empty())
        {
            name = nick.into();
        }
        format!("@{name}")
    }
    fn mention(&self, kind: &MentionKind) -> (String, Option<MessageTarget>) {
        match kind {
            MentionKind::User { id } => {
                let user = id.parse().ok().filter(|user_id| {
                    self.state.users.contains_key(user_id)
                        || self.message.mentions.iter().any(|user| user.id == *user_id)
                        || self
                            .state
                            .current_user
                            .as_ref()
                            .is_some_and(|user| user.id == *user_id)
                });
                (self.user_label(id), user.map(MessageTarget::User))
            }
            MentionKind::Role { id } => {
                let role = self
                    .guild()
                    .and_then(|id| self.state.guilds.get(&id))
                    .and_then(|guild| guild.roles.iter().find(|r| r.id.to_string() == *id));
                (
                    format!(
                        "@{}",
                        role.map(|r| r.name.as_str()).unwrap_or("Unknown role")
                    ),
                    role.map(|role| MessageTarget::Role(role.id)),
                )
            }
            MentionKind::Channel { id } => {
                let channel = id.parse().ok().and_then(|id| self.state.channels.get(&id));
                let fallback = self
                    .message
                    .extra
                    .get("mention_channels")
                    .and_then(|v| v.as_array())
                    .and_then(|items| {
                        items
                            .iter()
                            .find(|v| v.get("id").and_then(|v| v.as_str()) == Some(id))
                    })
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str());
                (
                    format!(
                        "#{}",
                        channel
                            .map(|c| c.name.as_str())
                            .or(fallback)
                            .unwrap_or("unknown-channel")
                    ),
                    channel
                        .filter(|c| c.kind != fluxer_core::ChannelType::Category)
                        .map(|c| MessageTarget::Channel(c.id)),
                )
            }
            MentionKind::Everyone => ("@everyone".into(), None),
            MentionKind::Here => ("@here".into(), None),
            _ => (
                fluxer_markdown_parser::plaintext::render_ast_to_plaintext(
                    &[Node::Mention { kind: kind.clone() }],
                    &Default::default(),
                ),
                None,
            ),
        }
    }
    fn custom(&self, id: &str, name: &str, animated: bool) -> CustomEmoji {
        let known = self
            .state
            .guilds
            .values()
            .filter_map(|g| g.extra.get("emojis").and_then(|v| v.as_array()))
            .flatten()
            .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(id));
        let name = known
            .and_then(|e| e.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or(name)
            .to_owned();
        let animated = known
            .and_then(|e| e.get("animated"))
            .and_then(|v| v.as_bool())
            .unwrap_or(animated);
        let url = self.media.and_then(|base| {
            if id.parse::<u64>().is_err() {
                return None;
            }
            let mut url =
                reqwest::Url::parse(&format!("{}/emojis/{id}.webp", base.trim_end_matches('/')))
                    .ok()?;
            if !matches!(url.scheme(), "http" | "https") {
                return None;
            }
            let _ = url.set_username("");
            let _ = url.set_password(None);
            if animated {
                url.query_pairs_mut().append_pair("animated", "true");
            }
            url.query_pairs_mut().append_pair("size", "128");
            Some(url.into())
        });
        CustomEmoji {
            id: id.into(),
            name,
            animated,
            url,
        }
    }
}
struct Projector<'a> {
    doc: MessageDocument,
    resolver: Resolver<'a>,
    next_spoiler: u32,
}
impl Projector<'_> {
    fn nodes(&mut self, nodes: &[Node], run: &Run) {
        let mut previous_block = false;
        for node in nodes {
            let block = is_block(node);
            if (previous_block || block)
                && !self.doc.text.is_empty()
                && !self.doc.text.ends_with('\n')
            {
                self.doc.push("\n", run, None);
            }
            self.node(node, run);
            previous_block = block;
        }
    }
    fn node(&mut self, node: &Node, run: &Run) {
        let mut nested = run.clone();
        let start = self.doc.text.len();
        let mut block = None;
        match node {
            Node::Text { content } => self.doc.push(content, run, None),
            Node::Strong { children } => {
                nested.style.bold = true;
                self.nodes(children, &nested);
            }
            Node::Emphasis { children } => {
                nested.style.italic = true;
                self.nodes(children, &nested);
            }
            Node::Underline { children } => {
                nested.style.underline = true;
                self.nodes(children, &nested);
            }
            Node::Strikethrough { children } => {
                nested.style.strike = true;
                self.nodes(children, &nested);
            }
            Node::Spoiler { children, .. } => {
                let id = self.next_spoiler;
                self.next_spoiler += 1;
                nested.spoilers.push(id);
                self.nodes(children, &nested);
            }
            Node::InlineCode { content } => {
                nested.style.code = true;
                self.doc.push(content, &nested, None);
            }
            Node::CodeBlock { language, content } => {
                nested.style.code = true;
                self.doc.push(content, &nested, None);
                block = Some(MessageBlockKind::Code {
                    language: language.clone(),
                });
            }
            Node::Blockquote { children, .. } => {
                nested.style.quote_depth = nested.style.quote_depth.saturating_add(1);
                self.nodes(children, &nested);
                block = Some(MessageBlockKind::Quote {
                    depth: nested.style.quote_depth,
                });
            }
            Node::Heading { level, children } => {
                nested.style.heading = *level;
                nested.style.bold = true;
                self.nodes(children, &nested);
                block = Some(MessageBlockKind::Heading { level: *level });
            }
            Node::Subtext { children } => {
                nested.style.subtext = true;
                self.nodes(children, &nested);
            }
            Node::Sequence { children } | Node::TableCell { children } => self.nodes(children, run),
            Node::Link {
                text,
                raw_url,
                source,
                ..
            } => {
                let label = text.as_ref().map(|n| link_text(n)).unwrap_or_default();
                if let Some(url) = clean_link(raw_url, &label) {
                    nested.target = Some(MessageTarget::Link(url.clone()));
                    if let Some(text) = text {
                        self.node(text, &nested);
                    } else {
                        self.doc.push(&url, &nested, None);
                    }
                } else {
                    self.doc.push(source, run, None);
                }
            }
            Node::Mention { kind } => {
                let (label, target) = self.resolver.mention(kind);
                nested.target = target;
                if matches!(
                    kind,
                    MentionKind::Command { .. } | MentionKind::GuildNavigation { .. }
                ) {
                    self.doc.note("Command/guild navigation interaction");
                }
                self.doc.push(&label, &nested, None);
            }
            Node::Emoji {
                kind: EmojiKind::Custom { id, name, animated },
            } => {
                let emoji = self.resolver.custom(id, name, *animated);
                self.doc.push("\u{2003}", run, Some(emoji));
            }
            Node::Emoji {
                kind: EmojiKind::Standard { raw, .. },
            } => self.doc.push(raw, run, None),
            Node::List { ordered, items } => {
                nested.list_depth = run.list_depth.saturating_add(1);
                if *ordered {
                    nested.ordered_depth = run.ordered_depth.saturating_add(1);
                } else {
                    nested.unordered_depth = run.unordered_depth.saturating_add(1);
                }
                let first_ordinal = items.first().and_then(|item| item.ordinal).unwrap_or(1);
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        self.doc.push("\n", run, None);
                    }
                    let marker = if *ordered {
                        format!(
                            "{}. ",
                            ordered_marker(
                                first_ordinal.saturating_add(index),
                                nested.ordered_depth
                            )
                        )
                    } else {
                        match nested.unordered_depth {
                            1 => "• ",
                            2 => "◦ ",
                            _ => "▪ ",
                        }
                        .into()
                    };
                    let item_start = self.doc.text.len();
                    self.doc.push(&marker, run, None);
                    let marker_range = item_start..self.doc.text.len();
                    self.nodes(&item.children, &nested);
                    self.doc.blocks.push(MessageBlock {
                        range: item_start..self.doc.text.len(),
                        kind: MessageBlockKind::ListItem {
                            marker: marker_range,
                        },
                    });
                }
                block = Some(MessageBlockKind::List {
                    ordered: *ordered,
                    depth: nested.list_depth,
                });
            }
            Node::Table {
                header,
                rows,
                alignments,
            } => {
                let mut cell_rows = Vec::new();
                for (row_index, row) in std::iter::once(header.as_ref())
                    .chain(rows.iter())
                    .enumerate()
                {
                    if row_index > 0 {
                        self.doc.push("\n", run, None);
                    }
                    if let Node::TableRow { cells } = row {
                        let mut ranges = Vec::new();
                        for (column, cell) in cells.iter().enumerate() {
                            if column > 0 {
                                self.doc.push("\t", run, None);
                            }
                            let cell_start = self.doc.text.len();
                            let mut cell_run = run.clone();
                            if row_index == 0 {
                                cell_run.style.bold = true;
                            }
                            self.node(cell, &cell_run);
                            ranges.push(cell_start..self.doc.text.len());
                        }
                        cell_rows.push(ranges);
                    } else {
                        self.node(row, run);
                    }
                }
                block = Some(MessageBlockKind::Table {
                    rows: cell_rows,
                    alignments: alignments.clone(),
                });
            }
            Node::TableRow { cells } => {
                for (index, cell) in cells.iter().enumerate() {
                    if index > 0 {
                        self.doc.push("\t", run, None);
                    }
                    self.node(cell, run);
                }
            }
            Node::Alert {
                alert_type,
                children,
            } => {
                nested.style.bold = true;
                self.doc.push(&format!("{alert_type:?}\n"), &nested, None);
                self.nodes(children, run);
                block = Some(MessageBlockKind::Alert { kind: *alert_type });
                self.doc.note("Alert icon, localization and card spacing");
            }
            Node::Timestamp { timestamp, style } => {
                let code = match style {
                    fluxer_markdown_parser::ast::TimestampStyle::ShortTime => "t",
                    fluxer_markdown_parser::ast::TimestampStyle::LongTime => "T",
                    fluxer_markdown_parser::ast::TimestampStyle::ShortDate => "d",
                    fluxer_markdown_parser::ast::TimestampStyle::LongDate => "D",
                    fluxer_markdown_parser::ast::TimestampStyle::ShortDateTime => "f",
                    fluxer_markdown_parser::ast::TimestampStyle::LongDateTime => "F",
                    fluxer_markdown_parser::ast::TimestampStyle::RelativeTime => "R",
                    fluxer_markdown_parser::ast::TimestampStyle::ShortDateShortTime => "s",
                    fluxer_markdown_parser::ast::TimestampStyle::ShortDateMediumTime => "S",
                };
                self.doc.push(&format!("<t:{timestamp}:{code}>"), run, None);
                self.doc
                    .timestamps
                    .push(crate::message_timestamp::Timestamp {
                        range: start..self.doc.text.len(),
                        seconds: i64::try_from(*timestamp).unwrap_or(i64::MAX),
                        style: *style,
                    });
            }
        }
        if let Some(kind) = block {
            self.doc.blocks.push(MessageBlock {
                range: start..self.doc.text.len(),
                kind,
            });
        }
    }
}
fn ordered_marker(mut number: usize, depth: u8) -> String {
    if depth == 2 && number > 0 {
        let mut letters = Vec::new();
        while number > 0 {
            number -= 1;
            letters.push((b'a' + (number % 26) as u8) as char);
            number /= 26;
        }
        return letters.into_iter().rev().collect();
    }
    if depth >= 3 && (1..=3999).contains(&number) {
        let mut result = String::new();
        for (value, glyph) in [
            (1000, "m"),
            (900, "cm"),
            (500, "d"),
            (400, "cd"),
            (100, "c"),
            (90, "xc"),
            (50, "l"),
            (40, "xl"),
            (10, "x"),
            (9, "ix"),
            (5, "v"),
            (4, "iv"),
            (1, "i"),
        ] {
            while number >= value {
                result.push_str(glyph);
                number -= value;
            }
        }
        return result;
    }
    number.to_string()
}
fn is_block(node: &Node) -> bool {
    matches!(
        node,
        Node::Blockquote { .. }
            | Node::Heading { .. }
            | Node::Subtext { .. }
            | Node::List { .. }
            | Node::CodeBlock { .. }
            | Node::Table { .. }
            | Node::Alert { .. }
            | Node::Spoiler {
                is_block: Some(true),
                ..
            }
    )
}
// Matches MarkdownParserWasm.textFromNode: only Text and children, not
// separately interpreted mention/emoji/code labels.
fn link_text(node: &Node) -> String {
    match node {
        Node::Text { content } => content.clone(),
        Node::Strong { children }
        | Node::Emphasis { children }
        | Node::Underline { children }
        | Node::Strikethrough { children }
        | Node::Spoiler { children, .. }
        | Node::Heading { children, .. }
        | Node::Subtext { children }
        | Node::Blockquote { children, .. }
        | Node::Sequence { children }
        | Node::TableCell { children }
        | Node::Alert { children, .. } => children.iter().map(link_text).collect(),
        _ => String::new(),
    }
}
fn clean_link(url: &str, label: &str) -> Option<String> {
    if url.starts_with('/') && !url.starts_with("//") {
        return None;
    }
    if ["http://", "https://", "fluxer:"]
        .iter()
        .any(|prefix| label.trim().starts_with(prefix))
    {
        let target = reqwest::Url::parse(url).ok()?;
        let shown = reqwest::Url::parse(label.trim()).ok()?;
        if target.origin() != shown.origin()
            || target.path() != shown.path()
            || target.query() != shown.query()
            || target.fragment() != shown.fragment()
        {
            return None;
        }
    }
    // URLUtils preserves special schemes, trimming only trailing slashes.
    for prefix in ["mailto:", "tel:", "sms:"] {
        if let Some(value) = url.strip_prefix(prefix) {
            let value = value.trim_end_matches('/');
            let value = if prefix != "mailto:" && value.starts_with('+') {
                value
                    .chars()
                    .filter(|c| !c.is_whitespace() && !['-', '(', ')'].contains(c))
                    .collect::<String>()
            } else {
                value.to_owned()
            };
            return Some(format!("{prefix}{value}"));
        }
    }
    // Desktop URLUtils accepts other schemes only in :// form.
    if !url.contains("://") {
        return None;
    }
    let mut parsed = reqwest::Url::parse(url).ok()?;
    if !matches!(
        parsed.scheme(),
        "http" | "https" | "mailto" | "tel" | "sms" | "fluxer"
    ) {
        return None;
    }
    if matches!(parsed.scheme(), "http" | "https") {
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
    }
    Some(parsed.into())
}
fn parse_nodes(
    source: &str,
    context: MessageContext,
) -> Result<Vec<Node>, fluxer_markdown_parser::ParseError> {
    // MarkdownParser::clone shares its immutable emoji context through Arc.
    // Avoid copying the complete bundled shortcode catalog on every cache miss.
    static STANDARD: OnceLock<MarkdownParser> = OnceLock::new();
    static REPLY: OnceLock<MarkdownParser> = OnceLock::new();
    static PREVIEW: OnceLock<MarkdownParser> = OnceLock::new();
    let slot = match context {
        MessageContext::Standard => &STANDARD,
        MessageContext::InlineReply => &REPLY,
        MessageContext::InlinePreview => &PREVIEW,
    };
    slot.get_or_init(|| MarkdownParser::new(context.flags(), unicode_context().clone()))
        .clone()
        .parse(source)
}
fn unicode_context() -> &'static EmojiContext {
    static CONTEXT: OnceLock<EmojiContext> = OnceLock::new();
    CONTEXT.get_or_init(|| {
        let mut rows = String::new();
        for emoji in crate::chat_helpers::emoji_choices(&ClientState::default(), None, "", "all")
            .into_iter()
            .filter(|e| e.id.is_none())
        {
            let points = emoji
                .insert
                .chars()
                .map(|c| format!("{:x}", c as u32))
                .collect::<Vec<_>>()
                .join("-");
            for name in std::iter::once(&emoji.name).chain(emoji.aliases.iter()) {
                if !name.contains(['\t', '\n']) {
                    rows.push_str(&format!("C\t{name}\t{}\t{points}\n", emoji.insert));
                }
            }
        }
        EmojiContext::parse(&rows)
    })
}
pub fn document(
    message: &Message,
    state: &ClientState,
    media: Option<&str>,
    context: MessageContext,
) -> MessageDocument {
    match parse_nodes(&message.content, context) {
        Ok(nodes) => project(message, state, media, &nodes, context),
        Err(_) => {
            let mut doc = MessageDocument::plain(&message.content);
            doc.note("Parser error: literal source fallback");
            doc
        }
    }
}
fn project(
    message: &Message,
    state: &ClientState,
    media: Option<&str>,
    nodes: &[Node],
    context: MessageContext,
) -> MessageDocument {
    let mut projector = Projector {
        doc: MessageDocument {
            source: message.content.clone(),
            text: String::new(),
            spans: vec![],
            blocks: vec![],
            unsupported: vec![],
            jumbo_emojis: false,
            restricted_inline: false,
            emoji_autoplay: true,
            timestamp_preferences: Default::default(),
            timestamps: vec![],
        },
        resolver: Resolver {
            message,
            state,
            media,
        },
        next_spoiler: 0,
    };
    projector.doc.jumbo_emojis = crate::emoji_presentation::jumbo(
        nodes,
        context == MessageContext::Standard,
        &state.user_settings,
    );
    projector.doc.restricted_inline = context != MessageContext::Standard;
    projector.doc.emoji_autoplay = crate::emoji_presentation::autoplay(&state.user_settings, false);
    projector.doc.timestamp_preferences =
        crate::message_timestamp::Preferences::from_settings(&state.user_settings);
    projector.nodes(nodes, &Run::default());
    projector.doc
}

/// Mirrors source/context parse caching with additional byte and visible-entry
/// bounds. Context labels are fingerprinted only for AST dependencies.
#[derive(Default)]
pub struct MessageCache {
    parsed: VecDeque<(MessageContext, String, Arc<Vec<Node>>)>,
    documents: BTreeMap<(Snowflake, Snowflake, MessageContext), (u64, Arc<MessageDocument>)>,
    bytes: usize,
}
impl MessageCache {
    pub fn get(
        &mut self,
        message: &Message,
        state: &ClientState,
        media: Option<&str>,
        context: MessageContext,
    ) -> Arc<MessageDocument> {
        let position = self
            .parsed
            .iter()
            .position(|(c, s, _)| *c == context && s == &message.content);
        let nodes = if let Some(index) = position {
            let entry = self.parsed.remove(index).expect("cached index");
            let nodes = entry.2.clone();
            self.parsed.push_back(entry);
            nodes
        } else {
            let Ok(nodes) = parse_nodes(&message.content, context) else {
                let mut doc = MessageDocument::plain(&message.content);
                doc.note("Parser error: literal source fallback");
                return Arc::new(doc);
            };
            let nodes = Arc::new(nodes);
            self.bytes += message.content.len();
            self.parsed
                .push_back((context, message.content.clone(), nodes.clone()));
            while self.parsed.len() > 2048 || self.bytes > 4 * 1024 * 1024 {
                if let Some((_, source, _)) = self.parsed.pop_front() {
                    self.bytes -= source.len();
                } else {
                    break;
                }
            }
            nodes
        };
        let resolver = Resolver {
            message,
            state,
            media,
        };
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        message.content.hash(&mut hash);
        media.hash(&mut hash);
        let time_preferences =
            crate::message_timestamp::Preferences::from_settings(&state.user_settings);
        time_preferences.locale.hash(&mut hash);
        time_preferences.hour12.hash(&mut hash);
        crate::emoji_presentation::autoplay(&state.user_settings, false).hash(&mut hash);
        fluxer_core::compact_messages(&state.user_settings).hash(&mut hash);
        dependencies(&nodes, &resolver, &mut hash);
        let fingerprint = hash.finish();
        let key = (message.channel_id, message.id, context);
        if let Some((previous, doc)) = self.documents.get(&key)
            && *previous == fingerprint
        {
            return doc.clone();
        }
        let doc = Arc::new(project(message, state, media, &nodes, context));
        self.documents.insert(key, (fingerprint, doc.clone()));
        while self.documents.len() > 256 {
            let key = *self.documents.keys().next().unwrap();
            self.documents.remove(&key);
        }
        doc
    }
    pub fn retain_visible(&mut self, visible: &BTreeSet<(Snowflake, Snowflake)>) {
        self.documents
            .retain(|(channel, id, _), _| visible.contains(&(*channel, *id)));
    }
    pub fn clear(&mut self) {
        self.parsed.clear();
        self.documents.clear();
        self.bytes = 0;
    }
}
fn dependencies(nodes: &[Node], resolver: &Resolver<'_>, hash: &mut impl Hasher) {
    for node in nodes {
        match node {
            Node::Mention { kind } => resolver.mention(kind).hash(hash),
            Node::Emoji {
                kind: EmojiKind::Custom { id, name, animated },
            } => resolver.custom(id, name, *animated).hash(hash),
            Node::Strong { children }
            | Node::Emphasis { children }
            | Node::Underline { children }
            | Node::Strikethrough { children }
            | Node::Spoiler { children, .. }
            | Node::Heading { children, .. }
            | Node::Subtext { children }
            | Node::Blockquote { children, .. }
            | Node::Sequence { children }
            | Node::TableCell { children }
            | Node::Alert { children, .. } => dependencies(children, resolver, hash),
            Node::List { items, .. } => {
                for item in items {
                    dependencies(&item.children, resolver, hash);
                }
            }
            Node::Link {
                text: Some(text), ..
            } => dependencies(std::slice::from_ref(text.as_ref()), resolver, hash),
            Node::Table { header, rows, .. } => {
                dependencies(std::slice::from_ref(header.as_ref()), resolver, hash);
                dependencies(rows, resolver, hash);
            }
            Node::TableRow { cells } => dependencies(cells, resolver, hash),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxer_core::{Channel, Guild, Member, Role, User};
    fn message(source: &str) -> Message {
        Message::text(100.into(), 10.into(), User::default(), source)
    }
    fn render(source: &str) -> MessageDocument {
        document(
            &message(source),
            &ClientState::default(),
            None,
            MessageContext::Standard,
        )
    }
    fn partition(doc: &MessageDocument) {
        let mut end = 0;
        for span in &doc.spans {
            assert_eq!(span.range.start, end, "spans must partition text");
            assert!(span.range.end > span.range.start);
            assert!(doc.text.is_char_boundary(span.range.start));
            assert!(doc.text.is_char_boundary(span.range.end));
            end = span.range.end;
        }
        assert_eq!(end, doc.text.len());
    }
    #[test]
    fn table_cells_keep_unicode_alignment_selection_and_author_offsets() {
        let source = "| Name | Value |\n| :--- | ---: |\n| 雪 | ||hidden|| |";
        let doc = render(source);
        let block = doc
            .blocks
            .iter()
            .find(|b| matches!(b.kind, MessageBlockKind::Table { .. }))
            .expect("table");
        let MessageBlockKind::Table { rows, alignments } = &block.kind else {
            unreachable!()
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(&doc.text[rows[1][0].clone()], "雪");
        assert_eq!(
            alignments[1],
            fluxer_markdown_parser::ast::TableAlignment::Right
        );
        assert_eq!(
            doc.copy_range(block.range.clone(), &BTreeSet::new()),
            "Name\tValue\n雪\t"
        );
        let mut msg = message(source);
        msg.author.username = "É".into();
        let compact = doc.with_author(&msg);
        let offset = compact.text.len() - doc.text.len();
        let shifted = compact
            .blocks
            .iter()
            .find_map(|b| {
                if let MessageBlockKind::Table { rows, .. } = &b.kind {
                    Some(rows)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(
            shifted[1][0],
            (rows[1][0].start + offset)..(rows[1][0].end + offset)
        );
        assert_eq!(compact.source, source);
    }

    #[test]
    fn list_markers_keep_byte_ranges_and_shift_with_compact_author() {
        let doc = render("3. outer 😄\n   1. inner");
        let markers: Vec<_> = doc
            .blocks
            .iter()
            .filter_map(|b| {
                if let MessageBlockKind::ListItem { marker } = &b.kind {
                    Some(marker.clone())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            markers
                .iter()
                .any(|range| &doc.text[range.clone()] == "3. ")
        );
        assert!(
            markers
                .iter()
                .any(|range| &doc.text[range.clone()] == "a. ")
        );
        let mut msg = message(&doc.source);
        msg.author.username = "É".into();
        let compact = doc.with_author(&msg);
        let offset = compact.text.len() - doc.text.len();
        for range in markers {
            assert!(compact.blocks.iter().any(|b| matches!(&b.kind, MessageBlockKind::ListItem { marker } if *marker == ((range.start + offset)..(range.end + offset)))));
        }
        assert_eq!(compact.source, doc.source);
    }
    #[test]
    fn nested_ordered_markers_follow_css_and_continue_first_ordinal() {
        assert_eq!(ordered_marker(1, 1), "1");
        assert_eq!(ordered_marker(27, 2), "aa");
        assert_eq!(ordered_marker(49, 3), "xlix");
        let doc = render("3. outer\n   1. inner\n      1. deep");
        assert!(doc.text.contains("3. outer"));
        assert!(doc.text.contains("a. inner"));
        assert!(doc.text.contains("i. deep"));
    }
    #[test]
    fn nested_lists_and_alert_keep_ranges_for_compact_author_and_copy() {
        let source = "- outer 😄\n  - inner ||secret||\n\n> [!NOTE]\n> body";
        let doc = render(source);
        partition(&doc);
        let nested = doc
            .blocks
            .iter()
            .find(|b| matches!(b.kind, MessageBlockKind::List { depth: 2, .. }))
            .expect("nested list");
        assert!(doc.text[nested.range.clone()].contains("inner"));
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b.kind, MessageBlockKind::Alert { .. }))
        );
        assert!(
            !doc.copy_range(0..doc.text.len(), &BTreeSet::new())
                .contains("secret")
        );
        let mut msg = message(source);
        msg.author.username = "Aléx".into();
        let compact = doc.with_author(&msg);
        partition(&compact);
        let offset = compact.text.len() - doc.text.len();
        assert!(
            compact
                .blocks
                .iter()
                .any(|b| b.range == ((nested.range.start + offset)..(nested.range.end + offset)))
        );
        assert_eq!(compact.source, source);
    }
    #[test]
    fn nested_unicode_formats_and_escapes_use_existing_ast() {
        let doc = render("**bold *ital😄* end** __under__ ~~gone~~ \\*literal\\*");
        assert_eq!(doc.text, "bold ital😄 end under gone *literal*");
        partition(&doc);
        let nested = doc
            .spans
            .iter()
            .find(|s| doc.text[s.range.clone()].contains("ital😄"))
            .unwrap();
        assert!(nested.style.bold && nested.style.italic);
        assert!(doc.spans.iter().any(|s| s.style.underline));
        assert!(doc.spans.iter().any(|s| s.style.strike));
    }
    #[test]
    fn code_blocks_quotes_and_lists_keep_content_without_parsing_literals() {
        let doc = render("> **quote**\n\n```rust\n**literal** <@12>\n```\n\n- one\n- two");
        partition(&doc);
        assert!(doc.text.contains("quote"));
        assert!(doc.text.contains("**literal** <@12>"));
        assert!(doc.text.contains("• one\n• two"));
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b.kind, MessageBlockKind::Code { .. }))
        );
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b.kind, MessageBlockKind::Quote { .. }))
        );
        assert!(
            doc.spans
                .iter()
                .filter(|s| s.style.code)
                .all(|s| s.target.is_none())
        );
        assert_eq!(render("`**literal**` ").text, "**literal** ");
    }
    #[test]
    fn spoiler_selection_omits_hidden_links_and_emoji_but_source_copy_remains_raw() {
        let source = "visible ||**secret😄** [private](https://example.org) <:party:123>|| end";
        let doc = render(source);
        partition(&doc);
        assert_eq!(doc.source, source);
        assert_eq!(
            doc.copy_range(0..doc.text.len(), &BTreeSet::new()),
            "visible  end"
        );
        let revealed = doc
            .spans
            .iter()
            .flat_map(|s| s.spoilers.iter().copied())
            .collect();
        let copied = doc.copy_range(0..doc.text.len(), &revealed);
        assert_eq!(copied, "visible secret😄 private :party: end");
        // Arbitrary byte selections never slice inside a Unicode scalar.
        for start in 0..doc.text.len() {
            let _ = doc.copy_range(start..start + 1, &revealed);
        }
    }
    #[test]
    fn custom_emoji_resolves_registered_metadata_safe_media_and_copy_alias() {
        let mut state = ClientState::default();
        let mut guild = Guild {
            id: 20.into(),
            ..Default::default()
        };
        guild.extra.insert(
            "emojis".into(),
            serde_json::json!([{"id":"123","name":"renamed","animated":true}]),
        );
        state.guilds.insert(guild.id, guild);
        let doc = document(
            &message("<:old:123>"),
            &state,
            Some("https://name:secret@media.example/media"),
            MessageContext::Standard,
        );
        assert_eq!(doc.text, "\u{2003}");
        let emoji = doc.spans[0].custom_emoji.as_ref().unwrap();
        assert_eq!(emoji.name, "renamed");
        assert_eq!(
            emoji.url.as_deref(),
            Some("https://media.example/media/emojis/123.webp?animated=true&size=128")
        );
        assert_eq!(doc.copy_range(0..3, &BTreeSet::new()), ":renamed:");
        let bad = document(
            &message("<:x:123>"),
            &state,
            Some("file:///private"),
            MessageContext::Standard,
        );
        assert!(bad.spans[0].custom_emoji.as_ref().unwrap().url.is_none());
    }
    #[test]
    fn mention_projection_uses_source_nickname_and_fallback_precedence() {
        let mut state = ClientState::default();
        let user = User {
            id: 12.into(),
            username: "username".into(),
            global_name: Some("Display".into()),
            ..Default::default()
        };
        state.users.insert(user.id, user.clone());
        state.channels.insert(
            10.into(),
            Channel {
                id: 10.into(),
                guild_id: Some(20.into()),
                ..Default::default()
            },
        );
        state.channels.insert(
            11.into(),
            Channel {
                id: 11.into(),
                name: "general".into(),
                ..Default::default()
            },
        );
        state.guilds.insert(
            20.into(),
            Guild {
                id: 20.into(),
                roles: vec![Role {
                    id: 21.into(),
                    name: "Mods".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        state.members.insert(
            (20.into(), 12.into()),
            Member {
                user,
                nick: Some("Guild Nick".into()),
                ..Default::default()
            },
        );
        let mut msg = message("<@12> <@&21> <#11> <#99> <@88> @everyone");
        msg.extra.insert(
            "mention_channels".into(),
            serde_json::json!([{"id":"99","name":"archived"}]),
        );
        let doc = document(&msg, &state, None, MessageContext::Standard);
        assert_eq!(
            doc.text,
            "@Guild Nick @Mods #general #archived @88 @everyone"
        );
        assert_eq!(
            doc.spans
                .iter()
                .filter(|s| matches!(s.target, Some(MessageTarget::Channel(_))))
                .count(),
            1
        );
        partition(&doc);
    }
    #[test]
    fn url_cleaning_matches_source_protocol_phone_credentials_and_masked_url_policy() {
        assert_eq!(
            clean_link("https://user:pass@bücher.example/a", "label").as_deref(),
            Some("https://xn--bcher-kva.example/a")
        );
        assert_eq!(
            clean_link("tel:+1 (555)-123/", "Call").as_deref(),
            Some("tel:+1555123")
        );
        assert_eq!(
            clean_link("sms:123-456///", "Text").as_deref(),
            Some("sms:123-456")
        );
        assert_eq!(clean_link("javascript:alert(1)", "label"), None);
        assert_eq!(
            clean_link("https://evil.example", "https://good.example"),
            None
        );
        assert_eq!(clean_link("/relative", "label"), None);
        let doc = render("[https://good.example](https://evil.example)");
        assert_eq!(doc.text, "[https://good.example](https://evil.example)");
        assert!(doc.spans.iter().all(|s| s.target.is_none()));
        let normal = render("[**safe😄**](https://example.org/path)");
        assert_eq!(normal.text, "safe😄");
        assert!(
            normal
                .spans
                .iter()
                .all(|s| s.style.bold && s.target.is_some())
        );
    }
    #[test]
    fn source_context_flags_restrict_reply_blocks_and_standard_shortcode_catalog() {
        let standard = render("# heading\n:smile:");
        assert!(standard.spans.iter().any(|s| s.style.heading == 1));
        assert!(!standard.text.contains(":smile:"));
        let reply = document(
            &message("# heading"),
            &ClientState::default(),
            None,
            MessageContext::InlineReply,
        );
        assert_eq!(reply.text, "# heading");
        assert!(reply.blocks.is_empty());
        let preview = document(
            &message("# heading"),
            &ClientState::default(),
            None,
            MessageContext::InlinePreview,
        );
        assert_eq!(preview.text, "heading");
    }
    #[test]
    fn cache_reuses_source_but_invalidates_resolved_mentions_and_media() {
        let mut state = ClientState::default();
        state.users.insert(
            12.into(),
            User {
                id: 12.into(),
                username: "Before".into(),
                ..Default::default()
            },
        );
        let msg = message("<@12> <:party:123>");
        let mut cache = MessageCache::default();
        let first = cache.get(
            &msg,
            &state,
            Some("https://m.example"),
            MessageContext::Standard,
        );
        let same = cache.get(
            &msg,
            &state,
            Some("https://m.example"),
            MessageContext::Standard,
        );
        assert!(Arc::ptr_eq(&first, &same));
        state.users.get_mut(&12.into()).unwrap().username = "After".into();
        let changed = cache.get(
            &msg,
            &state,
            Some("https://m.example"),
            MessageContext::Standard,
        );
        assert!(!Arc::ptr_eq(&first, &changed));
        assert!(changed.text.starts_with("@After"));
        let media = cache.get(
            &msg,
            &state,
            Some("https://other.example"),
            MessageContext::Standard,
        );
        assert!(!Arc::ptr_eq(&media, &changed));
        assert_eq!(cache.parsed.len(), 1);
        cache.retain_visible(&BTreeSet::new());
        assert!(cache.documents.is_empty());
        cache.clear();
        assert!(cache.parsed.is_empty());
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn jumbo_uses_actual_parser_unicode_nodes_and_excludes_compact_and_inline_contexts() {
        let source = "👋🏽 👨‍👩‍👧‍👦 🇺🇸 🫱🏻‍🫲🏼 <:party:123>";
        let message = Message::text(100.into(), 10.into(), Default::default(), source);
        let mut state = ClientState::default();
        let full = document(&message, &state, None, MessageContext::Standard);
        assert!(full.jumbo_emojis);
        assert_eq!(full.source, source);
        assert!(!document(&message, &state, None, MessageContext::InlineReply).jumbo_emojis);
        assert!(!document(&message, &state, None, MessageContext::InlinePreview).jumbo_emojis);
        state.user_settings = serde_json::json!({"message_display_compact":true});
        assert!(!document(&message, &state, None, MessageContext::Standard).jumbo_emojis);
        assert!(!render(":unknown: words").jumbo_emojis);
        assert!(render(&"🙂 ".repeat(30)).jumbo_emojis);
        assert!(!render(&"🙂 ".repeat(31)).jumbo_emojis);
    }
    #[test]
    fn timestamps_keep_source_ranges_and_copy_visible_localized_labels_without_hidden_leaks() {
        let source = "é 🙂 <t:1700000000:D> ||<t:1700000001:T>|| 日本語";
        let doc = render(source);
        assert_eq!(doc.source, source);
        assert_eq!(doc.timestamps.len(), 2);
        let timestamp = &doc.timestamps[0];
        assert_eq!(&doc.text[timestamp.range.clone()], "<t:1700000000:D>");
        let label = crate::message_timestamp::format(
            timestamp.seconds,
            timestamp.style,
            &doc.timestamp_preferences,
            crate::message_timestamp::now(),
        );
        assert_eq!(
            doc.copy_range(timestamp.range.clone(), &BTreeSet::new()),
            label
        );
        assert_eq!(
            doc.copy_range(0..doc.text.len(), &BTreeSet::new()),
            format!("é 🙂 {label}  日本語")
        );
        assert_eq!(
            doc.timestamps[1].range.start,
            doc.text.find("<t:1700000001").unwrap()
        );
        let message = Message::text(100.into(), 10.into(), Default::default(), source);
        let compact = doc.with_author(&message);
        assert_eq!(
            &compact.text[compact.timestamps[0].range.clone()],
            "<t:1700000000:D>"
        );
    }
    #[test]
    fn ast_fallbacks_preserve_visible_content_and_report_remaining_presentation() {
        let doc = render("a | b\n--- | ---\n**one** | ||secret||\n\n<t:123:s>");
        assert!(doc.text.contains("one"));
        assert!(doc.text.contains("<t:123:s>"));
        assert!(
            !doc.copy_range(0..doc.text.len(), &BTreeSet::new())
                .contains("secret")
        );
        assert_eq!(doc.timestamps.len(), 1);
    }
}
