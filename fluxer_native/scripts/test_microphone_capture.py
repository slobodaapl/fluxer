#!/usr/bin/env python3
"""Exercise microphone capture using an isolated, generated virtual source.

Requires pactl, pacat, pw-dump and a disposable local LiveKit server (see
scripts/test_media_loopback.py). Creates a unique null sink and remapped source,
feeds a synthetic 440 Hz tone, then runs the opt-in microphone pipeline test.
It does not change defaults, move existing application streams, capture real
microphones, save audio, or send playback to a physical output.
"""
import argparse
import array
import json
import math
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import threading
import time
import uuid

PREFIX = "fluxer-microphone-test-"


def run_json(command):
    result = subprocess.run(command, check=True, capture_output=True, text=True, timeout=10)
    value = json.loads(result.stdout)
    if not isinstance(value, list):
        raise RuntimeError(f"Expected a JSON array from {command[0]}")
    return value


def resolve_node(source_name, description, sources, objects):
    """Resolve our source through observed metadata, never a guessed node name."""
    source = [entry for entry in sources if entry.get("name") == source_name]
    if not source:
        return None
    if len(source) != 1:
        raise RuntimeError("The isolated PulseAudio source name is ambiguous")
    serial = source[0].get("properties", {}).get("object.serial")
    candidates = set()
    for entry in objects:
        if entry.get("type") != "PipeWire:Interface:Node":
            continue
        props = entry.get("info", {}).get("props", {})
        name = props.get("node.name")
        if not isinstance(name, str) or not str(props.get("media.class", "")).startswith("Audio/Source"):
            continue
        matches = (
            name == source_name
            or props.get("node.description") == description
            or props.get("device.description") == description
            or (serial is not None and str(props.get("object.serial")) == str(serial))
        )
        if matches:
            candidates.add(name)
    if len(candidates) > 1:
        raise RuntimeError("The isolated source matches multiple PipeWire node names")
    if not candidates:
        return None
    name = candidates.pop()
    if not name.startswith(PREFIX):
        raise RuntimeError("Observed virtual source does not have the required synthetic-test prefix")
    return name


def wait_for_node(source_name, description):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        name = resolve_node(
            source_name,
            description,
            run_json(["pactl", "--format=json", "list", "sources"]),
            run_json(["pw-dump"]),
        )
        if name is not None:
            return name
        time.sleep(0.05)
    raise RuntimeError("The isolated source did not appear in the PipeWire graph")


def tone_period():
    # 440/48000 = 11/1200, so this period repeats without a discontinuity.
    samples = array.array("h")
    for frame in range(1200):
        value = round(math.sin(frame * math.tau * 440 / 48_000) * (32_767 * 0.2))
        samples.extend((value, value))
    if sys.byteorder != "little":
        samples.byteswap()
    return samples.tobytes()


def feed_tone(process, stop, errors):
    data = tone_period()
    try:
        while not stop.is_set():
            remaining = memoryview(data)
            while remaining and not stop.is_set():
                written = process.stdin.write(remaining)
                if not written:
                    raise RuntimeError("Synthetic playback pipe closed")
                remaining = remaining[written:]
    except (BrokenPipeError, OSError, RuntimeError) as error:
        if not stop.is_set():
            errors.append(str(error))


def terminate(process, group=False):
    if process is None or process.poll() is not None:
        return
    try:
        if group:
            os.killpg(process.pid, signal.SIGTERM)
        else:
            process.terminate()
        process.wait(timeout=5)
    except ProcessLookupError:
        pass
    except subprocess.TimeoutExpired:
        if group:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        else:
            process.kill()
        process.wait(timeout=5)


def module_owned(entry, marker, requested_names):
    return entry.get("name") in requested_names and marker in entry.get("argument", "")


