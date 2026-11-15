# Expanded native settings

Source references:

- `fluxer_app/src/features/user/state/UserSettings.ts`, `packages/schema/src/domains/user/UserRequestSchemas.ts`, and `fluxer_api/src/api/user/services/UserAccountSettingsService.ts`: `message_display_compact` and `gif_auto_play` are account booleans, sent as partial PATCHes to `/users/@me/settings`.
- `packages/schema/proto/fluxer/user/preferences/v1/preferences.proto`: accessibility is field 1 in `synced_preferences`; `accessibility.proto` stores optional double font size at field 12. `Accessibility.ts` defines 8–48px limits and a 16px default. Native uses a bounded wire reader/editor so unrelated and unknown preferences survive an edit. Invalid serialized settings prevent a write.
- `fluxer_app/src/features/accessibility/state/Accessibility.ts`: motion preferences are device-local (`Accessibility:motion`). Native stores its local reduced-motion override alongside native in-app keybinds at `$XDG_CONFIG_HOME/fluxer-native/interface.json` (fallback `$HOME/.config`). `useShouldAnimate.ts` permits deliberate interaction while automatic GIF playback is disabled by reduced motion.
- Existing native `controls.rs` supplies compact toggles, slider, keyboard focus and tooltip presentation.

Implemented behavior:

- Appearance: compact timeline mode hides avatars, shows timestamps and inline authors, removes intergroup space, preserves date separators/replies/media. Account text size applies to actual selectable message text and line height; the slider has a sample preview and explicit Apply button.
- Messages & media: account GIF autoplay toggle affects chat and search image resources. Accessibility: local reduced motion takes priority over autoplay without rewriting the account setting. Hover/open behavior stays under the existing animated-image component.
- Keybinds: record shortcuts for switcher/search/settings/microphone/deafen. The root capture handler dispatches configured native actions, superseding the old static registrations. Changes persist atomically, reset works, failed disk saves retain active values. Plain typing keys, standard edit/close/quit shortcuts, Tab, Enter, F8 and conflicts are rejected; Escape cancels recording. These are in-app shortcuts. OS-global push to talk is not advertised.
- Account writes have their own pending/error state and worker generation, independent of presence mutations. Optimistic projections roll back on failure only if the rejected value is still current. Logout aborts and invalidates pending work. Only typed validated setting mutations enter this worker.
- Voice and Notifications sections remain in place.

Verification performed without opening the app or writing to an account:

- `fluxer-desktop` check passed through `/tmp/fluxer-push-cargo` with explicit isolated-worktree manifest.
- Core display setting tests: exact boolean wire keys, font projection and bounds, corrupt-input refusal, and preservation of unknown nested/top-level protobuf fields (3 passed).
- Client loopback HTTP test: exact authenticated partial PATCH bodies, one pending write per display scope, optimistic compact rollback on 403, subsequent successful GIF update, unchanged status (1 passed).
- Native preference tests: actual keystroke-to-action selection changes after rebinding, conflict/reserved validation, serialization, atomic persistence, and disk-failure rollback (5 passed).
- Animation policy tests include reduced-motion precedence while preserving account autoplay, and existing authoritative false/default/proxy behavior (4 passed).

Manual acceptance remaining for root's GUI fixture pass: visually inspect compact rows/replies/media-only messages at 8/16/48px; use the slider and apply; verify keyboard focus/capture/cancel/reset; hover/open GIFs with both motion policies; verify settings close/return navigation; test authenticated save-error UI using a fixture server. No real account, hardware, audio capture, or installed app was touched. Full classic settings coverage (themes, all accessibility controls, and full message/media preference registry) is outside this coherent expansion; no empty destination is exposed.

## Root offline GUI acceptance

Integrated settings navigation and Notifications presentation were checked in an
owned 880px-wide demo window. Appearance preview/apply and actual compact chat
were inspected at 8/16/48px. Inline author clicks open the proper profile; text
wraps under the first line. Rebinding the switcher to Ctrl-Shift-J changes the
actual dispatched action; Escape cancels recording without closing settings,
reset restores defaults, and leaving Keybinds cancels recording. No account
setting writes were made. Actual-account save errors, IME and subjective review
remain pending. Captures: `target/qa/expansion2-2026-11-15/`.
