# Chat parity push — paused 2026-11-16

Work is paused at the user's explicit request. Do not resume implementation,
build, deployment or agents without a later resume request. Source branch is
`yolo/gpui`; source HEAD before this record is `69d4ad671`.

## Current delivery state

The main branch integrates measured heading/list/block geometry, real table
layout and scrolling, pinned Arborium 2.18.2 highlighting with a bounded FIFO
worker/cache epoch, language and code Copy controls, user/role mention activation,
full glyph-bound popup placement, bounded mention metadata, shared overlay
geometry/focus helpers, emoji sizing/animation, localized timestamp tokens, and
initial inline keyboard target traversal. These are in-progress integrations,
not a completed or accepted feature push.

The installed live application is still **29a36327b-local**, the earlier expansion
build, running under GDB/Vulkan validation. It was left untouched. The owned
`--demo` QA application was closed. Agents are interrupted/completed and no Cargo
process remains running. Do not install/relaunch this source before resolving
the known integration bugs and completing validation.

## Integrated commits

- Rich foundation `4e859c1d3` → `a300d037c`; phase 2 `4f43de519` →
  `82561745a`; alert/heading corrections → `bf41bb65d`, `9cd15166d`.
- Mention lane `9aa8ee6fc`, `2e51ed38c` → `26548d2f0`, `116a064cd`;
  root bounds/lifetime integration `ff4a87616`, `80db96efb`, `d00d86c9d`.
- Overlay helpers `d795abb7c`; private context reset `dc79171e0`, `70365d0ab`
  (the latter context behavior needs correction, below).
- Emoji/time feature `b1d177aaf` → `73f393974`, manually merged with real
  table tokens, cell line heights, code controls and heading baselines.
- Highlight FIFO `d82efed96` → `a286a0e0b`.
- Root inline target keyboard navigation `69d4ad671`.

## Unmerged work and stopped agents

Worktrees are under `target/worktrees/`; actual HEADs, dirty paths and patch
snapshots are saved in `target/qa/chat-parity-2026-11-16/paused-worktrees.json` and
`*-paused.patch`.

- Terra `/root/chat3_rich_blocks`, `chat3-rich-blocks`, `chat3/rich-blocks`:
  **`0e7921886` remains unmerged**, fixing blank/padded table-cell hit testing
  with per-cell byte ranges. Later uncommitted work may contain scrollbar
  outside-release fixes, two-second Copied feedback and list hanging indents;
  inspect the saved tree/diff before resuming. Agent interrupted.
- Sol `/root/chat3_emoji_time`, `chat3-emoji-time`, `chat3/emoji-time`:
  **`1d612466b` remains unmerged**, keeping Unicode literal in inline/fenced
  code. Merge the new `inline_emoji_height` helper into real table rows and
  preserve heading `font_size * 1.2` baseline. Agent complete.
- Terra `/root/chat3_mentions` delivered mentions and reviews, then was working
  in **`chat3-focus`, `chat3/focus`**, based on `69d4ad671`, on offscreen keyboard
  focus scrolling. Agent interrupted; inspect dirty changes before continuing.

## Known blockers — first work on resume

1. **Repeated READY/context reset regression:** worker emits SessionContext on
   every READY (`client/src/worker.rs` around 2789). Root currently calls
   `reset_conversation_surfaces` unconditionally there, closing search/profiles,
   dropping reply/forward/outbox/media UI on reconnect. Gate resets on an actual
   account/instance boundary; preserve same-context reconnect work. The planned
   context identity correction was analysis only, not implemented.
2. **Account-scoped private state:** direct A→B with the same numeric channel ID
   still can retain search query, composer draft/edit state, pending messages and
   staged attachments. Clear/reload these on a confirmed boundary even if channel
   ID is unchanged. Draft restoration must use the new account's authoritative
   state, and must avoid the composer observer sending an empty SetDraft into the
   new worker before that state arrives. Do not blanket-clear on every READY.
3. **Offscreen keyboard focus:** Tab can select a target outside the clip and
   Enter can fall back to the mouse position. `chat3-focus` was addressing this.
   Search's GPUI List consumes `window.request_autoscroll` during prepaint;
   the timeline's ScrollHandle Div does not. Reveal the actual target vertically
   and horizontally within a scrolled table, only on keyboard intent.
4. Merge code Unicode and table hit-test fixes listed above. The table blank-cell
   case can otherwise select the neighboring column in asymmetric wrapped rows.
5. **Timestamp limits and clipping:** timestamp widgets cap `.take(128)` before
   visibility, so later visible relative timestamps have no timer/tooltip. Apply
   cap after clipping. Atomic long date labels can exceed narrow cell widths and
   paint/hit-test into neighboring columns; choose suitable intrinsic column
   widths or clip the cell consistently, including tooltip/hit boxes.
6. **Scrollbar release:** releasing table drag outside the message may leave
   table_drag active. Clear it on outside release or when no left button is down.
   Rich agent may have an unfinished fix in its dirty tree.
7. Finish/check Copied feedback (source uses two seconds), hanging list markers
   on wrapped rows, and remaining alert/heading presentation. Explicitly document
   residual scope (e.g. alert localization/icons) rather than claiming parity.

## Validation so far

The prior installed expansion had a full 638-pass/14-ignored gate. **That is not
validation of this push.** Agent lanes report focused message/Arborium/queue
checks and an isolated 280-pass/1-ignored desktop emoji gate. Root overlay
geometry test passed. Root integration builds through `qa-build4` succeeded, but
latest keyboard helper/test edits and pending fixes have no final combined gate.

Owned pointer GUI checks passed resolved user/role activation, exact adjacent-gap
behavior, inert unknown/code mentions, Escape and initial popup placement. Full
merged emoji/timestamp, table/clipboard/scroll/keyboard and narrow-window checks
remain. Captures/logs: `target/qa/chat-parity-2026-11-16/` and
`/tmp/fluxer-chat3-*.log`. Existing QA helpers are `/tmp/fluxer-push-ui/qa.py` and
`resume_qa.py`, exact title `Fluxer · Native · push-qa`.

## Build and preservation rules

All Cargo operations use `/tmp/fluxer-push-cargo` with an explicit worktree
`--manifest-path .../fluxer_native/Cargo.toml --locked`; it serializes the shared
**disk-backed** `/tmp/fluxer-gpui-target`. Do not cargo clean. Rich lane downloaded
and pinned Arborium 2.18.2 grammar dependencies; keep their attribution/declared
licenses in release review. Source highlighting matches the checked-in Fluxer
version and uses native captures rather than an HTML parser.

Root's original uncommitted audio/DSP work remains present and unstaged. Do not
commit or discard it. Preexisting patches, file hashes, untracked tar and retained
integration stash IDs are in `target/qa/chat-parity-2026-11-16/`. Cargo.lock now
contains committed Arborium dependencies plus the original uncommitted DSP graph.
Selective staging is required for main.rs and Cargo.lock. No real-account
messages/settings/call joins/capture were used for this push's checks. The media
resource budget (#2) and multi-stream acceptance remain deferred by the user.
