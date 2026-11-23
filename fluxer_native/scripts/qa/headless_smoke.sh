#!/usr/bin/env bash
# Run a Fluxer offline demo in a private Umbriel Wayland session.
set -euo pipefail
umask 077

if [[ $# != 4 ]]; then
    echo "Usage: $0 UMBRIEL FLUXER-DESKTOP ARTIFACT-DIRECTORY {messages|selection|composer-keys|media|stream|fullscreen|forum|private-call}" >&2
    exit 2
fi
umbriel="$(realpath "$1")"
desktop="$(realpath "$2")"
artifacts="$(realpath -m "$3")"
scenario="$4"
script_dir="$(dirname "$(realpath "$0")")"
case "$scenario" in
    messages|selection|composer-keys) demo=--demo-messages ;;
    media) demo=--demo-media ;;
    stream|fullscreen) demo=--demo-stream ;;
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
wayland_debug=
if [[ "$scenario" == fullscreen ]]; then
    wayland_debug=client
fi
WAYLAND_DEBUG="$wayland_debug" "$desktop" "$demo" >"$artifacts/app.log" 2>&1 &
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
        click 90 324
        screenshot accessibility
        click 934 182
        screenshot reduced-motion
        wtype -s 150 -k Escape
        screenshot settings-closed
        click 505 639
        wtype -s 150 'Sent by clicking the Send button.'
        screenshot send-button-draft
        click 1230 640
        screenshot send-button-sent
        wtype -s 150 'Sent by pressing Enter.' -k Return
        screenshot send-enter-sent
        ;;
    selection)
        # Snap to the newest messages before selecting across several rows.
        python3 "$script_dir/headless_pointer.py" 760 420 5000
        screenshot newest
        python3 "$script_dir/headless_pointer.py" 380 232 715 562
        screenshot selected
        wtype -s 150 -M ctrl -s 80 -k c -s 80 -m ctrl
        timeout 3 wl-paste --no-newline > "$artifacts/selection.txt"
        test "$(rg -c '^Alex — ' "$artifacts/selection.txt")" -eq 2
        test "$(rg -c '^Sam — ' "$artifacts/selection.txt")" -eq 1
        rg -Fxq "And the new day's messages group normally." "$artifacts/selection.txt"
        if rg -Fq "private spoiler text" "$artifacts/selection.txt"; then
            echo "Chat selection copied a concealed spoiler" >&2
            exit 1
        fi
        python3 "$script_dir/headless_pointer.py" 760 420 -5000
        screenshot scrolled-away
        wl-copy "Clipboard sentinel"
        wtype -s 150 -M ctrl -s 80 -k c -s 80 -m ctrl
        timeout 3 wl-paste --no-newline > "$artifacts/selection-after-scroll.txt"
        cmp "$artifacts/selection.txt" "$artifacts/selection-after-scroll.txt"
        python3 "$script_dir/headless_pointer.py" 760 420 5000
        screenshot newest-again
        copy_from_menu() {
            wl-copy "Clipboard sentinel"
            python3 "$script_dir/headless_pointer.py" "$2" 260 right
            screenshot "$1"
            click "$(($2 + 85))" 377
            timeout 3 wl-paste --no-newline > "$artifacts/$1.txt"
        }
        copy_from_menu multi-message-menu 540
        cmp "$artifacts/selection.txt" "$artifacts/multi-message-menu.txt"
        python3 "$script_dir/headless_pointer.py" 380 260 482 260
        copy_from_menu single-message-selection-menu 435
        rg -qx 'Custom emoji: ?' "$artifacts/single-message-selection-menu.txt"
        click 835 260
        copy_from_menu whole-message-menu 540
        rg -Fxq 'Custom emoji: <:native_wave:123456789012345678> and standard Unicode 👋. **Formatting remains selectable.**' \
            "$artifacts/whole-message-menu.txt"
        ;;
    composer-keys)
        assert_clipboard() {
            timeout 3 wl-paste --no-newline > "$artifacts/$1.txt"
            printf '%s' "$2" | cmp - "$artifacts/$1.txt"
        }
        copy_draft() {
            wtype -s 100 -M ctrl -k a -k c -m ctrl
        }
        click 505 639
        wtype -s 100 "keep café" -M ctrl -k BackSpace -m ctrl
        copy_draft
        assert_clipboard word-backspace "keep "
        wtype -s 100 -M ctrl -k z -m ctrl
        copy_draft
        assert_clipboard undo-word-backspace "keep café"
        wtype -s 100 -k Home -M ctrl -k Delete -m ctrl
        copy_draft
        assert_clipboard word-delete " café"

        wtype -s 100 alpha -M shift -k Return -m shift beta -M shift -k Return -m shift gamma
        wtype -s 100 -M ctrl -k Home -k Down -M shift -k Down -m shift -k Insert -m ctrl
        assert_clipboard paragraph-selection $'beta\n'
        wtype -s 100 -M ctrl -k a -k b -m ctrl
        copy_draft
        assert_clipboard bold $'**alpha\nbeta\ngamma**'
        wtype -s 100 -M ctrl -k b -m ctrl
        copy_draft
        assert_clipboard bold-toggle $'alpha\nbeta\ngamma'
        wtype -s 100 -M ctrl -k z -m ctrl
        copy_draft
        assert_clipboard undo-bold-toggle $'**alpha\nbeta\ngamma**'

        wtype -s 100 café -M ctrl -k a -M shift -k s -m shift -m ctrl
        copy_draft
        assert_clipboard strikethrough "~~café~~"

        wl-copy "plain café"
        wtype -s 100 -M ctrl -k a -M shift -k v -m shift -m ctrl
        copy_draft
        assert_clipboard plain-text "plain café"
        wl-copy "Clipboard sentinel"
        wtype -s 100 -M shift -k Delete -m shift
        assert_clipboard cut-alias "plain café"
        wl-copy "insert paste"
        wtype -s 100 -M shift -k Insert -m shift
        copy_draft
        assert_clipboard paste-alias "insert paste"
        screenshot composer-shortcuts
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
    fullscreen)
        fullscreen_requests() {
            local count
            count="$(rg -c "xdg_toplevel#[0-9]+\\.$1\\(" "$artifacts/app.log" || true)"
            [[ "${count:-0}" -eq "$2" ]]
        }
        click 1148 62
        wait_for fullscreen_requests set_fullscreen 1
        screenshot viewer-fullscreen
        click 1190 52
        wait_for fullscreen_requests unset_fullscreen 1
        screenshot back-to-chat-restored
        click 380 116
        screenshot returned-to-viewer
        click 1148 62
        wait_for fullscreen_requests set_fullscreen 2
        screenshot viewer-fullscreen-again
        click 1254 52
        screenshot one-stream-left
        fullscreen_requests unset_fullscreen 1
        click 1254 52
        wait_for fullscreen_requests unset_fullscreen 2
        screenshot last-stream-restored
        "$umbriel" msg window-toggle-fullscreen
        screenshot chat-fullscreen
        wtype -s 150 -k Escape
        wait_for fullscreen_requests unset_fullscreen 3
        screenshot chat-escape-restored
        "$umbriel" msg window-toggle-fullscreen
        screenshot chat-fullscreen-again
        click 1207 43
        wait_for fullscreen_requests unset_fullscreen 4
        screenshot chat-button-restored
        ;;
    forum)
        click 134 302
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
