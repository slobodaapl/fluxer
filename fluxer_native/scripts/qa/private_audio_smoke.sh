#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 || ( $# -eq 2 && "$2" != --preflight ) ]]; then
    printf 'Usage: %s ARTIFACT_DIRECTORY [--preflight]\n' "${0##*/}" >&2
    exit 2
fi
for executable in dbus-run-session pipewire wireplumber pipewire-pulse pactl python3; do
    command -v "$executable" >/dev/null || { printf 'Missing executable %s\n' "$executable" >&2; exit 1; }
done
if [[ "${2:-}" != --preflight ]]; then
    for executable in cargo curl timeout pacat pw-dump pw-loopback; do
        command -v "$executable" >/dev/null || { printf 'Missing executable %s\n' "$executable" >&2; exit 1; }
    done
fi

qa_scripts="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
mkdir -p -- "$1"
qa_artifacts="$(cd -- "$1" && pwd -P)"
qa_runtime="$(mktemp -d "${TMPDIR:-/tmp}/fluxer-audio.XXXXXXXX")"
trap 'rm -rf -- "$qa_runtime"' EXIT
export XDG_RUNTIME_DIR="$qa_runtime"

dbus-run-session -- bash -s -- "$qa_runtime" "$qa_artifacts" "$qa_scripts" "${2:-}" <<'BASH'
set -euo pipefail
qa_runtime="$1"
qa_artifacts="$2"
qa_scripts="$3"
export XDG_RUNTIME_DIR="$qa_runtime" PIPEWIRE_RUNTIME_DIR="$qa_runtime"
export XDG_CONFIG_HOME="$qa_runtime/config" XDG_CACHE_HOME="$qa_runtime/cache" XDG_STATE_HOME="$qa_runtime/state"
export DBUS_SYSTEM_BUS_ADDRESS="unix:path=$qa_runtime/no-system-bus"
export PIPEWIRE_REMOTE=pipewire-0 PULSE_SERVER="unix:$qa_runtime/pulse/native" PULSE_COOKIE="$qa_runtime/pulse-cookie"
unset PIPEWIRE_CONFIG_DIR PIPEWIRE_CONFIG_NAME PIPEWIRE_CONFIG_PREFIX WIREPLUMBER_CONFIG_DIR PULSE_CLIENTCONFIG

pw_pid='' wp_pid='' pulse_pid=''
cleanup() {
    local daemon
    for daemon in "$wp_pid" "$pulse_pid" "$pw_pid"; do
        if [[ -n "$daemon" ]]; then kill "$daemon" 2>/dev/null || true; fi
    done
    wait 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

pipewire > "$qa_artifacts/audio-pipewire.log" 2>&1 &
pw_pid=$!
for attempt in {1..40}; do
    if [[ -S "$qa_runtime/pipewire-0" ]]; then break; fi
    sleep .1
done
if [[ ! -S "$qa_runtime/pipewire-0" ]]; then
    printf 'Private PipeWire did not start. See audio-pipewire.log.\n' >&2
    exit 1
fi

# The policy profile starts graph policy without hardware discovery monitors.
wireplumber --profile policy > "$qa_artifacts/audio-wireplumber.log" 2>&1 &
wp_pid=$!
pipewire-pulse > "$qa_artifacts/audio-pulse.log" 2>&1 &
pulse_pid=$!
for attempt in {1..40}; do
    if [[ -S "$qa_runtime/pulse/native" ]] && pactl info >/dev/null 2>&1; then break; fi
    sleep .1
done
if [[ ! -S "$qa_runtime/pulse/native" ]] || ! pactl info > "$qa_artifacts/audio-pulse-info.log"; then
    printf 'Private PulseAudio socket did not become ready. See audio-pulse.log.\n' >&2
    exit 1
fi
kill -0 "$wp_pid"
pactl --format=json list cards > "$qa_artifacts/audio-cards.json"
python3 - "$qa_artifacts/audio-cards.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as cards:
    if json.load(cards) != []:
        raise SystemExit("Audio isolation failed: physical cards are visible")
PY
printf 'Private audio preflight PASS\n'
if [[ "$4" == --preflight ]]; then exit 0; fi

export FLUXER_MEDIA_TEST_URL=ws://127.0.0.1:7880
export FLUXER_MEDIA_TEST_API_KEY=devkey FLUXER_MEDIA_TEST_API_SECRET=secret
if ! curl --noproxy '*' --silent --show-error --fail --max-time 5 http://127.0.0.1:7880/ >/dev/null; then
    printf 'Start the development LiveKit server at 127.0.0.1:7880 before running the audio scenarios.\n' >&2
    exit 1
fi

status=0
for scenario in loopback microphone output watch share; do
    args=()
    case "$scenario" in
        loopback) script=test_media_loopback.py ;;
        microphone) script=test_microphone_capture.py ;;
        output) script=test_output_routing.py; args=(--media) ;;
        watch) script=test_output_routing.py; args=(--screen-watch) ;;
        share) script=test_shared_audio_capture.py; args=(--verify-system-isolation) ;;
    esac
    if timeout --kill-after=10s "${FLUXER_AUDIO_SMOKE_TIMEOUT:-160s}" \
        python3 "$qa_scripts/$script" "${args[@]}" > "$qa_artifacts/audio-$scenario.log" 2>&1; then
        printf '%s PASS\n' "$scenario"
    else
        printf '%s FAIL\n' "$scenario"
        status=1
    fi
done
exit "$status"
BASH
