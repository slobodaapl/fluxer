#!/usr/bin/env python3
"""Verify output routing through isolated virtual sinks, never physical outputs.

Creates two UUID-scoped null sinks without changing defaults or moving existing
streams. Records only their synthetic monitor signals in memory. The direct
SpeakerOutput probe verifies A -> B -> stopped; --media uses the encoded-media
pipeline test with A -> gain -> mute -> B -> stopped handshakes.
"""
import argparse
import array
from collections import deque
import json
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

PREFIX = "fluxer-output-test-"
SETTLE = 0.8
WINDOW = 1.0


def node_properties(entry):
    return entry.get("info", {}).get("props", {})


def resolve_sink(name, sinks, objects):
    matches = [sink for sink in sinks if sink.get("name") == name]
    if not matches:
        return None
    if len(matches) != 1:
        raise RuntimeError("Isolated sink name is ambiguous")
    serial = matches[0].get("properties", {}).get("object.serial")
    candidates = []
    for entry in objects:
        props = node_properties(entry)
        if entry.get("type") != "PipeWire:Interface:Node" or props.get("media.class") != "Audio/Sink":
            continue
        if props.get("node.name") == name or (
            serial is not None and str(props.get("object.serial")) == str(serial)
        ):
            candidates.append(entry)
    if len(candidates) > 1:
        raise RuntimeError("Isolated sink matches multiple PipeWire nodes")
    if not candidates:
        return None
    observed_name = node_properties(candidates[0]).get("node.name")
    if not isinstance(observed_name, str) or not observed_name.startswith(PREFIX):
        raise RuntimeError("Observed output does not have the synthetic-test prefix")
    return observed_name


def wait_for_sink(name):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        result = resolve_sink(name, run_json(["pactl", "--format=json", "list", "sinks"]),
                              run_json(["pw-dump"]))
        if result is not None:
            return result
        time.sleep(0.05)
    raise RuntimeError("Isolated output did not appear in the PipeWire graph")


def output_targets(objects, pid):
    """Observed stream links for the exact probe PID, never generic Fluxer IDs."""
    clients = {str(entry["id"]) for entry in objects
               if entry.get("type") == "PipeWire:Interface:Client"
               and str(node_properties(entry).get("application.process.id")) == str(pid)}
    streams = set()
    names = {}
    for entry in objects:
        if entry.get("type") != "PipeWire:Interface:Node":
            continue
        props = node_properties(entry)
        names[int(entry["id"])] = props.get("node.name")
        if props.get("media.class") != "Stream/Output/Audio":
            continue
        if (str(props.get("application.process.id")) == str(pid)
                or str(props.get("client.id")) in clients):
            streams.add(int(entry["id"]))
    targets = set()
    for entry in objects:
        if entry.get("type") != "PipeWire:Interface:Link":
            continue
        info = entry.get("info", {})
        props = info.get("props", {})
        source = info.get("output-node-id", props.get("link.output.node"))
        target = info.get("input-node-id", props.get("link.input.node"))
        if source is not None and target is not None and int(source) in streams:
            target_name = names.get(int(target))
            if target_name is None:
                raise RuntimeError("Probe output links to an unresolved graph node")
            targets.add(target_name)
    return streams, targets


def check_graph(pid, expected):
    deadline = time.monotonic() + 5
    while True:
        streams, targets = output_targets(run_json(["pw-dump"]), pid)
        if targets == expected and (bool(streams) if expected else not streams):
            return
        # An unexpected destination is an immediate failure, rather than allowing
        # a later reroute to hide a momentary physical/wrong-sink link.
        if targets - expected:
            raise RuntimeError(f"Probe output linked outside the selected sink: {sorted(targets)}")
        if time.monotonic() >= deadline:
            raise RuntimeError(f"Probe graph did not match selected sink: expected {sorted(expected)}, got {sorted(targets)}")
        time.sleep(0.05)


