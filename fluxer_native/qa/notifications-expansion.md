# Notification lifecycle and unread synchronization

## Source contracts

- `fluxer_desktop/src/main/NotificationTargets.ts`: bounded identity-to-target map; clicks consume targets and closes forget targets. Native targets additionally carry session and per-channel generations.
- `fluxer_desktop/src/main/NotificationsIpc.ts`: freedesktop `ActionInvoked` routes clicks; `NotificationClosed` removes active ID mappings. Native registers both notification and owner-change subscriptions before its first Notify.
- `fluxer_app/src/features/notification/utils/NotificationTracker.ts`: account/channel-scoped notification clearing, at most five per channel.
- `fluxer_app/src/features/ui/state/Notification.ts`: channel selection/focus, accepted message ACKs and message deletes dismiss channel notifications. Existing native privacy/streamer/DND/thread/mute/content-warning notification policy is retained.
- `fluxer_app/src/features/read_state/state/read_states/ReadStateEntry.ts`, `ReadStateMentionMachine.ts`: direct/everyone/role mention suppression, blocked author suppression, non-member-thread everyone handling, ordinary unmuted DMs as mentions. This fixes live native insertion, which previously missed role/ordinary DM mentions and counted blocked/suppressed mentions.
- `ReadStateServerAckMachine.ts` and existing native navigation ACK implementation: version ordering precedes manual/older/current/newer ACK handling. This work retains that ordering and emits notification-dismissal events only for accepted gateway outcomes.

## Implementation

`notification_transport.rs` owns one cancellable worker and its D-Bus connection, two filtered bounded signal subscriptions, a bounded command queue and nonblocking bounded feedback queue, and at most 256 targets / five per account channel. The worker is joined when dropped. Dropping it also cancels stalled Notify calls rather than leaving a detached blocking signal thread. Ordinary method calls have a two-second timeout. There is no process-global D-Bus environment mutation.

The active unique owner of `org.freedesktop.Notifications` owns its numeric IDs. Owner replacement clears all target mappings; outgoing Notify/Close calls address that unique name. The owner is rechecked after Notify and before accepting an action, so daemon replacement and numeric ID reuse cannot route an old callback to a new target. A queued ownership signal rechecks the actual owner instead of replaying an obsolete generation. Normal D-Bus service activation is retained for Notify when no owner exists; Clear never activates a missing service.

Close and clear forget targets before contacting the daemon, including failure/disconnection. NotificationClosed removes targets, default actions consume targets, and unknown actions/IDs/senders are ignored. If the bounded transport queue is saturated when dismissal is requested, the old worker is canceled and its callback map discarded; an explicit error reports that the daemon may still show old notifications. Ordinary queue failures and asynchronous audio-output failures are also reported instead of silently claiming delivery/playback.

Desktop clicks check the authenticated account, session generation, per-channel generation, existing channel, VIEW_CHANNEL, READ_MESSAGE_HISTORY and message-channel kind before changing selection. Every SessionContext rotates the generation, including relogin or another instance with the same numeric user ID. ACK/delete/selection dismissal rotates that channel's generation before queueing OS work, so an already queued old click cannot navigate afterward. Accepted cross-client ACK/delete events are forwarded by the client worker without adding writes or changing read-ack payloads. The existing window-activation/deferred focus mechanism follows successful navigation.

Existing preference loading, bounded/private atomic JSON persistence, early user-edit precedence, sound volume/device policy, account-local seen tracking and privacy text escaping are preserved. Turning desktop delivery off, enabling streamer mode or changing preview privacy dismisses existing notifications.

## Automated verification

All Rust checks/tests use `/tmp/fluxer-push-cargo` with this isolated worktree's explicit manifest.

- Desktop notification tests: existing safe-text/failure/preference tests plus access/session/channel-generation click guards. Private D-Bus fixture tests cover exact Notify payload/actions/hints, an action emitted inside Notify before its reply (registration race), NotificationClosed cleanup, CloseNotification and clear-all, daemon-owner replacement reusing ID 1, stale old-owner actions, five-per-channel eviction, idle teardown, stalled-call cancellation, connection unique-name disappearance, and full-queue dismissal cancellation. No user's session bus is used.
- Core: all 59 tests pass, including three new live mention fixtures for DM/role behavior, blocked/suppressed/muted handling, non-member threads, duplicate dispatch and already acknowledged history.
- Client: local gateway fixture verifies stale-version ACKs do not dismiss notifications, accepted remote/manual ACKs and single/bulk deletes do, and manual unread state remains held.
- Desktop check passed before fixture expansion; the final desktop test target also compiles all production modules.

## Manual OS / two-account acceptance remaining

These require root's controlled acceptance pass and were not performed on the user's running app:

1. On the supported Linux desktop, deliver a permitted real notification, click it, and confirm window focus and message centering; verify notification expiry/dismissal and daemon restart.
2. Switch accounts and instances (including equal numeric IDs), then click old OS notifications; none should navigate the new session. Delete/revoke channel access before clicking and confirm rejection.
3. Read the notified channel from another authenticated client and verify OS dismissal; mark unread manually and verify it stays unread until the normal native navigation/paint policy allows acknowledgment.
4. Verify mute/DND/streamer/privacy behavior and chosen-output sound failure feedback on the real OS. No sounds, capture, real-account read mutations, or real notifications were exercised during automated work.
