#!/usr/bin/env python3
"""Run real shared-audio capture against an isolated synthetic playback process.

Creates two UUID null sinks and a non-Fluxer pacat sibling of the test process.
Only this owned playback process is selected for capture. Stereo 48 kHz float32
tones (left 440 Hz/right 880 Hz, amplitude 0.2) feed sink A; the publisher's
explicit output is sink B. No defaults, existing application streams, physical
inputs/outputs, or personal sink monitors are changed or recorded.
"""
import argparse
import array
import math
import os
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import threading
import time
import unittest
import uuid

from test_microphone_capture import module_inventory, module_owned, run_json, terminate
from test_output_routing import PREFIX, node_properties, output_targets, wait_for_sink

PROCESS_MARKER = "FLUXER_SHARED_AUDIO_PROCESS "


def tone_period():
    samples = array.array("f")
    # 440/48000=11/1200 and 880/48000=22/1200: both channels loop smoothly.
    for frame in range(1200):
        samples.extend(math.sin(frame * math.tau * frequency / 48_000) * 0.2
                       for frequency in (440, 880))
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


def process_marker(line):
    if PROCESS_MARKER not in line:
        return None
    value = line.split(PROCESS_MARKER, 1)[1].strip()
    if not value.isdecimal() or int(value) <= 1:
        raise RuntimeError("Invalid test process identity marker")
    return int(value)


def node_key(entry):
    props = node_properties(entry)
    return (int(entry["id"]), str(props.get("object.serial", "")), str(props.get("node.name", "")))


def owned_audio_nodes(objects, process_ids):
    """Scope cleanup checks by exact PID or the native backend's PID names.

    Private capture sinks are created server-side and may not expose the
    process's client.id. common.rs next_direct_sink_name includes the exact PID.
    Object serials prevent graph-ID reuse from implicating unrelated nodes.
    """
    pids = {str(pid) for pid in process_ids}
    clients = {str(entry["id"]) for entry in objects
               if entry.get("type") == "PipeWire:Interface:Client"
               and str(node_properties(entry).get("application.process.id")) in pids}
    result = set()
    for entry in objects:
        if entry.get("type") != "PipeWire:Interface:Node":
            continue
        props = node_properties(entry)
        name = str(props.get("node.name", ""))
        named_owner = any(name.startswith(f"fluxer-direct-capture-{pid}-")
                          or name == f"fluxer-native-speaker-{pid}"
                          or name == f"fluxer-native-microphone-{pid}" for pid in pids)
        if (named_owner or str(props.get("application.process.id")) in pids
                or str(props.get("client.id")) in clients):
            result.add(node_key(entry))
    return result


def wait_no_owned_nodes(process_ids, baseline_nodes):
    deadline = time.monotonic() + 8
    while True:
        remaining = owned_audio_nodes(run_json(["pw-dump"]), process_ids) - baseline_nodes
        if not remaining:
            return
        if time.monotonic() >= deadline:
            raise RuntimeError(f"Test-owned PipeWire nodes remained after teardown: {sorted(remaining)}")
        time.sleep(0.05)


def wait_playback_route(process, expected):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("Synthetic playback exited before its route was verified")
        streams, targets = output_targets(run_json(["pw-dump"]), process.pid)
        if streams and targets == {expected}:
            return
        if targets - {expected}:
            raise RuntimeError("Synthetic playback was routed outside its isolated sink")
        time.sleep(0.05)
    raise RuntimeError("Synthetic playback did not connect to its selected virtual sink")


def read_stdout(process, lines):
    try:
        for line in process.stdout:
            lines.put(line)
    finally:
        lines.put(None)


def consume_lines(lines, test_pids):
    while True:
        try:
            line = lines.get_nowait()
        except queue.Empty:
            return
        if line is not None:
            print(line, end="", flush=True)
            pid = process_marker(line)
            if pid is not None:
                test_pids.add(pid)


