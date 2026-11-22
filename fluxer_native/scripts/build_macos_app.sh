#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
    echo "Build the Fluxer Native app bundle on macOS." >&2
    exit 1
fi

native_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$native_root/target}"
mkdir -p "$target_dir"
target_dir="$(cd -- "$target_dir" && pwd)"
bundle="$target_dir/macos/Fluxer Native.app"

cargo build --manifest-path "$native_root/Cargo.toml" --locked \
    --target-dir "$target_dir" -p fluxer-desktop

mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
install -m 755 "$target_dir/debug/fluxer-desktop" "$bundle/Contents/MacOS/fluxer-desktop"
cp "$native_root/packaging/macos/Info.plist" "$bundle/Contents/Info.plist"
cp "$native_root/../fluxer_desktop/build_resources/icons-stable/_compiled/AppIcon.icns" \
    "$bundle/Contents/Resources/AppIcon.icns"

codesign --force --sign "${FLUXER_MACOS_SIGN_IDENTITY:--}" "$bundle"
codesign --verify --strict "$bundle"
printf '%s\n' "$bundle"
