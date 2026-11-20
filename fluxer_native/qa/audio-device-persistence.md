# Audio device persistence review — 2026-11-15

Explicit input and output selections were reported to revert after rebuilds or
restarts. The review below confirmed persistence in the tested build.

Observed on the live normal profile:

1. Before the new selection, `~/.config/fluxer-native/audio-preferences.json`
   contained default input/output IDs. The running app used that exact normal
   config path, not the isolated demo config or a path named after the build.
2. Selecting explicit input and output devices saved their stable PipeWire node
   names and readable labels immediately. Live dropdown probes confirmed both IDs.
3. The authenticated app was cold-restarted after a rebuild at the same source
   revision (`5a3e5f2ae-local`), using the same config.
4. Opening Voice & audio confirmed both device selections after restart. The
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
* Read/save failures are surfaced as client errors.

Four audio-preference regressions passed using the existing client test filter
`audio_preferences`; the forced native desktop build also passed. Owned debugger
logs and the before/after hash are in ignored
`target/qa/live-gpu-debug-20261115-122723/`. Prior-build behavior and alternate
environments remain unverified.