def unload_owned_modules(marker, baseline, created):
    errors = []
    try:
        owned = {int(entry["index"]) for entry in module_inventory()
                 if int(entry["index"]) not in baseline and module_owned(entry, marker, {"module-null-sink"})}
        for module_id in owned:
            if module_id not in created:
                created.append(module_id)
        for module_id in reversed(created):
            # Recheck ownership immediately before mutation: daemon restarts can
            # reuse numeric module IDs. Never unload by remembered ID alone.
            entry = next((item for item in module_inventory() if int(item["index"]) == module_id), None)
            if entry is None or not module_owned(entry, marker, {"module-null-sink"}):
                continue
            result = subprocess.run(["pactl", "unload-module", str(module_id)],
                                    capture_output=True, text=True, timeout=10)
            if result.returncode:
                errors.append(f"Could not unload this test's module {module_id}")
    except (subprocess.SubprocessError, OSError, ValueError) as error:
        errors.append(f"Could not complete isolated sink cleanup: {error}")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="Run pure fixtures without accessing audio services")
    args = parser.parse_args()
    if args.self_test:
        result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(FixtureTests))
        return 0 if result.wasSuccessful() else 1
    for executable in ("pactl", "pacat", "pw-dump", "cargo"):
        if shutil.which(executable) is None:
            raise RuntimeError(f"Required executable is unavailable: {executable}")

    marker = uuid.uuid4().hex
    sinks = [f"{PREFIX}{marker}-{suffix}" for suffix in ("a", "b")]
    baseline_modules = {int(entry["index"]) for entry in module_inventory()}
    baseline_nodes = {node_key(entry) for entry in run_json(["pw-dump"])
                      if entry.get("type") == "PipeWire:Interface:Node"}
    created = []
    playback = tests = feeder = reader = None
    stop = threading.Event()
    errors = []
    cleanup_errors = []
    lines = queue.Queue()
    test_pids = set()
    try:
        for sink in sinks:
            result = subprocess.run(
                ["pactl", "load-module", "module-null-sink", f"sink_name={sink}", "rate=48000", "channels=2",
                 "format=float32le", f"sink_properties=device.description=SyntheticSharedAudio-{marker}"],
                check=True, capture_output=True, text=True, timeout=10)
            module_id = int(result.stdout.strip())
            if module_id in baseline_modules:
                raise RuntimeError("Audio service returned a preexisting module ID")
            created.append(module_id)
        observed = [wait_for_sink(sink) for sink in sinks]
        print("Using two isolated virtual sinks and an owned synthetic playback process.", flush=True)
        playback = subprocess.Popen(
            ["pacat", "--playback", "--raw", "--format=float32le", "--rate=48000", "--channels=2",
             f"--client-name=SyntheticAudioTone-{marker}", f"--stream-name=SyntheticShareTone-{marker}",
             f"--device={sinks[0]}"],
            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, bufsize=0)
        feeder = threading.Thread(target=feed_tone, args=(playback, stop, errors), daemon=True)
        feeder.start()
        wait_playback_route(playback, observed[0])
        env = os.environ.copy()
        env["FLUXER_SHARED_AUDIO_TEST_PROCESS"] = str(playback.pid)
        env["FLUXER_SHARED_AUDIO_TEST_OUTPUT"] = f"output:pipewire:{observed[1]}"
        runner = Path(__file__).resolve().with_name("test_media_loopback.py")
        # Playback and the test runner are siblings; the native capture backend
        # deliberately excludes descendants of the Rust process to prevent echo.
        tests = subprocess.Popen([sys.executable, str(runner), "--shared-audio-capture"], env=env,
                                 stdout=subprocess.PIPE, text=True, bufsize=1, start_new_session=True)
        reader = threading.Thread(target=read_stdout, args=(tests, lines), daemon=True)
        reader.start()
        deadline = time.monotonic() + 180
        while tests.poll() is None:
            consume_lines(lines, test_pids)
            if playback.poll() is not None or errors:
                raise RuntimeError("Synthetic playback stopped before shared-audio tests finished")
            if time.monotonic() >= deadline:
                raise RuntimeError("Shared-audio integration test deadline exceeded")
            time.sleep(0.05)
        reader.join(timeout=5)
        consume_lines(lines, test_pids)
        if tests.returncode:
            raise RuntimeError("Shared-audio integration fixture failed")
        if not test_pids:
            raise RuntimeError("Shared-audio fixture did not identify its process for teardown checks")
        # Capture must release its private graph before the process finishes;
        # verify it did not leave capture nodes or redirect the synthetic app.
        wait_no_owned_nodes(test_pids, baseline_nodes)
        wait_playback_route(playback, observed[0])
        print("Actual shared-audio capture tests passed; test-owned capture nodes were released.", flush=True)
        return 0
    finally:
        terminate(tests, group=True)
        if reader is not None:
            reader.join(timeout=5)
        consume_lines(lines, test_pids)
        stop.set()
        terminate(playback)
        if feeder is not None:
            feeder.join(timeout=5)
        if playback is not None and playback.stdin is not None:
            playback.stdin.close()
        try:
            wait_no_owned_nodes(test_pids | ({playback.pid} if playback is not None else set()), baseline_nodes)
        except (RuntimeError, subprocess.SubprocessError, OSError, ValueError) as error:
            cleanup_errors.append(str(error))
        cleanup_errors.extend(unload_owned_modules(marker, baseline_modules, created))
        for error in cleanup_errors:
            print(error, file=sys.stderr)
        if cleanup_errors and sys.exc_info()[0] is None:
            raise RuntimeError("Isolated shared-audio cleanup needs attention")


