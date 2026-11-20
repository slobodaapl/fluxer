# Short GIFV GPU lifetime investigation

The original kernel and thread captures identify fluxer-desktop in an NVIDIA
Xid 13 MMU fault. The main UI thread is inside Vulkan semaphore waiting from
BladeRenderer, while file playback is idle. The preserved open file is a 640x640
VP8 WebM with duration 160 ms; its association with the precise visible GIF has
not been established. This is separate from the earlier Loading/focus defect.

Source review confirms immediate atlas texture destruction without waiting for
the latest GPU use. A CPU image eviction is not GPU completion. Vulkan requires
[all submitted image references to finish before destruction](https://docs.vulkan.org/refpages/latest/refpages/source/vkDestroyImage.html).
The freeze's precise trigger remains a hypothesis until reproduced with useful
GPU validation evidence; do not label the particular Xid as definitively solved.

The patch retires zero-key textures after their own final GPU submission, while
preserving queued uploads and current scene references. Collection polls with a
zero timeout and only then returns the atlas slot for reuse. Redraws of a retained
scene update the final submission even if its image key was already removed.
Frame-wait failure now exits with a stderr diagnostic after the bounded wait,
instead of looping forever. This does not implement GPU device recovery.

## Offline reproduction

Generate a nonpersonal clip matching the captured file's dimensions, codec and
duration (the actual content and exact byte size differ):

```sh
ffmpeg -f lavfi -i testsrc2=size=640x640:rate=25:duration=0.16 \
  -an -c:v libvpx -b:v 1M short.webm
FLUXER_GPU_VALIDATION=1 \
FLUXER_MEDIA_DEMO_ATLAS_STRESS=1 \
FLUXER_MEDIA_DEMO_GIF_ONLY=1 \
FLUXER_MEDIA_DEMO_VIDEO="$PWD/short.webm" \
fluxer-desktop --demo --demo-media
```

This explicit offline fixture serves only loopback-generated media and scrolls
between sixteen GIF messages 48 times, every 450 ms. It exercises repeated frame
replacement and offscreen player/texture retirement; `ATLAS_STRESS complete`
marks the end. Vulkan validation requires the Khronos validation layer. Check
stderr for missing-layer warnings before claiming a validation run. Neither the
fixture nor the investigation sends account traffic or alters the frozen app.

Four normal desktop regressions exercise the production lifetime policy with
controlled submission completion. Real GPU/device-loss injection and the exact
live-account scrolling scenario remain review items. Test artifacts are retained
in ignored `target/qa/gif-gpu-lifetime-2026-11-15/`.

## Validation result (2026-11-15)

* 244 desktop tests passed, one existing performance test ignored; this includes
  all four atlas lifetime regressions. Strict desktop/all-targets Clippy passed.
* The generated 640x640, 160 ms VP8 clip completed all 48 scroll cycles, with 200
  video fetches as offscreen players retired/reopened. The end capture shows
  decoded frames and a responsive window. No Vulkan warning/error or fatal GPU
  wait was logged. The fixture's process maps confirm that the Khronos
  validation layer loaded.
* The pre-fix build was not deliberately subjected to the unsafe GPU lifetime
  scenario again. Passing stress/validation is evidence for the patch's behavior,
  not proof of the original fault's cause or a guarantee against driver faults.
* The frozen live process was left untouched. Device-loss/fatal-timeout handling
  was reviewed in source, not induced on the user's GPU. Safe device recovery is
  still future work; a failed frame wait now terminates instead of hanging.
