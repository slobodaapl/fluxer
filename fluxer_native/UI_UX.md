# Native UI/UX review — 2026-11-12

The native client needs the existing client's interaction hierarchy as well as
its features. The most widespread issue was treating every available command as
a filled, permanently visible button. That interrupted conversations, repeated
secondary actions, and made consequential controls harder to find.

This pass reviews the implemented surfaces against the checked-in Fluxer client.
It does not claim full visual, accessibility, or functional parity.

## Implemented in this pass

| Surface | Finding | Change |
|---|---|---|
| Message timeline | Every message reserved a row for React/Pin/Reply/Forward/Copy/Edit/Delete. | Absolute React/Reply/More toolbar on hover or keyboard focus, plus right-click/Shift+F10 menus. Actions no longer change message height. Permissions and command paths are preserved. |
| Message safety and navigation | Destructive actions competed with routine actions; keyboard reachability was inconsistent. | Delete lives in the menu and confirms unless Shift bypasses, as in Fluxer. Tab enters the timeline at its last row; Up/Down move through rows. Editable fields and compact controls are Tab reachable. Open menus block background send/read acknowledgements. |
| Header and composer | Search, pins, emoji, attachment, reply cancellation and send used competing text buttons. | Consistent compact SVG controls, tooltips, a quieter reply/edit tray and a single composer surface. Normal text selection, Enter/Shift+Enter and draft ownership are retained. |
| Search and pins | Each preview repeated a full Jump/Copy/Pin action bank. | Clickable preview header/background, corner Jump/More icons and the shared context menu. Message text remains selectable; actions do not cause accidental jumps. Query filters, paging and confirmations remain available. |
| Staged attachments | Repeated text actions and permanently visible upload policy competed with the draft. | Smaller cards, corner spoiler/remove/cancel/retry controls; progress and failures remain visible. Limits and preupload privacy information are available from Add files. Empty drafts have no attachment help block. |
| Voice/account dock | Disconnected calls still displayed Share/Leave alongside a separate account/sign-out block. | One compact identity/microphone/deafen/settings row; connection/share/disconnect controls appear when relevant. Reconnecting is explicit and Stop sharing remains accessible. Sign out is secondary in settings. |
| Voice participants | Large watch buttons competed with names and speaking state. | Compact participant cards and a quiet Watch affordance; roster and speaking/mute/live indicators are retained. |
| Voice/audio settings | Gain used minus/plus button banks; booleans used repeated text buttons. | Labeled sliders with drag, arrows, Home/End and visible numeric values; switches for DSP and mute options; device/mode selections remain dropdowns. Microphone testing remains an explicit action. |
| Stream viewer/share setup | Numerous text buttons competed with video. | Compact back/chat/expand/fullscreen/stop toolbar, actual volume slider and quieter share setup controls. Existing immediate local stop and media lifecycle are retained. |
| Navigation/errors | Rail initials were not consistently centered; sign out and Dismiss were overemphasized. | Centered rail artwork/initials, compact identity dock, secondary account action and a slim error banner with a close icon. |

Artwork is extracted from the same pinned `@phosphor-icons/react` 2.1.10 regular
icon set used by Fluxer, embedded into the executable with its MIT license.
Controls use consistent geometry and named tooltips rather than platform emoji.

## Source references

- `fluxer_app/src/features/channel/components/MessageActionBar.tsx` and
  `MessageActionBar.module.css`: absolute contextual message actions, visibility,
  spacing and icon hierarchy.
- `fluxer_app/src/features/channel/components/MessageActionUtils.tsx`: command
  availability and Shift-bypassed delete confirmation.
- `fluxer_app/src/features/ui/action_menu/ContextMenuIcons.tsx`: existing artwork
  and menu vocabulary.
- `fluxer_app/src/features/channel/components/` and
  `fluxer_app/src/features/search/`: message previews, pins and attachment
  interaction patterns.
- `fluxer_app/src/features/voice/`: connection, participant, stream and volume
  controls. The native gain scale remains its existing linear 0–2 range;
  Fluxer's nonlinear boost curve has not been migrated in this UI pass.

## Next priorities

1. Message grouping, date separators, inline markdown/custom emoji, reply context,
   image presentation and density settings. Removing buttons improves scanning;
   the remaining message presentation still needs source-level parity.
2. A unified modal/popover system: focus trapping, dismissal, anchoring, screen
   bounds, accessible names and consistent confirmations across all surfaces.
3. Responsive behavior at short/minimum window sizes, larger text, long names,
   many participants, multiple streams and moves between monitors. A blank-content
   redraw was observed after moving the offline preview between outputs; this
   remains an explicit review item. Navigation should remain reachable
   without crowding the conversation.
4. Settings navigation and full preference migration. Current voice settings are
   clearer, but the rest of Fluxer's settings, locale/hour-cycle preferences,
   screen-reader semantics and broader accessibility remain incomplete.
5. Member/profile and channel/guild context menus, unread/mention hierarchy,
   quick switching, inbox and remaining feature areas in `parity.json`.

The full local gate passes 326 tests, strict workspace Clippy, formatting and the
47-area source inventory. Offline Wayland checks verified actual SVG rendering,
hover and right-click menus, Delete/Pin cancellation after Tab wrapping, and
slider dragging, arrow adjustment and range limits. These checks opened no real
account mutations or microphone capture.

Real-account, device, keyboard and subjective acceptance checks are recorded in
[REVIEW.md](REVIEW.md). Local fixture evidence is recorded separately from user
acceptance; the user being away is not a manual pass.
