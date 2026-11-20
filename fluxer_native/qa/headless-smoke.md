# Private Wayland smoke runs

Run the driver with an Umbriel executable, the native Desktop executable, an
artifact directory, and one of `messages`, `media`, `stream`, or `forum`.
Use a new artifact directory for each run. The driver requires Linux, Bash,
`dbus-run-session`, `grim`, `wtype`, `rg`, and Python 3.

```sh
fluxer_native/scripts/qa/headless_smoke.sh \
  /path/to/umbriel /path/to/fluxer-desktop \
  fluxer_native/target/qa/forum forum
```

The driver starts a headless compositor and a separate D-Bus session without
service activation. The app uses temporary XDG directories and opens an offline
demo. All clicks, keys, and screenshots go through the private Wayland socket.
The media run plays a generated silent MP4. The stream run displays two
synthetic screen shares. The driver stops the app and compositor on exit and
keeps screenshots and logs in the artifact directory.

Inspect the screenshots in scenario order. The driver uses coordinates for a
1280×720 Umbriel output, so layout changes may require coordinate updates.

| Scenario | Expected screenshots |
| --- | --- |
| `messages` | The quick switcher opens, filters to `off-topic`, and navigates on Enter. Accessibility shows Reduce motion off and then on. Escape returns to `off-topic`. |
| `media` | The silent clip starts inline, opens in focus view, and returns to the chat timeline on Escape. |
| `stream` | Tile all shows Alex and Morgan. Main stage starts with Alex, then switches to Morgan's portrait stream when selected. |
| `forum` | `#ideas` starts with Keyboard shortcuts and Compact navigation. Load more reveals Focus indicators. Design filters to Compact navigation and Focus indicators. Tab focuses Accessibility and Enter selects it. Tab and Space switch the match mode. With both tags selected, Match all shows only Focus indicators. Tab and Enter on Clear tags restore the unfiltered page. Newest puts Focus indicators first. Tab highlights it and Enter opens its starter message. |