class FixtureTests(unittest.TestCase):
    def test_stereo_tone_format_amplitude_and_channel_frequencies(self):
        samples = array.array("f")
        samples.frombytes(tone_period())
        if sys.byteorder != "little":
            samples.byteswap()
        self.assertEqual(len(samples), 2400)
        for channel, frequency in [(0, 440), (1, 880)]:
            data = samples[channel::2]
            expected = [math.sin(n * math.tau * frequency / 48_000) * 0.2 for n in range(1200)]
            self.assertLess(max(abs(actual - ideal) for actual, ideal in zip(data, expected)), 1e-7)
            self.assertAlmostEqual(math.sqrt(sum(value * value for value in data) / 1200), 0.2 / math.sqrt(2), places=6)

    def test_owned_nodes_match_exact_process_and_native_pid_prefix_only(self):
        objects = [
            {"id": 1, "type": "PipeWire:Interface:Client", "info": {"props": {"application.process.id": 42}}},
            {"id": 2, "type": "PipeWire:Interface:Node", "info": {"props": {"node.name": "capture", "client.id": 1, "object.serial": 200}}},
            {"id": 3, "type": "PipeWire:Interface:Node", "info": {"props": {"node.name": "fluxer-direct-capture-42-1", "object.serial": 300}}},
            {"id": 4, "type": "PipeWire:Interface:Node", "info": {"props": {"node.name": "fluxer-direct-capture-420-1", "object.serial": 400}}},
            {"id": 5, "type": "PipeWire:Interface:Node", "info": {"props": {"node.name": "personal-output", "application.process.id": 99}}},
        ]
        self.assertEqual(owned_audio_nodes(objects, {42}), {(2, "200", "capture"), (3, "300", "fluxer-direct-capture-42-1")})
        self.assertNotEqual(node_key(objects[1]), (2, "201", "capture"))

    def test_process_marker_handles_libtest_prefix_and_rejects_invalid_pid(self):
        self.assertEqual(process_marker("test actual_capture ... FLUXER_SHARED_AUDIO_PROCESS 123\n"), 123)
        self.assertIsNone(process_marker("ordinary test output"))
        for value in ("0", "1", "-2", "123 extra"):
            with self.assertRaises(RuntimeError):
                process_marker(PROCESS_MARKER + value)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(130)
    except (RuntimeError, subprocess.SubprocessError, OSError, ValueError) as error:
        print(f"Shared-audio capture test failed: {error}", file=sys.stderr)
        raise SystemExit(1)
