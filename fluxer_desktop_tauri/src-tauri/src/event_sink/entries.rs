// SPDX-License-Identifier: AGPL-3.0-or-later

//! What fluxer_desktop/src/main/VoiceDebugEventSinkPopout.ts kept for the event sink window:
//! the last 1 000 normalised entries, the last stats snapshot, and the entries waiting for the
//! next 50 ms flush to the page. Lengths are JavaScript string lengths (UTF-16 units).

use serde::Serialize;
use serde_json::Value;

use crate::js::utf16_length;

const MAX_ENTRIES: usize = 1000;
const MAX_PENDING_ENTRIES: usize = 1000;
const MAX_LINE_CHARS: usize = 262_144;
const MAX_STATS_HTML_CHARS: usize = 1_048_576;
/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `DesktopVoiceDebugEventSinkEntry`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    sequence: u64,
    line: String,
}

/// The first `max` UTF-16 units of `text` and the count of units cut, or `None` if it fits.
/// `String.prototype.slice` can split a surrogate pair and keep the lone high half; a Rust
/// string cannot hold one, so the cut falls before the pair instead.
fn truncated(text: &str, max: usize) -> Option<(&str, usize)> {
    let length = utf16_length(text);
    if length <= max {
        return None;
    }
    let mut units = 0;
    let end = text
        .char_indices()
        .find(|(_, character)| {
            units += character.len_utf16();
            units > max
        })
        .map_or(text.len(), |(index, _)| index);
    Some((&text[..end], length - max))
}

/// `truncateLine`.
fn truncate_line(line: &str) -> String {
    match truncated(line, MAX_LINE_CHARS) {
        Some((kept, omitted)) => format!("{kept}... [truncated {omitted} chars]"),
        None => line.to_owned(),
    }
}

/// `Number.isSafeInteger(sequence) && sequence >= 1`.
fn sequence(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    if number.fract() != 0.0 || !(1.0..=MAX_SAFE_INTEGER).contains(&number) {
        return None;
    }
    // Exact: a safe integer converts without loss.
    Some(number as u64)
}

/// `normalizeEntry`.
fn normalize_entry(value: &Value) -> Option<Entry> {
    let record = value.as_object()?;
    Some(Entry {
        sequence: sequence(record.get("sequence")?)?,
        line: truncate_line(record.get("line")?.as_str()?),
    })
}

/// `normalizeEntries`: the last 1 000 values, keeping the valid ones.
pub fn normalize_entries(value: &Value) -> Vec<Entry> {
    let Some(values) = value.as_array() else {
        return Vec::new();
    };
    values[values.len().saturating_sub(MAX_ENTRIES)..]
        .iter()
        .filter_map(normalize_entry)
        .collect()
}

/// `normalizeStatsHtml`.
pub fn normalize_stats_html(value: &Value) -> String {
    let Some(html) = value.as_str() else {
        return String::new();
    };
    match truncated(html, MAX_STATS_HTML_CHARS) {
        Some((kept, omitted)) => format!("{kept}<p>[truncated {omitted} chars]</p>"),
        None => html.to_owned(),
    }
}

fn keep_last(entries: &mut Vec<Entry>, max: usize) {
    let excess = entries.len().saturating_sub(max);
    entries.drain(..excess);
}

/// `webContents.isLoadingMainFrame()` of the event sink window.
#[derive(Clone, Copy)]
pub struct Loading(pub bool);

#[derive(Debug, PartialEq)]
pub enum Flush {
    Idle,
    /// The page is loading; the timer runs again.
    Reschedule,
    /// `__fluxerVoiceDebugEventSinkAppendEntries` gets these.
    Deliver(Vec<Entry>),
}

#[derive(Default)]
pub struct SinkState {
    entries: Vec<Entry>,
    stats_html: String,
    /// `pendingPopoutEntries`.
    pending: Vec<Entry>,
    /// `flushTimer` is set.
    flush_scheduled: bool,
}

impl SinkState {
    /// `replaceEventSinkEntries`.
    pub fn replace(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        keep_last(&mut self.entries, MAX_ENTRIES);
        self.pending.clear();
    }

