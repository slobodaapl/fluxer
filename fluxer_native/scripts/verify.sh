#!/usr/bin/env bash
set -euo pipefail
native_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$native_root/scripts/check_parity.py"
cargo fmt --manifest-path "$native_root/Cargo.toml" --all --check
cargo clippy --manifest-path "$native_root/Cargo.toml" --workspace --all-targets --locked -- -D warnings
cargo test --manifest-path "$native_root/Cargo.toml" --workspace --locked
if [[ "${FLUXER_NATIVE_RUN_MEDIA_LOOPBACK:-0}" == 1 ]]; then
    python3 "$native_root/scripts/test_media_loopback.py"
fi