def main():
    argparse.ArgumentParser(description=__doc__).parse_args()
    for executable in ("pactl", "pacat", "pw-dump"):
        if shutil.which(executable) is None:
            raise RuntimeError(f"Required executable is unavailable: {executable}")
    marker = uuid.uuid4().hex
    sink = f"{PREFIX}{marker}-sink"
    source = f"{PREFIX}{marker}"
    description = f"FluxerSyntheticMicrophone-{marker}"
    requested_names = {"module-null-sink", "module-remap-source"}
    baseline = {int(entry["index"]) for entry in run_json(["pactl", "--format=json", "list", "modules"])}
    created = []
    playback = None
    tests = None
    feeder = None
    stop = threading.Event()
    errors = []
    cleanup_errors = []

    def load_module(name, arguments):
        result = subprocess.run(["pactl", "load-module", name, *arguments],
                                check=True, capture_output=True, text=True, timeout=10)
        module_id = int(result.stdout.strip())
        if module_id in baseline:
            raise RuntimeError("Audio daemon returned an existing module ID")
        created.append(module_id)

    try:
        print("Creating isolated synthetic microphone; existing routes and defaults stay unchanged.", flush=True)
        load_module("module-null-sink", [f"sink_name={sink}", "rate=48000", "channels=2", "format=s16le",
                                        f"sink_properties=device.description=FluxerSyntheticSink-{marker}"])
        load_module("module-remap-source", [f"master={sink}.monitor", f"source_name={source}",
                                           f"source_properties=device.description={description}"])
        node = wait_for_node(source, description)
        print(f"Verified virtual PipeWire input: {node}", flush=True)
        playback = subprocess.Popen(["pacat", "--playback", "--raw", "--format=s16le", "--rate=48000",
                                     "--channels=2", f"--device={sink}"],
                                    stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, bufsize=0)
        feeder = threading.Thread(target=feed_tone, args=(playback, stop, errors), daemon=True,
                                  name="fluxer-synthetic-microphone")
        feeder.start()
        env = os.environ.copy()
        env["FLUXER_MICROPHONE_TEST_INPUT"] = f"input:pipewire:{node}"
        runner = Path(__file__).resolve().with_name("test_media_loopback.py")
        tests = subprocess.Popen([sys.executable, str(runner), "--microphone-pipeline"],
                                 env=env, start_new_session=True)
        while tests.poll() is None:
            if playback.poll() is not None or errors:
                raise RuntimeError("Synthetic audio playback stopped before the microphone test completed")
            time.sleep(0.1)
        return tests.returncode
    finally:
        # Terminate the test group before removing its explicitly selected source.
        # The credential-generating runner, Cargo and test binary all belong to it.
        terminate(tests, group=True)
        stop.set()
        terminate(playback)
        if feeder is not None:
            feeder.join(timeout=5)
        if playback is not None and playback.stdin is not None:
            playback.stdin.close()
        try:
            # Recover IDs even if load-module created a device but its command
            # was interrupted before returning its ID. Only this run's UUID and
            # modules absent from the baseline can qualify.
            modules = run_json(["pactl", "--format=json", "list", "modules"])
            owned = {int(entry["index"]): entry for entry in modules
                     if int(entry["index"]) not in baseline and module_owned(entry, marker, requested_names)}
            for module_id in owned:
                if module_id not in created:
                    created.append(module_id)
            for module_id in reversed(created):
                # A daemon restart could reuse an ID. Verify ownership again
                # instead of ever unloading someone else's module by ID alone.
                current = run_json(["pactl", "--format=json", "list", "modules"])
                entry = next((entry for entry in current if int(entry["index"]) == module_id), None)
                if entry is None or not module_owned(entry, marker, requested_names):
                    continue
                result = subprocess.run(["pactl", "unload-module", str(module_id)],
                                        capture_output=True, text=True, timeout=10)
                if result.returncode:
                    cleanup_errors.append(f"Could not unload this test's module {module_id}")
        except (subprocess.SubprocessError, ValueError, OSError) as error:
            cleanup_errors.append(f"Could not finish virtual-device cleanup: {error}")
        for error in cleanup_errors:
            print(error, file=sys.stderr)
        if cleanup_errors and sys.exc_info()[0] is None:
            raise RuntimeError("Virtual-device cleanup needs attention; see module IDs above")


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(130)
    except (RuntimeError, subprocess.SubprocessError, OSError, ValueError) as error:
        print(f"Microphone test setup failed: {error}", file=sys.stderr)
        raise SystemExit(1)
