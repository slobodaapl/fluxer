#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Opt-in decoded file PCM test; creates and monitors only two owned null sinks."""
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import uuid

native = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(native / "scripts"))
from test_output_routing import (PREFIX, Monitor, await_phase, check_graph, measure_pair,
                                 module_inventory, module_owned, stdout_reader, terminate, wait_for_sink)


def main():
    marker = uuid.uuid4().hex
    sinks = [f"{PREFIX}{marker}-{suffix}" for suffix in ("a", "b")]
    baseline = {int(entry["index"]) for entry in module_inventory()}
    monitors = []
    process = None
    try:
        for sink in sinks:
            result = subprocess.run(["pactl", "load-module", "module-null-sink", f"sink_name={sink}",
                                     "rate=48000", "channels=2", "format=float32le",
                                     f"sink_properties=device.description=FluxerFileOutput-{marker}"],
                                    check=True, capture_output=True, text=True, timeout=10)
            module_id = int(result.stdout.strip())
            if module_id in baseline:
                raise RuntimeError("Audio service returned a preexisting module")
        observed = [wait_for_sink(sink) for sink in sinks]
        env = os.environ.copy()
        env.setdefault("CARGO_INCREMENTAL", "0")
        env["FLUXER_OUTPUT_TEST_A"], env["FLUXER_OUTPUT_TEST_B"] = [f"output:pipewire:{name}" for name in observed]
        for sink in sinks:
            monitors.append(Monitor(sink))
        command = ["cargo", "test", "--manifest-path", str(native / "Cargo.toml"), "-p", "fluxer-media", "--test", "file_output", "--offline", "--", "--ignored", "--nocapture"]
        process = subprocess.Popen(command, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   text=True, bufsize=1, start_new_session=True)
        lines = queue.Queue()
        reader = threading.Thread(target=stdout_reader, args=(process, lines), daemon=True)
        reader.start()
        pid = None
        reference = None
        seek_rms = None
        for phase in ("A", "GAIN", "MUTE", "PAUSE", "SEEK", "B", "STOPPED"):
            pid = await_phase(process, lines, phase, pid)
            target = set() if phase == "STOPPED" else {observed[1] if phase == "B" else observed[0]}
            check_graph(pid, target)
            a, b = measure_pair(monitors)
            print(f"File output {phase}: A RMS={a:.6f}, B RMS={b:.6f}", flush=True)
            if phase in ("MUTE", "PAUSE", "STOPPED"):
                if max(a, b) > 0.0001: raise RuntimeError(f"Expected silence during {phase}")
            elif phase == "A":
                if a < 0.1 or b > 0.0001: raise RuntimeError("Tone did not reach only selected sink A")
                reference = a
            elif phase == "GAIN":
                if not 0.2 <= a / reference <= 0.3 or b > 0.0001: raise RuntimeError("Volume failed")
            elif phase == "SEEK":
                if not 0.35 <= a / reference <= 0.45 or b > 0.0001: raise RuntimeError("Seek did not reach second tone section")
                seek_rms = a
            elif phase == "B":
                if a > 0.0001 or not 0.9 <= b / seek_rms <= 1.1: raise RuntimeError("Output switch failed")
            check_graph(pid, target)
            process.stdin.write("NEXT\n"); process.stdin.flush()
        process.stdin.close()
        if process.wait(timeout=20): raise RuntimeError("File output probe failed")
        print("Isolated decoded-file output checks passed.")
    finally:
        terminate(process, group=True)
        for monitor in monitors: monitor.close()
        # Cleanup only modules that still carry our unique ownership marker.
        for entry in module_inventory():
            module_id = int(entry["index"])
            if module_id not in baseline and module_owned(entry, marker, {"module-null-sink"}):
                subprocess.run(["pactl", "unload-module", str(module_id)], check=True, timeout=10)

if __name__ == "__main__":
    main()
