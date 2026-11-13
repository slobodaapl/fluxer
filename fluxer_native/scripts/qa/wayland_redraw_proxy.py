#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Test-only standard-library Wayland proxy for explicit offline-fixture use.

No personal or application protocol payloads are logged; diagnostics contain
socket paths, callback IDs, event counts and timing only. Never insert this proxy
into a personal application session.

Usage: wayland_redraw_proxy.py SOCKET_PATH UPSTREAM_PATH CONTROL_DIR
Create CONTROL_DIR/hold containing 'one' to delay one surface frame Done and
its display.delete_id per connection/activation, or 'all' (also empty/default)
to delay every surface frame Done. Create CONTROL_DIR/release to flush delayed
events in their original order. Other events continue flowing in both modes.
Remove release and touch/rewrite hold to start a new activation. Delays survive
removal of hold until release is created. Ctrl-C/SIGTERM cleans up owned FDs.
"""
import argparse
import array
from collections import deque
from dataclasses import dataclass, field
import json
import os
from pathlib import Path
import select
import signal
import socket
import struct
import sys
import threading
import time

HEADER = struct.Struct('=II')
U32 = struct.Struct('=I')
MAX_BUFFER = 16 * 1024 * 1024
MAX_FDS = 253
STOP = threading.Event()
LOG_LOCK = threading.Lock()


def log(event, **fields):
    with LOG_LOCK:
        print(json.dumps(dict(event=event, monotonic=round(time.monotonic(), 6), **fields)),
              file=sys.stderr, flush=True)


def close_fds(fds):
    for fd in fds:
        try:
            os.close(fd)
        except OSError:
            pass


@dataclass
class Write:
    data: bytes
    fds: list = field(default_factory=list)
    offset: int = 0


class Endpoint:
    def __init__(self, sock):
        self.sock = sock
        self.incoming = bytearray()
        self.pending_fds = []
        self.writes = deque()
        self.queued_bytes = 0

    def queue(self, data, fds=None):
        fds = [] if fds is None else fds
        if self.queued_bytes + len(data) > MAX_BUFFER:
            close_fds(fds)
            raise RuntimeError('outbound buffer limit exceeded')
        self.writes.append(Write(data, fds))
        self.queued_bytes += len(data)

    def flush(self):
        if not self.writes:
            return
        current = self.writes[0]
        ancillary = []
        if current.fds:
            ancillary = [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', current.fds))]
        try:
            sent = self.sock.sendmsg([memoryview(current.data)[current.offset:]], ancillary)
        except (BlockingIOError, InterruptedError):
            return
        if sent <= 0:
            raise ConnectionError('zero-byte write')
        # Ancillary descriptors are transferred with the first successful byte.
        close_fds(current.fds)
        current.fds = []
        current.offset += sent
        self.queued_bytes -= sent
        if current.offset == len(current.data):
            self.writes.popleft()

    def close(self):
        close_fds(self.pending_fds)
        self.pending_fds = []
        while self.writes:
            close_fds(self.writes.popleft().fds)
        self.sock.close()


class ProxyConnection:
    def __init__(self, client, upstream, control, number):
        self.client = Endpoint(client)
        self.upstream = Endpoint(upstream)
        self.control = control
        self.number = number
        self.objects = {1: 'wl_display'}
        self.frame_callbacks = set()
        self.held_ids = set()
        self.held = deque()
        self.held_bytes = 0
        self.activation = None
        self.mode = None
        self.one_consumed = False
        self.held_done_count = 0
        self.forwarded_done_count = 0
        self.release_seen = False

    def update_control(self):
        released = (self.control / 'release').exists()
        if released:
            if self.held:
                count = len(self.held)
                while self.held:
                    self.client.queue(self.held.popleft())
                self.held_bytes = 0
                log('released', connection=self.number, events=count,
                    callbacks=len(self.held_ids))
            self.held_ids.clear()
            self.activation = None
            self.mode = None
            self.release_seen = True
            return
        self.release_seen = False
        try:
            hold = self.control / 'hold'
            stat = hold.stat()
            content = hold.read_text().strip().lower()
        except FileNotFoundError:
            self.activation = None
            self.mode = None
            return
        token = (stat.st_ino, stat.st_mtime_ns, content)
        if token != self.activation:
            self.activation = token
            self.mode = 'one' if content == 'one' else 'all'
            self.one_consumed = False
            log('hold_active', connection=self.number, mode=self.mode)

    def request(self, message):
        object_id, word = HEADER.unpack_from(message)
        opcode = word & 0xffff
        kind = self.objects.get(object_id)
        if kind == 'wl_display' and opcode in (0, 1) and len(message) >= 12:
            new_id = U32.unpack_from(message, 8)[0]
            self.objects[new_id] = 'wl_callback' if opcode == 0 else 'wl_registry'
        elif kind == 'wl_registry' and opcode == 0 and len(message) >= 24:
            length = U32.unpack_from(message, 12)[0]
            end = 16 + length
            version_offset = 16 + ((length + 3) & ~3)
            if length == 0 or end > len(message) or version_offset + 8 > len(message):
                raise RuntimeError('invalid wl_registry.bind request')
            interface = message[16:end - 1].decode('ascii')
            new_id = U32.unpack_from(message, version_offset + 4)[0]
            self.objects[new_id] = interface
        elif kind == 'wl_compositor' and opcode == 0 and len(message) >= 12:
            self.objects[U32.unpack_from(message, 8)[0]] = 'wl_surface'
        elif kind == 'wl_surface' and opcode == 3 and len(message) >= 12:
            callback = U32.unpack_from(message, 8)[0]
            self.objects[callback] = 'wl_callback'
            self.frame_callbacks.add(callback)
        elif kind == 'wl_surface' and opcode == 0:
            self.objects.pop(object_id, None)

    def event(self, message):
        object_id, word = HEADER.unpack_from(message)
        opcode = word & 0xffff
        hold = False
        if object_id in self.frame_callbacks and opcode == 0:
            hold = self.mode == 'all' or (self.mode == 'one' and not self.one_consumed)
            if hold:
                self.one_consumed = True
                self.held_ids.add(object_id)
                self.held_done_count += 1
                log('held_done', connection=self.number, callback=object_id,
                    mode=self.mode, held_total=self.held_done_count)
            else:
                self.forwarded_done_count += 1
                log('forwarded_done', connection=self.number, callback=object_id,
                    forwarded_total=self.forwarded_done_count)
        elif object_id == 1 and opcode == 1 and len(message) >= 12:
            deleted_id = U32.unpack_from(message, 8)[0]
            hold = deleted_id in self.held_ids
            self.frame_callbacks.discard(deleted_id)
            self.objects.pop(deleted_id, None)
        if hold:
            if self.held_bytes + len(message) > MAX_BUFFER:
                raise RuntimeError('held event buffer limit exceeded')
            self.held.append(message)
            self.held_bytes += len(message)
            return False
        return True

    def receive(self, source, destination, server):
        flags = getattr(socket, 'MSG_CMSG_CLOEXEC', 0)
        data, ancillary, recv_flags, _ = source.sock.recvmsg(
            65536, socket.CMSG_SPACE(MAX_FDS * array.array('i').itemsize), flags)
        received_fds = []
        for level, kind, payload in ancillary:
            if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                values = array.array('i')
                values.frombytes(payload[:len(payload) - len(payload) % values.itemsize])
                received_fds.extend(values)
        if recv_flags & socket.MSG_CTRUNC:
            close_fds(received_fds)
            raise RuntimeError('ancillary descriptors truncated')
        if not flags:
            for fd in received_fds:
                os.set_inheritable(fd, False)
        source.pending_fds.extend(received_fds)
        if not data:
            raise EOFError()
        source.incoming.extend(data)
        if len(source.incoming) > MAX_BUFFER or len(source.pending_fds) > MAX_FDS:
            raise RuntimeError('incoming buffer limit exceeded')
        while len(source.incoming) >= 8:
            _, word = HEADER.unpack_from(source.incoming)
            length = word >> 16
            if length < 8 or length % 4:
                raise RuntimeError('invalid Wayland message size')
            if len(source.incoming) < length:
                break
            message = bytes(source.incoming[:length])
            del source.incoming[:length]
            forward = self.event(message) if server else (self.request(message) or True)
            if forward:
                # Wayland FDs form an independent ordered stream. Attaching all
                # received descriptors early to the next forwarded bytes preserves
                # their order; delayed callback/delete_id events never consume FDs.
                fds, source.pending_fds = source.pending_fds, []
                destination.queue(message, fds)

    def run(self):
        log('connected', connection=self.number)
        try:
            while not STOP.is_set():
                self.update_control()
                endpoints = (self.client, self.upstream)
                by_socket = {e.sock: e for e in endpoints}
                reads, writes, _ = select.select(
                    list(by_socket), [e.sock for e in endpoints if e.writes], [], 0.025)
                for sock in writes:
                    by_socket[sock].flush()
                for sock in reads:
                    source = by_socket[sock]
                    server = source is self.upstream
                    destination = self.client if server else self.upstream
                    self.receive(source, destination, server)
        except EOFError:
            pass
        except (OSError, RuntimeError, ValueError) as error:
            log('connection_error', connection=self.number, error=str(error))
        finally:
            self.client.close()
            self.upstream.close()
            log('closed', connection=self.number, withheld_events=len(self.held),
                held_done=self.held_done_count, forwarded_done=self.forwarded_done_count)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('socketpath', type=Path)
    parser.add_argument('upstream_path', type=Path)
    parser.add_argument('control_dir', type=Path)
    args = parser.parse_args()
    args.control_dir.mkdir(parents=True, exist_ok=True)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    threads = []
    bound = False
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, lambda *_: STOP.set())
    try:
        listener.bind(str(args.socketpath))
        bound = True
        listener.listen(8)
        listener.settimeout(0.2)
        log('listening', socket=str(args.socketpath), upstream=str(args.upstream_path))
        number = 0
        while not STOP.is_set():
            try:
                client, _ = listener.accept()
            except socket.timeout:
                continue
            upstream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                upstream.settimeout(5)
                upstream.connect(str(args.upstream_path))
            except OSError as error:
                log('connect_error', error=str(error))
                client.close()
                upstream.close()
                continue
            client.setblocking(False)
            upstream.setblocking(False)
            number += 1
            connection = ProxyConnection(client, upstream, args.control_dir, number)
            thread = threading.Thread(target=connection.run, daemon=True)
            threads = [t for t in threads if t.is_alive()]
            threads.append(thread)
            thread.start()
    finally:
        STOP.set()
        listener.close()
        for thread in threads:
            thread.join(1)
        if bound:
            args.socketpath.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
