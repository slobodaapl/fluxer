# Chat parity push — integrated 2026-11-16

The user resumed the paused push. All delivered Terra/Sol lanes and the subsequent
integration fixes are merged into `yolo/gpui`. This is an integrated chat-rendering
increment; the overall native migration and live/manual acceptance remain open.
The independent audio/DSP working-tree changes were preserved and remain unstaged.

## Delivered behavior

- Source-backed heading sizing and spacing, nested list markers with hanging
  continuation lines, and variable-height selection/layout. The shared Fluxer
  parser supports heading levels 1–4; five/six hashes remain literal.
- Real aligned table cells with wrapping, measured timestamp column minimums,
  horizontal scrolling/thumbs, cell-first padded/empty-area hit testing, and
  outside-release drag cleanup. Long timestamp labels wrap atomically in prose
  and widen their table column instead of painting over neighbors.
- Arborium 2.18.2 native syntax highlighting, matching the checked-in web client,
  language headers, exact code Copy and two-second Copied feedback. Unicode emoji
  inside code stay literal. Highlight work uses one FIFO worker, bounded admission,
  bounded cache, stale-input cancellation and account-boundary cache invalidation.
- User mentions open the existing scoped profile card. Resolved role mentions
  open a small native card with role color/name and an honest partial member count.
  The latter is a native extension: the existing Fluxer role mention is inert.
  Unknown mentions and mentions inside code/concealed spoilers remain inert.
- Full glyph bounds anchor mention/profile cards; shared overlay geometry/focus
  helpers improve edge placement and Escape behavior. Tab/Shift-Tab traverse inline
  targets; keyboard focus reveals offscreen targets vertically and inside wide
  tables before Enter can activate them. A GUI-discovered redraw bug was fixed
  with explicit follow-up frames for table and timeline scroll changes.
- Source top-level jumbo eligibility (1–30 emoji), 48px jumbo versus inline sizing,
  static/animated custom emoji and motion settings. Large Unicode art now selects
  the 64px sheet based on its rendered pixel footprint even on a 1× display.
- All nine timestamp styles use account locale/time format; visible relative
  timestamps share a bounded one-shot clock. Widget limits apply after visibility,
  preserving timers/tooltips for tokens beyond the first 128 source tokens.
- Repeated READY events preserve drafts and conversation UI. Actual account or
  instance changes clear private UI/cache state and reload authoritative drafts,
  including when both accounts have the same numeric channel ID. Programmatic
  composer updates do not write an empty draft into the new account.

## Validation

The final integrated `scripts/verify.sh` passes the parity audit, formatting,
strict workspace Clippy and **674 tests: 0 failures, 14 opt-in ignored**. This gate
includes the preexisting local DSP changes as well as the committed chat push.
The real worker regression checks READY identity ordering and reconnect draft
preservation. Final evidence is in `target/qa/chat-parity-2026-11-16/`, including
`integrated-verify.log`, `ready-reconnect-regression.log`, `results.json` and PNGs.

Owned Wayland demo checks verified heading/list layout, table cell separation and
long-date columns, exact Unicode code clipboard bytes/trailing newline and Copied
reset, horizontal thumb release outside the message, animated WebP changing frames,
paused emoji and scroll return, 30/31 jumbo thresholds, French/24-hour labels,
a tooltip on timestamp row 250, and keyboard reveal/first-Enter activation of an
otherwise offscreen eighth-column role and the following user profile at 880×720.
No live messages, account-setting mutations, call joins or capture were performed.

The installed application is **375faae7b-local**, relaunched after the final gate
under GDB/Vulkan validation. Its authenticated session and selected community/
channel restored; the audio-preference file is unchanged. Build/launch evidence
is recorded in `results.json` and the live debugger directory pointed to by
`/tmp/fluxer-live-debug-dir`. The build includes the preserved local DSP work.
Intentional debugger SIGINT events during setup are recorded as such.

## Review queue and limits

See the chat-push entry in [REVIEW.md](REVIEW.md). Real Fluxer messages, animated
community emoji/CDN-only Unicode art, subjective spacing, extensive selection and
keyboard combinations, font-size settings, IME/accessibility, real account-switch
and reconnect behavior, and prolonged hidden-window timestamp behavior still need
manual acceptance. The offline fixture has no static-CDN endpoint, so a known
mixed-skin-tone emoji absent from bundled sheets is blank there; this fixture does
not establish live CDN-art acceptance. Linux ICU localization is exercised;
other-platform fallback behavior remains a portability gap.

Alert icons/localization, rich profile biography rendering, the full profile modal,
and complete markdown/accessibility parity remain migration work. Attribution and
the release-packaging gap for Arborium's language grammars are documented in
[THIRD_PARTY.md](THIRD_PARTY.md); this is not a distributable-release sign-off.
The aggregate media resource budget (#2) and multi-stream acceptance stay deferred.

## Worktree/preservation record

The original pause snapshots, untracked-file archive, retained integration stash
IDs and worktree patches remain in the QA directory. Delivered table/code/focus/
timestamp follow-ups formerly listed as unmerged are now integrated. Subsequent
Clippy, redraw and attribution agent work is also merged. Agent worktrees remain
available for inspection; no worktrees or saved WIP were removed.

All Cargo operations use the serialized `/tmp/fluxer-push-cargo` or equivalent
build lock and the disk-backed `/tmp/fluxer-gpui-target`. Do not cargo clean or
stage unrelated audio/DSP changes. `Cargo.lock` contains committed Arborium
changes plus the original uncommitted DSP dependency graph.