    /// `appendEventSinkEntries`.
    pub fn append(&mut self, entries: &[Entry]) {
        self.entries.extend_from_slice(entries);
        keep_last(&mut self.entries, MAX_ENTRIES);
    }

    /// `queueEntriesForPopout` while the window exists. True when a flush must be scheduled.
    pub fn queue(&mut self, entries: Vec<Entry>) -> bool {
        if entries.is_empty() {
            return false;
        }
        self.pending.extend(entries);
        keep_last(&mut self.pending, MAX_PENDING_ENTRIES);
        !std::mem::replace(&mut self.flush_scheduled, true)
    }

    /// `flushPendingEntriesToPopout`, run when the flush timer fires. `window` is whether the
    /// window exists and, if so, whether its page is loading.
    pub fn flush(&mut self, window: Option<Loading>) -> Flush {
        self.flush_scheduled = false;
        match window {
            None => Flush::Idle,
            Some(_) if self.pending.is_empty() => Flush::Idle,
            Some(Loading(true)) => {
                self.flush_scheduled = true;
                Flush::Reschedule
            }
            Some(Loading(false)) => Flush::Deliver(std::mem::take(&mut self.pending)),
        }
    }

    /// `sendSnapshotToPopout`: the pending entries are dropped, the page gets everything.
    pub fn snapshot(&mut self) -> (Vec<Entry>, String) {
        self.pending.clear();
        (self.entries.clone(), self.stats_html.clone())
    }

