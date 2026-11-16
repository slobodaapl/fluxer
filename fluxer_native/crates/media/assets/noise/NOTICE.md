# Native noise suppression assets

These unmodified binaries come from `@sapphi-red/web-noise-suppressor` **0.4.0**,
the same package pinned by `fluxer_app` in `pnpm-lock.yaml`:
https://registry.npmjs.org/@sapphi-red/web-noise-suppressor/-/web-noise-suppressor-0.4.0.tgz

They are embedded in the executable; no npm installation, model download, or
network access is needed at runtime. The Rust adapter implements their exported
C interfaces using Wasmtime, including GTCRN's 48 kHz interface.

The source tarball's SHA-512 integrity was checked against `pnpm-lock.yaml`.

| File | Implementation | License |
| --- | --- | --- |
| rnnoise.wasm | Xiph RNNoise, @shiguredo/rnnoise-wasm 2022.2.0 | BSD-3-Clause; Apache-2.0 wrapper |
| speex.wasm | SpeexDSP, @sapphi-red/speex-preprocess-wasm 0.4.0 | BSD-3-Clause |
| gtcrn.wasm | GTCRN code/weights, @sapphi-red/gtcrn-wasm 0.0.3; pffft | MIT; FFTPACK/UCAR |

Full notices are in the adjacent LICENSE files. The package's MIT license is
also included. When updating these assets, verify both imports and exports:
the Speex and GTCRN Emscripten export names are minified and version-specific.

## DeepFilterNet3

DFN reuses the existing patched WASM in
`fluxer_app/src/features/voice/utils/noise_suppression/deepfilternet3/df_bg.wasm`
and its MIT/Apache-2.0 licenses and [provenance notice](../../../../../fluxer_app/src/features/voice/utils/noise_suppression/deepfilternet3/NOTICE.md).
WASM SHA-256: `c42dcfa05c64ff21d23fe3e3a35e934323e4fd85c303b6a9f8a21bed05f7fb74`.

The build embeds `DeepFilterNet3_onnx.tar.gz` with SHA-256
`c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616`
and size 7,983,136 bytes. It uses the existing hydrated LFS archive, an explicit
`FLUXER_DEEP_FILTER_MODEL` path, or a hash-verified build-time download from
https://raw.githubusercontent.com/Rikorose/DeepFilterNet/main/models/DeepFilterNet3_onnx.tar.gz.
The source pointer is left intact and neither asset is downloaded at runtime.