class Monitor:
    def __init__(self, sink):
        if not sink.startswith(PREFIX):
            raise RuntimeError("Refusing to record a non-synthetic sink monitor")
        self.frames = deque(maxlen=2000)
        self.lock = threading.Lock()
        self.errors = []
        self.process = subprocess.Popen(
            ["pacat", "--record", "--raw", "--format=float32le", "--rate=48000", "--channels=2",
             "--latency-msec=20", f"--device={sink}.monitor"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0)
        self.thread = threading.Thread(target=self._read, daemon=True, name="fluxer-output-monitor")
        self.thread.start()

    def _read(self):
        pending = b""
        try:
            while True:
                data = self.process.stdout.read(3840)
                if not data:
                    break
                pending += data
                complete = len(pending) // 4 * 4
                if not complete:
                    continue
                samples = array.array("f")
                samples.frombytes(pending[:complete])
                pending = pending[complete:]
                if sys.byteorder != "little":
                    samples.byteswap()
                if any(not math.isfinite(value) for value in samples):
                    raise RuntimeError("Sink monitor produced non-finite PCM")
                with self.lock:
                    self.frames.append((time.monotonic(), sum(value * value for value in samples), len(samples)))
        except (OSError, RuntimeError) as error:
            self.errors.append(str(error))

    def rms(self, start, end):
        if self.process.poll() is not None or self.errors:
            raise RuntimeError("Synthetic sink monitor stopped unexpectedly")
        with self.lock:
            frames = [frame for frame in self.frames if start <= frame[0] <= end]
        count = sum(frame[2] for frame in frames)
        if count < 9600:
            raise RuntimeError("Too few virtual monitor samples to verify routing")
        return math.sqrt(sum(frame[1] for frame in frames) / count)

    def close(self):
        terminate(self.process)
        self.thread.join(timeout=5)
        if self.process.stdout is not None:
            self.process.stdout.close()


def measure_pair(monitors):
    start = time.monotonic() + SETTLE
    end = start + WINDOW
    while time.monotonic() < end:
        if any(monitor.process.poll() is not None for monitor in monitors):
            raise RuntimeError("Synthetic sink monitor exited during measurement")
        time.sleep(min(0.05, max(0, end - time.monotonic())))
    return tuple(monitor.rms(start, end) for monitor in monitors)


def stdout_reader(process, lines):
    try:
        for line in process.stdout:
            lines.put(line)
    finally:
        lines.put(None)


def await_phase(process, lines, expected, pid):
    deadline = time.monotonic() + 40
    while time.monotonic() < deadline:
        try:
            line = lines.get(timeout=max(0.01, deadline - time.monotonic()))
        except queue.Empty:
            break
        if line is None:
            raise RuntimeError(f"Output probe exited before phase {expected}")
        print(line, end="", flush=True)
        if "FLUXER_OUTPUT_PROCESS " in line:
            observed_pid = int(line.split("FLUXER_OUTPUT_PROCESS ", 1)[1].strip())
            if pid is not None and pid != observed_pid:
                raise RuntimeError("Probe unexpectedly changed process identity")
            pid = observed_pid
        if "FLUXER_OUTPUT_PHASE " in line:
            phase = line.split("FLUXER_OUTPUT_PHASE ", 1)[1].strip()
            if phase != expected or pid is None:
                raise RuntimeError(f"Unexpected probe handshake: {phase}")
            return pid
    raise RuntimeError(f"Output probe phase deadline exceeded: {expected}")


def build_probe(native, env):
    result = subprocess.run(
        ["cargo", "build", "--manifest-path", str(native / "Cargo.toml"), "-p", "fluxer-system-audio",
         "--example", "output_routing_probe", "--offline", "--message-format=json"],
        env=env, capture_output=True, text=True, timeout=180)
    if result.stderr:
        print(result.stderr, end="", file=sys.stderr)
    if result.returncode:
        raise RuntimeError("Could not build isolated output probe")
    for line in result.stdout.splitlines():
        value = json.loads(line)
        if value.get("reason") == "compiler-artifact" and value.get("target", {}).get("name") == "output_routing_probe":
            if value.get("executable"):
                return [value["executable"]]
    raise RuntimeError("Cargo did not report the output probe executable")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--media", action="store_true", help="Run encoded remote audio through MediaEngine's mixer/output")
    parser.add_argument("--self-test", action="store_true", help="Run pure fixture tests without accessing audio services")
    args = parser.parse_args()
    if args.self_test:
        result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(FixtureTests))
        return 0 if result.wasSuccessful() else 1
    for executable in ("pactl", "pacat", "pw-dump", "cargo"):
        if shutil.which(executable) is None:
            raise RuntimeError(f"Required executable is unavailable: {executable}")
    native = Path(__file__).resolve().parents[1]
    env = os.environ.copy()
    env.setdefault("CARGO_INCREMENTAL", "0")
    command = ([sys.executable, str(native / "scripts" / "test_media_loopback.py"), "--output-pipeline"]
               if args.media else build_probe(native, env))
    marker = uuid.uuid4().hex
    sinks = [f"{PREFIX}{marker}-{suffix}" for suffix in ("a", "b")]
    baseline = {int(entry["index"]) for entry in module_inventory()}
    created = []
    monitors = []
    process = None
    cleanup_errors = []
    try:
        for sink in sinks:
            result = subprocess.run(
                ["pactl", "load-module", "module-null-sink", f"sink_name={sink}", "rate=48000", "channels=2",
                 "format=float32le", f"sink_properties=device.description=FluxerOutputRouting-{marker}"],
                check=True, capture_output=True, text=True, timeout=10)
            module_id = int(result.stdout.strip())
            if module_id in baseline:
                raise RuntimeError("Audio service returned a preexisting module ID")
            created.append(module_id)
        observed = [wait_for_sink(sink) for sink in sinks]
        env["FLUXER_OUTPUT_TEST_A"], env["FLUXER_OUTPUT_TEST_B"] = [f"output:pipewire:{node}" for node in observed]
        for sink in sinks:
            monitors.append(Monitor(sink))
        process = subprocess.Popen(command, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   text=True, bufsize=1, start_new_session=True)
        lines = queue.Queue()
        reader = threading.Thread(target=stdout_reader, args=(process, lines), daemon=True)
        reader.start()
        pid = None
        baseline_rms = None
        phases = ("A", "GAIN", "MUTE", "B", "STOPPED") if args.media else ("A", "B", "STOPPED")
        for phase in phases:
            pid = await_phase(process, lines, phase, pid)
            target = set() if phase == "STOPPED" else {observed[1] if phase == "B" else observed[0]}
            check_graph(pid, target)
            # MediaEngine can announce the selected output before the remote
            # decoder receives its first packets. Allow bounded warm-up without
            # accepting a silent baseline or hiding wrong-sink playback.
            audible_deadline = time.monotonic() + 8
            while True:
                a, b = measure_pair(monitors)
                if phase != "A" or a >= 0.01 or time.monotonic() >= audible_deadline:
                    break
                if b > 0.0001:
                    raise RuntimeError("Baseline audio reached the unselected sink")
            print(f"Virtual output phase {phase}: A RMS={a:.6f}, B RMS={b:.6f}", flush=True)
            if phase in ("MUTE", "STOPPED"):
                if max(a, b) > 0.0001:
                    raise RuntimeError(f"Output was not silent during {phase}")
            elif phase == "GAIN":
                if baseline_rms is None or not 0.14 <= a / baseline_rms <= 0.4 or b > 0.0001:
                    raise RuntimeError("Output gain did not scale actual selected-sink PCM")
            else:
                active, inactive = (b, a) if phase == "B" else (a, b)
                if active < 0.01 or inactive > 0.0001:
                    raise RuntimeError("Synthetic tone was not isolated to the selected sink")
                if phase == "A":
                    baseline_rms = a
            check_graph(pid, target)
            command = "NEXT" if args.media else {"A": "B", "B": "STOP", "STOPPED": "EXIT"}[phase]
            process.stdin.write(command + "\n")
            process.stdin.flush()
        process.stdin.close()
        process.wait(timeout=20)
        reader.join(timeout=5)
        while not lines.empty():
            line = lines.get_nowait()
            if line is not None:
                print(line, end="")
        if process.returncode:
            raise RuntimeError("Output probe failed after final handshake")
        print("Isolated output routing and stop checks passed.")
        return 0
    finally:
        terminate(process, group=True)
        for monitor in monitors:
            monitor.close()
        try:
            owned = {int(entry["index"]) for entry in module_inventory()
                     if int(entry["index"]) not in baseline and module_owned(entry, marker, {"module-null-sink"})}
            for module_id in owned:
                if module_id not in created:
                    created.append(module_id)
            for module_id in reversed(created):
                entry = next((entry for entry in module_inventory() if int(entry["index"]) == module_id), None)
                if entry is None or not module_owned(entry, marker, {"module-null-sink"}):
                    continue
                result = subprocess.run(["pactl", "unload-module", str(module_id)],
                                        capture_output=True, text=True, timeout=10)
                if result.returncode:
                    cleanup_errors.append(f"Could not unload this test's module {module_id}")
        except (subprocess.SubprocessError, OSError, ValueError) as error:
            cleanup_errors.append(f"Could not finish isolated-output cleanup: {error}")
        for error in cleanup_errors:
            print(error, file=sys.stderr)
        if cleanup_errors and sys.exc_info()[0] is None:
            raise RuntimeError("Isolated output cleanup needs attention")


