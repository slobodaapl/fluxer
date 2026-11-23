#!/usr/bin/env python3
"""Click, drag or scroll a virtual Wayland pointer in headless_smoke.sh."""
import os
from pathlib import Path
import socket
import struct
import sys
import time


def send(wayland, object_id, opcode, payload=b""):
    wayland.sendall(struct.pack("=II", object_id, (len(payload) + 8) << 16 | opcode) + payload)


def words(*values):
    return struct.pack(f"={len(values)}I", *values)


def events(wayland):
    with wayland.makefile("rb") as wire:
        while header := wire.read(8):
            object_id, size_and_opcode = struct.unpack("=II", header)
            yield object_id, size_and_opcode & 0xFFFF, wire.read((size_and_opcode >> 16) - 8)


def main():
    runtime = Path(os.environ["XDG_RUNTIME_DIR"])
    if (
        os.environ.get("WLR_BACKENDS") != "headless"
        or not runtime.name.startswith("fluxer-headless.")
        or os.environ.get("WAYLAND_DISPLAY") != "wayland-0"
    ):
        raise SystemExit("Run inside the private headless smoke session")

    if len(sys.argv) not in (3, 4, 5):
        raise SystemExit("Usage: headless_pointer.py X Y [right | SCROLL_PIXELS | END_X END_Y]")
    x, y = map(int, sys.argv[1:3])
    right_click = sys.argv[3:] == ["right"]
    action = [] if right_click else list(map(int, sys.argv[3:]))
    width, height = 1280, 720
    positions = [(x, y)]
    if len(action) == 2:
        positions.append(tuple(action))
    if any(not (0 <= px < width and 0 <= py < height) for px, py in positions):
        raise SystemExit("Pointer position is outside the 1280x720 headless output")

    manager_name = b"zwlr_virtual_pointer_manager_v1"
    # wl_display.get_registry and sync. Drain registry globals through the sync reply.
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as wayland:
        wayland.settimeout(3)
        wayland.connect(str(runtime / os.environ["WAYLAND_DISPLAY"]))
        send(wayland, 1, 1, words(2))
        send(wayland, 1, 0, words(3))
        manager = None
        reply = events(wayland)
        for object_id, opcode, payload in reply:
            if object_id == 3 and opcode == 0:
                break
            if object_id == 2 and opcode == 0:
                name, length = struct.unpack_from("=II", payload)
                if payload[8:8 + length - 1] == manager_name:
                    manager = name
        if manager is None:
            raise SystemExit("Umbriel did not advertise a virtual pointer manager")

        interface = manager_name + b"\0"
        interface += b"\0" * (-len(interface) % 4)
        # wl_registry.bind, followed by create_virtual_pointer without a seat hint.
        send(wayland, 2, 0, words(manager, len(manager_name) + 1) + interface + words(1, 4))
        send(wayland, 4, 0, words(0, 5))
        timestamp = int(time.monotonic() * 1000) & 0xFFFFFFFF
        send(wayland, 5, 1, words(timestamp, x, y, width, height))
        send(wayland, 5, 4)
        if len(action) == 1:
            # A vertical finger scroll uses pixel deltas in Wayland fixed-point units.
            send(wayland, 5, 5, words(1))
            send(wayland, 5, 3, words(timestamp, 0, (action[0] * 256) & 0xFFFFFFFF))
            send(wayland, 5, 4)
            send(wayland, 5, 6, words(timestamp, 0))
            send(wayland, 5, 4)
        else:
            button = 0x111 if right_click else 0x110  # Linux BTN_RIGHT / BTN_LEFT
            send(wayland, 5, 2, words(timestamp, button, 1))
            send(wayland, 5, 4)
            if action:
                end_x, end_y = action
                time.sleep(0.1)
                timestamp = int(time.monotonic() * 1000) & 0xFFFFFFFF
                send(wayland, 5, 1, words(timestamp, end_x, end_y, width, height))
                send(wayland, 5, 4)
            send(wayland, 5, 2, words(timestamp, button, 0))
            send(wayland, 5, 4)
        send(wayland, 1, 0, words(6))
        for object_id, opcode, payload in reply:
            if object_id == 1 and opcode == 0:
                raise SystemExit(f"Wayland protocol error: {payload!r}")
            if object_id == 6 and opcode == 0:
                break
        else:
            raise SystemExit("Compositor disconnected before processing the pointer input")


if __name__ == "__main__":
    main()
