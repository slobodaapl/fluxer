#!/usr/bin/env bash
# Run a Fluxer offline demo in a private Umbriel Wayland session.
set -euo pipefail
umask 077

if [[ $# != 4 ]]; then
    echo "Usage: $0 UMBRIEL FLUXER-DESKTOP ARTIFACT-DIRECTORY {messages|media|stream|forum|private-call}" >&2
    exit 2
fi
umbriel="$(realpath "$1")"
desktop="$(realpath "$2")"
artifacts="$(realpath -m "$3")"
scenario="$4"
script_dir="$(dirname "$(realpath "$0")")"
case "$scenario" in
    messages) demo=--demo-messages ;;
    media) demo=--demo-media ;;
    stream) demo=--demo-stream ;;
    forum) demo=--demo ;;
    private-call) demo=--demo-private-call ;;
    *) echo "Unknown scenario: $scenario" >&2; exit 2 ;;
esac
if [[ ! -x "$umbriel" || ! -x "$desktop" ]]; then
    echo "Provide paths to the Umbriel and Fluxer Desktop executables" >&2
    exit 2
fi
mkdir -p "$artifacts"
runtime="$(mktemp -d /tmp/fluxer-headless.XXXXXX)"
trap 'rm -rf -- "$runtime"' EXIT
mkdir -p "$runtime"/{config,cache,data,state,empty}
cat > "$runtime/config.toml" <<'CONFIG'
[general]
xwayland = false
show_cheatsheet = false
autostart = []
[animation]
enabled = false
[input.keyboard]
layout = "us"
numlock_toggle = false
CONFIG
# Keep the bus private and disable host service activation.
cat > "$runtime/session.conf" <<'BUS'
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <allow own="*"/>
  </policy>
</busconfig>
BUS

env -i PATH=/usr/local/bin:/usr/bin:/bin HOME="$HOME" LANG=C.UTF-8 \
    XDG_RUNTIME_DIR="$runtime" XDG_CONFIG_HOME="$runtime/config" \
    XDG_CACHE_HOME="$runtime/cache" XDG_DATA_HOME="$runtime/data" \
    XDG_STATE_HOME="$runtime/state" XDG_CONFIG_DIRS="$runtime/empty" \
    XDG_DATA_DIRS=/usr/local/share:/usr/share XDG_CURRENT_DESKTOP=umbriel \
    XDG_SESSION_TYPE=wayland WAYLAND_DISPLAY=wayland-0 \
    UMBRIEL_SOCKET="$runtime/umbriel-wayland-0.sock" \
    DBUS_SYSTEM_BUS_ADDRESS="unix:path=$runtime/no-system-bus" \
    PIPEWIRE_RUNTIME_DIR="$runtime" PULSE_SERVER="unix:$runtime/no-pulse" \
    GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
    dbus-run-session --config-file="$runtime/session.conf" -- bash -s -- "$umbriel" "$desktop" "$artifacts" "$script_dir" "$demo" "$scenario" <<'SESSION'
set -euo pipefail
umbriel="$1"
desktop="$2"
artifacts="$3"
script_dir="$4"
demo="$5"
scenario="$6"
app_pid=
"$umbriel" -c "$XDG_RUNTIME_DIR/config.toml" >"$artifacts/compositor.log" 2>&1 &
compositor_pid=$!
cleanup() {
    if [[ -n "$app_pid" ]]; then
        kill "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
    kill "$compositor_pid" 2>/dev/null || true
    wait "$compositor_pid" 2>/dev/null || true
}
trap cleanup EXIT

wait_for() {
    for ((i=0; i<100; i++)); do
        if "$@"; then return; fi
        sleep 0.1
    done
    echo "Timed out: $*" >&2
    return 1
}
wait_for test -S "$UMBRIEL_SOCKET"
"$desktop" "$demo" >"$artifacts/app.log" 2>&1 &
app_pid=$!
app_is_focused() {
    kill -0 "$app_pid" && "$umbriel" focused-window 2>/dev/null | rg -qi 'fluxer'
}
wait_for app_is_focused
"$umbriel" outputs >"$artifacts/outputs.log"

screenshot() {
    sleep 0.35
    kill -0 "$app_pid"
    grim -o HEADLESS-1 "$artifacts/$1.png"
}
click() {
    python3 "$script_dir/headless_pointer.py" "$1" "$2"
}
sleep 1.5
screenshot initial
case "$scenario" in
    messages)
        click 505 639
        wtype -s 150 -M ctrl -s 80 -k k -s 80 -m ctrl
        screenshot switcher
        wtype -s 150 -d 40 'off-topic'
        screenshot switcher-filtered
        wtype -s 150 -k Return
        screenshot selected-channel
        click 290 684
        screenshot settings
        click 90 282
        screenshot accessibility
        click 934 182
        screenshot reduced-motion
        wtype -s 150 -k Escape
        screenshot settings-closed
        ;;
    media)
        click 590 350
        screenshot video-playing
        click 749 508
        screenshot video-focus
        wtype -s 150 -k Escape
        screenshot video-returned
        ;;
    stream)
        click 1020 62
        screenshot main-stage
        click 535 605
        screenshot second-participant
        ;;
    forum)
        click 134 374
        screenshot forum-activity
        click 366 369
        screenshot forum-next-page
        click 360 115
        screenshot forum-design
        wtype -s 150 -k Tab
        screenshot forum-tag-keyboard-focus
        wtype -s 150 -k Return
        screenshot forum-match-any
        wtype -s 150 -k Tab
        wtype -s 150 -k space
        screenshot forum-match-all
        wtype -s 150 -k Tab
        wtype -s 150 -k Return
        screenshot forum-tags-cleared
        click 387 159
        screenshot forum-newest
        wtype -s 150 -k Tab
        screenshot forum-keyboard-focus
        wtype -s 150 -k Return
        screenshot forum-post-opened
        ;;
    private-call)
        click 1227 63
        screenshot call-declined
        ;;
esac
SESSION
