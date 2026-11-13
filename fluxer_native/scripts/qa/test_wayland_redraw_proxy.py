#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Standard-library smoke test for the test-only Wayland redraw proxy.

Run explicitly with Python. All protocol bytes are synthetic offline fixtures;
private temporary UNIX sockets and socketpairs leave the real compositor and
personal sessions untouched. No personal protocol payloads are logged.
"""
import array
import importlib.util
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import time


def msg(obj, op, payload=b''):
    return struct.pack('=II', obj, ((8 + len(payload)) << 16) | op) + payload


def u32(*values):
    return struct.pack('=' + 'I' * len(values), *values)


def bind(new_id, interface):
    value = interface.encode() + b'\0'
    return msg(2, 0, u32(1, len(value)) + value + bytes(-len(value) % 4) + u32(4, new_id))


def exact(sock, length):
    data = b''
    while len(data) < length:
        part = sock.recv(length - len(data))
        assert part
        data += part
    return data


def no_data(sock):
    sock.settimeout(.1)
    try:
        data = sock.recv(1)
        raise AssertionError(f'unexpected bytes {data!r}')
    except socket.timeout:
        pass
    finally:
        sock.settimeout(2)


def recv_fd(sock, expected):
    data, anc, flags, _ = sock.recvmsg(65536, socket.CMSG_SPACE(4 * 253))
    assert not flags & socket.MSG_CTRUNC
    if len(data) < len(expected):
        data += exact(sock, len(expected) - len(data))
    assert data == expected, (data, expected)
    fds = []
    for level, kind, payload in anc:
        if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
            a = array.array('i')
            a.frombytes(payload)
            fds += a.tolist()
    assert len(fds) == 1, fds
    try:
        assert os.pread(fds[0], 4, 0) == b'test'
    finally:
        for fd in fds:
            os.close(fd)


def main():
    proxy_path = Path(__file__).resolve().with_name("wayland_redraw_proxy.py")
    # Force partial sendmsg writes: ancillary rights must be sent once, then closed.
    spec = importlib.util.spec_from_file_location('fluxer_redraw_proxy', str(proxy_path))
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    left, right = socket.socketpair()
    left.setblocking(False)
    right.setblocking(False)
    left.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 1024)
    endpoint = module.Endpoint(left)
    fd = os.memfd_create('partial-send-smoke', os.MFD_CLOEXEC)
    os.write(fd, b'test')
    payload = bytes(range(256)) * 256
    endpoint.queue(payload, [fd])
    endpoint.flush()
    assert endpoint.writes and 0 < endpoint.writes[0].offset < len(payload)
    try:
        os.fstat(fd)
        raise AssertionError('local transferred FD was not closed')
    except OSError:
        pass
    received, descriptor_count = b'', 0
    while endpoint.writes or len(received) < len(payload):
        try:
            data, anc, _, _ = right.recvmsg(65536, socket.CMSG_SPACE(253 * 4))
            received += data
            for level, kind, raw in anc:
                if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                    values = array.array('i')
                    values.frombytes(raw)
                    descriptor_count += len(values)
                    for received_fd in values:
                        assert os.pread(received_fd, 4, 0) == b'test'
                        os.close(received_fd)
        except BlockingIOError:
            pass
        endpoint.flush()
    assert received == payload and descriptor_count == 1
    # Closing a connection also closes unsent descriptor copies.
    fd = os.memfd_create('unsent-fd-smoke', os.MFD_CLOEXEC)
    endpoint.queue(b'unwritten', [fd])
    endpoint.close()
    right.close()
    try:
        os.fstat(fd)
        raise AssertionError('unsent FD was not closed')
    except OSError:
        pass
    print('PASS: partial sendmsg; ancillary sent once; transferred/unsent FD cleanup')

    with tempfile.TemporaryDirectory(prefix='fluxer-wayland-proxy-smoke-') as directory:
        root = Path(directory)
        control = root / 'control'
        control.mkdir()
        (control / 'hold').write_text('one')
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(root / 'upstream'))
        listener.listen(1)
        listener.settimeout(3)
        with (root / 'log').open('w+') as log:
            process = subprocess.Popen([sys.executable, str(proxy_path), str(root / 'proxy'),
                                        str(root / 'upstream'), str(control)], stderr=log)
            try:
                end = time.monotonic() + 3
                while not (root / 'proxy').exists():
                    assert time.monotonic() < end
                    time.sleep(.01)
                client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                client.connect(str(root / 'proxy'))
                client.settimeout(2)
                server, _ = listener.accept()
                server.settimeout(2)
                requests = (msg(1, 1, u32(2)) + bind(3, 'wl_compositor') +
                            msg(3, 0, u32(4)) + msg(4, 3, u32(5)) +
                            msg(1, 0, u32(6)) + msg(4, 3, u32(7)))
                # Headers and string fields split across reads.
                client.sendall(requests[:13])
                time.sleep(.03)
                client.sendall(requests[13:])
                assert exact(server, len(requests)) == requests
                # Hold only frame callback5; sync6 and replacement frame7 must pass.
                done5, delete5 = msg(5, 0, u32(100)), msg(1, 1, u32(5))
                passed = msg(6, 0, u32(101)) + msg(1, 1, u32(6)) + msg(7, 0, u32(102)) + msg(1, 1, u32(7))
                server.sendall(done5 + delete5 + passed)
                assert exact(client, len(passed)) == passed
                no_data(client)
                # Pass an FD in both directions while frame5 remains held.
                fd = os.memfd_create('proxy-smoke', os.MFD_CLOEXEC)
                os.write(fd, b'test')
                try:
                    request = msg(9, 0, u32(4))
                    client.sendmsg([request], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [fd]))])
                    recv_fd(server, request)
                    event = msg(10, 0, u32(1, 4))
                    server.sendmsg([event], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [fd]))])
                    recv_fd(client, event)
                finally:
                    os.close(fd)
                (control / 'release').touch()
                assert exact(client, len(done5 + delete5)) == done5 + delete5
                no_data(client)
                # Rearm all mode; non-callback surface events pass while callbacks wait.
                (control / 'hold').write_text('all')
                (control / 'release').unlink()
                time.sleep(.06)
                request = msg(4, 3, u32(11)) + msg(4, 3, u32(12))
                client.sendall(request)
                assert exact(server, len(request)) == request
                withheld = msg(11, 0, u32(103)) + msg(1, 1, u32(11)) + msg(12, 0, u32(104)) + msg(1, 1, u32(12))
                passthrough = msg(4, 0, u32(20))
                server.sendall(withheld + passthrough)
                assert exact(client, len(passthrough)) == passthrough
                no_data(client)
                (control / 'release').touch()
                assert exact(client, len(withheld)) == withheld
                client.close()
                server.close()
            finally:
                process.terminate()
                process.wait(timeout=4)
                listener.close()
            assert process.returncode == 0
            log.seek(0)
            output = log.read()
            assert 'connection_error' not in output, output
            assert output.count('"event": "held_done"') == 3, output
            assert output.count('"event": "released"') == 2, output
            print(output, end='')
            print('PASS: fragmented framing; one/all suppression; sync passthrough; ordered release; bidirectional SCM_RIGHTS; clean shutdown')


if __name__ == "__main__":
    main()
