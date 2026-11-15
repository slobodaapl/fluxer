# Audio device persistence review — 2026-11-15

The user reported that explicit GoXLR input/output selections appeared to revert
to defaults after native rebuilds/restarts. This review did not reproduce a reset
on the current code/build, and does not claim to explain the earlier loss.

Observed on the live normal profile:

1. Before the new selection, `~/.config/fluxer-native/audio-preferences.json`
   contained default input/output IDs. The running app used that exact normal
   config path, not the isolated demo config or a path named after the build.
2. The user selected GoXLR Chat Mic and GoXLR Chat. The file changed immediately
   to the corresponding stable PipeWire node names and readable labels. Live
   dropdown probes confirmed both exact preferred IDs.
3. With no call active, force-rebuilt the desktop executable at the same source
   revision (`5a3e5f2ae-local`), installed it atomically, and cold-restarted the
   authenticated app under GDB using the same config environment.
4. Opened Voice & audio with Ctrl+, without choosing either device again. GDB
   dropdown-render probes confirmed the GoXLR input and output selections. The
   complete preferences file's SHA-256 matched before and after the rebuild and
   restart. Neither device was silently rewritten to a default during the check.

Source audit:

* The dropdown emits a selection command; accepted device selections update the
  worker's preferences and call persistence immediately, independently of exit.
* Storage is scoped by discovered API origin and authenticated user ID. It writes
  a sibling tempfile, flushes/fsyncs it and atomically replaces the JSON file.
  Build version and executable location are not part of the path or account key.
* Authentication/session restoration loads this file and emits the stored
  selections. Media engine creation restores both preferred device IDs. Inventory
  refresh preserves an unavailable preferred device instead of overwriting the
  saved preference with a temporary system-default fallback.
* Read/save failures are surfaced as client errors. No settings implementation
  change was made during this audit. Existing unrelated audio/DSP changes remain
  separate and untouched.

Four audio-preference regressions passed using the existing client test filter
`audio_preferences`; the forced native desktop build also passed. Owned debugger
logs and the before/after hash are in ignored
`target/qa/live-gpu-debug-20261115-122723/`. The live app is left running with both
GoXLR choices under GDB/Vulkan validation; dropdown changes continue to be logged
for review if a reset occurs again. Prior-build behavior, alternate environments
and choices made before this observation remain unverified.