class FixtureTests(unittest.TestCase):
    def test_sink_resolution_requires_observed_exact_node(self):
        name = PREFIX + "fixture-a"
        sinks = [{"name": name, "properties": {"object.serial": "31"}}]
        node = {"id": 7, "type": "PipeWire:Interface:Node", "info": {"props": {
            "node.name": name, "object.serial": 31, "media.class": "Audio/Sink"}}}
        self.assertEqual(resolve_sink(name, sinks, [node]), name)
        self.assertIsNone(resolve_sink(name, sinks, []))
        node["info"]["props"]["node.name"] = "physical-output"
        with self.assertRaises(RuntimeError):
            resolve_sink(name, sinks, [node])

    def test_graph_tracks_only_exact_process(self):
        objects = [
            {"id": 1, "type": "PipeWire:Interface:Client", "info": {"props": {"application.process.id": 42}}},
            {"id": 2, "type": "PipeWire:Interface:Node", "info": {"props": {
                "node.name": "probe", "client.id": 1, "media.class": "Stream/Output/Audio"}}},
            {"id": 3, "type": "PipeWire:Interface:Node", "info": {"props": {
                "node.name": "other-app", "application.process.id": 43, "media.class": "Stream/Output/Audio"}}},
            {"id": 4, "type": "PipeWire:Interface:Node", "info": {"props": {"node.name": PREFIX + "a"}}},
            {"id": 5, "type": "PipeWire:Interface:Node", "info": {"props": {"node.name": "physical"}}},
            {"id": 6, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 2, "input-node-id": 4}},
            {"id": 7, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 3, "input-node-id": 5}},
        ]
        self.assertEqual(output_targets(objects, 42), ({2}, {PREFIX + "a"}))
        self.assertEqual(output_targets(objects, 99), (set(), set()))

    def test_cleanup_ownership_requires_marker_and_module_type(self):
        self.assertTrue(module_owned({"name": "module-null-sink", "argument": "sink_name=fixture-uuid"},
                                     "uuid", {"module-null-sink"}))
        self.assertFalse(module_owned({"name": "module-null-sink", "argument": "sink_name=unrelated"},
                                      "uuid", {"module-null-sink"}))
        self.assertFalse(module_owned({"name": "module-loopback", "argument": "fixture-uuid"},
                                      "uuid", {"module-null-sink"}))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(130)
    except (RuntimeError, subprocess.SubprocessError, OSError, ValueError) as error:
        print(f"Output routing test failed: {error}", file=sys.stderr)
        raise SystemExit(1)
