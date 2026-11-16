# Rich block lane

Source reviewed: `fluxer_app/src/features/theme/styles/Markup.module.css` (heading scale, nested-list markers and indentation, alert rule) and `fluxer_app/src/features/messaging/components/markdown/renderers/common/BlockElements.tsx` (list and alert rendering). The existing Rust parser remains the source of block structure.

Implemented: heading font scales and variable-height visual rows; row geometry is shared by drawing, selection, pointer hit testing, and inline emoji. Lists carry nesting depth and use distinct unordered markers and indentation. Alert blocks retain their type and draw a type-colored rule. Message activation exposes the target glyph bounds in window coordinates for both pointer and keyboard activation. Copy Message still uses original Markdown; selection copy uses the existing spoiler-aware projected text.

Limitations: tables still project as tab-delimited text, without cell layout, horizontal scrolling, or table-aware selection. Fenced code is monospaced with a background, but has no syntax highlighting, language header, or per-block Copy button. Alert icons, localized labels, and vertical card padding are not implemented. Nested ordered lists do not yet use alphabetic/roman markers, and heading margins are not yet matched. These are not claimed as parity.

Automated tests cover existing Unicode, spoilers, emoji, clipping, and compact author behavior plus nested-list and alert range/copy projection. GUI rendering, selection across variable-height wrapped headings, popover anchoring, and narrow window behavior need manual inspection in an offline demo; this lane did not launch an app or capture screenshots.