    pub fn set_stats_html(&mut self, html: String) -> &str {
        self.stats_html = html;
        &self.stats_html
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(sequence: u64) -> Entry {
        Entry {
            sequence,
            line: format!("line {sequence}"),
        }
    }

    fn entries(range: std::ops::RangeInclusive<u64>) -> Vec<Entry> {
        range.map(entry).collect()
    }

    fn sequences(entries: &[Entry]) -> Vec<u64> {
        entries.iter().map(|entry| entry.sequence).collect()
    }

    #[test]
    fn entries_need_a_safe_positive_integer_sequence_and_a_string_line() {
        let normalized = normalize_entries(&json!([
            {"sequence": 1, "line": "a"},
            {"sequence": 2.0, "line": "b"},
            {"sequence": 9_007_199_254_740_991_u64, "line": "c"},
            {"sequence": 9_007_199_254_740_992_u64, "line": "too large"},
            {"sequence": 0, "line": "zero"},
            {"sequence": -1, "line": "negative"},
            {"sequence": 1.5, "line": "fraction"},
            {"sequence": "3", "line": "string sequence"},
            {"sequence": 4, "line": 4},
            {"sequence": 5},
            [6, "array"],
            null,
            "text",
        ]));
        assert_eq!(
            normalized,
            vec![
                Entry {
                    sequence: 1,
                    line: "a".into()
                },
                Entry {
                    sequence: 2,
                    line: "b".into()
                },
                Entry {
                    sequence: 9_007_199_254_740_991,
                    line: "c".into()
                },
            ]
        );
        assert!(normalize_entries(&json!({"sequence": 1, "line": "a"})).is_empty());
        assert!(normalize_entries(&Value::Null).is_empty());
    }

    #[test]
    fn only_the_last_thousand_values_are_read() {
        let mut values: Vec<Value> = (1..=1001)
            .map(|sequence| json!({"sequence": sequence, "line": "x"}))
            .collect();
        assert_eq!(normalize_entries(&Value::from(values.clone())).len(), 1000);
        // Invalid values among the last thousand are not made up from earlier ones.
        values[1000] = Value::Null;
        let normalized = normalize_entries(&Value::from(values));
        assert_eq!(normalized.len(), 999);
        assert_eq!(normalized[0].sequence, 2);
    }

    #[test]
    fn long_lines_and_stats_are_cut_with_the_count_of_cut_units() {
        let line = "a".repeat(262_144 + 10);
        let normalized = normalize_entries(&json!([{"sequence": 1, "line": line}]));
        assert_eq!(
            normalized[0].line,
            format!("{}... [truncated 10 chars]", "a".repeat(262_144))
        );
        let fits = "a".repeat(262_144);
        let normalized = normalize_entries(&json!([{"sequence": 1, "line": fits}]));
        assert_eq!(normalized[0].line, fits);

        let html = "b".repeat(1_048_576 + 3);
        assert_eq!(
            normalize_stats_html(&json!(html)),
            format!("{}<p>[truncated 3 chars]</p>", "b".repeat(1_048_576))
        );
        assert_eq!(normalize_stats_html(&json!("<p>x</p>")), "<p>x</p>");
        assert_eq!(normalize_stats_html(&json!(7)), "");
        assert_eq!(normalize_stats_html(&Value::Null), "");
    }

    #[test]
    fn lengths_are_counted_in_utf16_units() {
        // 262 144 units of two-unit characters fit exactly; one more unit does not.
        let pairs = "\u{1f600}".repeat(131_072);
        let normalized = normalize_entries(&json!([{"sequence": 1, "line": pairs}]));
        assert_eq!(normalized[0].line, pairs);
        let over = format!("{pairs}a");
        let normalized = normalize_entries(&json!([{"sequence": 1, "line": over}]));
        assert_eq!(
            normalized[0].line,
            format!("{pairs}... [truncated 1 chars]")
        );
        // A pair straddling the limit is cut before the pair.
        let straddling = format!("a{pairs}");
        let normalized = normalize_entries(&json!([{"sequence": 1, "line": straddling}]));
        assert_eq!(
            normalized[0].line,
            format!("a{}... [truncated 1 chars]", "\u{1f600}".repeat(131_071))
        );
    }

    #[test]
    fn stored_entries_keep_the_last_thousand() {
        let mut state = SinkState::default();
        state.replace(entries(1..=1200));
        assert_eq!(state.snapshot().0.len(), 1000);
        assert_eq!(state.snapshot().0[0].sequence, 201);
        state.append(&entries(1201..=1205));
        let (stored, _) = state.snapshot();
        assert_eq!(stored.len(), 1000);
        assert_eq!(
            sequences(&stored[995..]),
            vec![1201, 1202, 1203, 1204, 1205]
        );
        assert_eq!(stored[0].sequence, 206);
    }

    fn delivered(flush: Flush) -> Vec<u64> {
        match flush {
            Flush::Deliver(entries) => sequences(&entries),
            other => panic!("expected a delivery, got {other:?}"),
        }
    }

    const LOADED: Option<Loading> = Some(Loading(false));

    #[test]
    fn queued_entries_flush_once_per_timer_and_cap_at_a_thousand() {
        let mut state = SinkState::default();
        assert!(!state.queue(Vec::new()));
        assert!(state.queue(entries(1..=600)));
        // The timer is already running.
        assert!(!state.queue(entries(601..=1100)));
        let flushed = delivered(state.flush(LOADED));
        assert_eq!(flushed.len(), 1000);
        assert_eq!(flushed[0], 101);
        assert_eq!(state.flush(LOADED), Flush::Idle);
        assert!(state.queue(entries(1101..=1101)));
    }

    #[test]
    fn a_loading_page_defers_the_flush_and_a_missing_window_skips_it() {
        let mut state = SinkState::default();
        state.queue(entries(1..=2));
        assert_eq!(state.flush(Some(Loading(true))), Flush::Reschedule);
        // The rescheduled timer covers entries queued meanwhile.
        assert!(!state.queue(entries(3..=3)));
        assert_eq!(delivered(state.flush(LOADED)), vec![1, 2, 3]);
        state.queue(entries(4..=4));
        assert_eq!(state.flush(None), Flush::Idle);
        // Electron left them pending; the next window's snapshot drops them.
        assert!(state.queue(entries(5..=5)));
        assert_eq!(delivered(state.flush(LOADED)), vec![4, 5]);
    }

    #[test]
    fn a_snapshot_or_replacement_drops_pending_entries() {
        let mut state = SinkState::default();
        state.replace(entries(1..=2));
        state.queue(entries(3..=4));
        state.set_stats_html("<p>s</p>".into());
        let (stored, stats) = state.snapshot();
        assert_eq!(sequences(&stored), vec![1, 2]);
        assert_eq!(stats, "<p>s</p>");
        assert_eq!(state.flush(LOADED), Flush::Idle);
        state.queue(entries(5..=5));
        state.replace(entries(9..=9));
        assert_eq!(state.flush(LOADED), Flush::Idle);
    }
}
