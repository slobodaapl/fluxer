# Reuse encoded media after scrolling

The HTTP reader previously owned its entire cache. Offscreen GIF retirement
dropped that reader, its range buffer and its downloaded tempfile. Returning to
the same already-loaded message therefore started another HTTP request. The
MediaPlayer comment about keeping a cached file did not apply to open_reader.

Encoded media now has a process-local LRU independent of decoder/GPU ownership:

* 256 MiB and 128 entries maximum; ten minutes since the last cache acquisition,
  bounded further by the response's freshness. Expired entries are collected on
  cache access. Active reader leases remain valid when their entry is evicted.
* Small complete range responses and validated partial ranges retain encoded
  bytes in memory. Full-response fallbacks use unnamed files in the disk cache
  directory. Their descriptors keep them alive across reader retirement, and the
  OS reclaims them when the final owner/process exits; no stale file index or
  leftover named cache file. Decoded frames/decoders still retire offscreen.
* Cache keys contain the entire original signed URL and, for partial media, the
  aligned range offset. Cached ranges must agree with the reader's object size
  and validator. Seeking inside a cached range does not start another request.
  Progressive playback still fetches only requested ranges plus bounded prefetch.
* Reuse requires explicit positive max-age, accounts for Age, and skips no-store,
  no-cache and Vary:*. The source media proxy declares a long max-age for audio
  and video (`fluxer_media_proxy/src/http_headers.rs`); it is not assumed from a
  filename. Cache expiry is never extended beyond server freshness. Retry evicts
  the exact resource's whole body and ranges before fetching again.
* Credential-free media HTTP clients are shared across reader instances instead
  of starting a new client/runtime for each scroll return. API/gateway traffic
  and URL/query construction are unchanged. Simultaneous initial misses are not
  coalesced; bodies larger than the budget are usable only by their active reader.

Validation: 251 desktop tests passed, one existing performance case ignored;
strict desktop/all-targets Clippy passed. Transport tests drop/cancel the reader,
close the HTTP server, then reopen and seek three times with the server offline.
This covers full responses, complete single-range responses and multi-range
objects. Other regressions cover no-store, expiry, LRU/count/byte limits, signed
key isolation, independent file cursors, retry invalidation and active eviction.

Explicit offline GUI fixture:

```sh
FLUXER_MEDIA_DEMO_GIF_ONLY=1 \
FLUXER_MEDIA_DEMO_CACHE_SCROLL=1 \
FLUXER_MEDIA_DEMO_DELAY_MS=600 \
fluxer-desktop --demo --demo-media
```

It alternates between a GIF and distant filler rows twelve times. The run
recorded seven decoder opens, six retirements, and **one** video HTTP request.
The return capture shows a decoded playing frame without Loading. Artifacts are
under ignored `target/qa/media-cache-2026-11-15/`. Native live-account scrolling
and subjective return latency remain acceptance checks; the debugger remains
available for those checks. This encoded cache intentionally does not retain a
collection of full-resolution decoded/GPU frames or keep offscreen GIFs playing.
