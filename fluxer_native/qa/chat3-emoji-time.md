# Chat 3 emoji and timestamp presentation

Implemented in the isolated `chat3/emoji-time` worktree; no installed binary, live account/call, media/audio WIP, or root working-tree edits.

## Checked-in source contracts

- `fluxer_app/src/features/messaging/utils/markdown/JumboDetector.ts`: 1–30 top-level emoji plus whitespace; compact disabled. `renderers/index.tsx`: jumbo only in standard-with-jumbo context. Native counts catalog Unicode graphemes when the Rust parser's shared shortcode context leaves them as Text; the web EmojiProvider supplies per-message Unicode ranges.
- `globals.css`: normal emoji 1.5em, jumbo 3rem; `Markup.module.css`: restricted inline defaults 1.25em. Fixed jumbo image fragments preserve normal account-font whitespace and byte-indexed selection. Compact/reply/preview contexts do not become jumbo.
- Deliberate exclusion approved during implementation: the web detector's unknown-shortcode Text-prefix regex can flag `:unknown: words` as jumbo. Native retains ordinary fallback text and does not enlarge such a row.
- `EmojiDetector.ts`, `EmojiRenderer.tsx`, `CustomEmojiImageUrl.ts`: registered names/animation metadata override token values; URL rungs 128/240. `useShouldAnimate.ts`: emoji autoplay is independent of GIF autoplay and interaction can enable animation under reduced motion. Native account `animate_emoji`, existing native reduced-motion preference, hover, concealment, and actual ancestor clipping gate playback. Native does not invent a user-settings field for the web-only local “keep emoji under reduced motion” override.
- `TimestampRenderer.tsx`, markdown `DateFormatter.ts`, `packages/date_utils/src/DateTimestampStyle.ts`, user `DateFormatting.ts`, `LocaleUtils.ts`: all nine styles t/T/d/D/f/F/s/S/R, locale, AUTO/12h/24h setting, full seconds tooltip, and relative labels. Display uses source fixed 365/30/7-day thresholds with floor; tooltip uses numeric calendar month/year/day units (Luxon's tooltip behavior), without week substitution.
- Native locale grammar/order comes from system ICU/CLDR date-pattern and relative-date APIs. Linux dynamic symbol loading accepts versioned runtime libraries as well as development symlinks; formatter objects never escape their call/thread. [ICU date format documentation](https://unicode-org.github.io/icu/userguide/format_parse/datetime/) and [relative-date C API](https://unicode-org.github.io/icu-docs/apidoc/released/icu4c/ureldatefmt_8h.html) confirm skeleton/auto-versus-numeric behavior. No new Cargo dependency/download.

## Ranges, visibility, and lifetime

Timestamp source markers remain in document text/source. Atomic display-token projection maps shaped labels to original UTF-8 boundaries for wrapping, selection, caret and hit testing; selection Copy uses formatted visible labels. Copy Message still uses literal original source. Spoilers neither reveal image/timestamp widgets nor copy concealed labels. Compact author prefix shifts timestamp ranges too.

Only clipped-visible custom emoji lay out an Img, with stable source-revision/position ID so GPUI advances GIF/WebP frames. Each input owns its decoded image cache; after every prepaint it evicts sources whose boxes no longer intersect viewport and ancestor content mask. At most 64 custom image boxes are rendered; visible emoji may retain static/animated variants (at most 128 resource keys). Spoiler concealment, document revision, input release, and scroll visibility loss release associated resources. Re-entering a row loads again through GPUI's normal completion notification.

Relative timestamp refresh is one one-shot timer per window, independent of token/message count. Only unconcealed relative token boxes intersecting both the ancestor content mask and viewport schedule/re-arm it. Scrolling away allows at most the already pending final refresh; no recurring offscreen clock remains. Static timestamp labels have no repeating clock. Formatted labels use a bounded 512-entry cache.

## Verification

Commands use `/tmp/fluxer-push-cargo` (serialized shared disk cache), explicit isolated-tree manifest, `--locked`, and offline cache. No cargo clean.

- Emoji-focused gate: 13 passed, including real animated lossless WebP decode with four distinct 160ms frames.
- Formatter gate: 2 passed, including English/French, locale-specific relative grammar, floor thresholds, and account 12/24-hour preference.
- AST/document gate: 12 passed, including Unicode/ZWJ/skin-tone 30/31 boundary, context/compact exclusions, timestamp selected-copy, source copy, spoilers and compact-prefix offsets.
- Full desktop gate before final layout refinement: 280 passed, 1 ignored. Final gate is recorded in handoff.
- `cargo check` passes after fixed-width jumbo fragment refinement.
- Existing GPUI/LiveKit dependency warnings remain; no new crate dependencies.

## Explicit offline GUI fixture

Run the review binary with `--demo --demo-emoji-time`. This flag is required in addition to `--demo`; normal/live sessions never start the fixture server. It generates a real four-frame animated WebP served only on `127.0.0.1`, with 4096-byte request cap, one-second IO timeouts, 256-request limit and 30-minute lifetime. Only the two synthetic emoji routes are accepted; static requests return a still WebP.

Optional flags: `--demo-emoji-paused` (hover-only animation), `--demo-timestamp-fr`, `--demo-timestamp-24h`. Rows include Unicode family/flag/skin-tone/joined hands, mixed static/animated custom emoji, concealed animation, 30/31 counts, formatted/nonjumbo text, all nine timestamp styles, concealed/invalid timestamps, and a reply preview.

Validation: final offline desktop gate passed **280 tests, 1 ignored**, with the explicit locked worktree manifest through `/tmp/fluxer-push-cargo`; `cargo check` and `git diff --check` passed. Logs: `/tmp/chat3-emoji-time-final-tests.log` and `/tmp/chat3-emoji-time-check.log`.

Root owns GUI validation and integrated binary build. Check multiple animated frames, pause/resume on scroll and concealment, no loading forever after returning, bitmap sizes/spacing at changed account fonts and display scale, timestamp tooltip/copy, French/24-hour settings, and relative refresh while scrolling offscreen. No GUI execution is claimed by this worktree task.

## Literal code follow-up

`renderers/common/CodeElements.tsx` renders code content as literal text. Inline and fenced code Unicode therefore bypass emoji image substitution and emoji-driven line-height expansion. Composer fallback remains enabled, while message projection consumes only its explicit eligible replacement tokens. The same non-code eligibility gates token generation, Unicode bitmap painting, custom-image painting, and row-height contribution. Ordinary table/body Unicode continues to use bundled Fluxer artwork. Selection and source copy preserve the original Unicode bytes.

Follow-up validation: all 38 `input::` tests passed through the locked offline worktree manifest (`/tmp/chat3-code-unicode-input-tests.log`). Regression tests inspect actual inline/fenced AST code spans beside ordinary body/table emoji, exact 16px→24px ordinary emoji height contribution, absent code height contribution/tokens, literal message projection, composer fallback, explicit message token round-trips, and unchanged source/copy.

## Limits

System ICU/CLDR versions can differ in punctuation from the web runtime's ICU version. Without system ICU (and on non-Linux targets), formatting falls back to local ISO-style English; full locale parity is provided on this Linux target. Browser-locale AUTO override is a web local accessibility setting; native AUTO uses account locale. Existing profile bio rendering loops spans outside TextInput and does not render its custom emoji placeholders; converting that separate widget is outside the main chat/reply/search scope. Existing unsupported table/alert markers are owned by the rich-blocks integration.
