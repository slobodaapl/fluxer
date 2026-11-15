# Profile and presence expansion

The expanded profile reuses the compact profile's loaded snapshot and image resources. The modal presents only data already returned by the profile request, including biography, membership dates and guild roles. It preserves the existing direct-message and relationship confirmation flows. `Escape` closes it, backdrop clicks dismiss it, and its scroll area is bounded by the viewport. The entry is omitted for webhook identities.

Source references: `fluxer_app/src/features/user/models/Profile.ts`, `fluxer_app/src/features/user/state/CustomStatus.ts`, `fluxer_app/src/features/user/hooks/useCustomStatusDraft.tsx`, `fluxer_app/src/features/app/config/TimeWindowPresets.ts`, and `fluxer_app/src/features/user/state/StatusExpiry.ts`.

Pending live GUI review: narrow windows, long biographies and roles, image loading, keyboard navigation, backdrop dismissal and relationship confirmation. No live account mutations were performed.

The custom status editor uses the existing emoji catalog and source duration presets. It sends either `emoji_id` or `emoji_name`, and computes `expires_at` at save time. Pending live GUI review includes emoji picker focus and custom status expiry.

Timed presence resets use the source `status_resets_at` and `status_resets_to` fields, with a one-second expiry poll; custom status is omitted from gateway presence after expiry. Pending live review includes expiry during reconnect and account switching. On Linux, a separate Wayland connection uses version 2 input-only idle notifications when advertised. In particular, window focus is not evidence of system idleness; the client does not infer AFK from it. Biography links use the existing message navigation and trust policy; concealed spoiler spans remain concealed. Biography links participate in the profile keyboard cycle; richer inline formatting needs further work.

Classic desktop uses Electron `powerMonitor.getSystemIdleTime` (`fluxer_desktop/src/main/IpcHandlers.ts`) and combines it with local input in `fluxer_app/src/features/ui/state/Idle.ts`. Wayland's `ext_idle_notifier_v1` version 2 provides input-only threshold notifications tied to a seat; they begin their timeout upon creation, so reconnect may delay idle detection. Version 1 honors idle inhibitors and is not equivalent to input inactivity. Mutter's IdleMonitor provides elapsed idle time where that D-Bus service is available. The Wayland backend selects the first advertised seat and recreates its AFK notification when the configured timeout changes. A missing version 2 global or seat, disconnect, or non-Linux platform is treated as unavailable; Mutter is not wired. logind's `IdleSinceHint` records when a desktop-provided idle hint changed, not when input last occurred. References: the cached `wayland-protocols` staging `ext-idle-notify-v1.xml` and https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html.

## Root offline GUI acceptance

The full profile was opened from the compact popup, and a long biography and
external link were rendered. Link warnings appear above the profile; cancelling
returns to it and Escape closes it. GUI testing exposed and fixed an immediate
nested root-entity update in the profile link subscription.

Both expiry fields now use the shared scrollable Dropdown. Mouse selection and
keyboard arrow/Enter selection keep the editor open; Escape dismisses the open
menu before the editor. Emoji selection, artwork, keyboard navigation and picker
cancellation, clearing, Tab traversal and Save were exercised in an owned demo.
No real account writes were performed. Status surfaces are drawn in the root
overlay order, avoiding unsupported nested GPUI deferred drawing. The earlier
source-only GUI-pending notes above describe worktree delivery; real-account,
physical idle and cross-client review remains pending.
