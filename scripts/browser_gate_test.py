#!/usr/bin/env python3
"""Unit tests for the repository-owned browser gate WebSocket client.

Every test drives the real client against a real loopback socket that speaks
the parts of RFC 6455 the Chrome DevTools Protocol actually uses, so frame
reassembly is proved end to end rather than asserted about in the abstract.
"""

from __future__ import annotations

import socket
import struct
import sys
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from browser_gate import GateFailure, WebSocket  # noqa: E402

TEXT = 0x1
BINARY = 0x2
CONTINUATION = 0x0
CLOSE = 0x8
PING = 0x9
PONG = 0xA


def frame(opcode: int, payload: bytes, final: bool = True) -> bytes:
    """One unmasked server frame, exactly as a browser would send it."""
    header = bytearray([(0x80 if final else 0x00) | opcode])
    length = len(payload)
    if length < 126:
        header.append(length)
    elif length < (1 << 16):
        header.append(126)
        header += struct.pack(">H", length)
    else:
        header.append(127)
        header += struct.pack(">Q", length)
    return bytes(header) + payload


def announced_frame(opcode: int, length: int) -> bytes:
    """A frame header that claims a payload it never sends."""
    return bytes([0x80 | opcode, 127]) + struct.pack(">Q", length)


class FrameServer:
    """A loopback server that completes the upgrade then replays raw frames."""

    def __init__(self, payload: bytes) -> None:
        self._payload = payload
        self._listener = socket.socket()
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", 0))
        self._listener.listen(1)
        self.port = self._listener.getsockname()[1]
        self.received = bytearray()
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    @property
    def url(self) -> str:
        return f"ws://127.0.0.1:{self.port}/devtools/page/1"

    def _serve(self) -> None:
        try:
            connection, _ = self._listener.accept()
        except OSError:
            return
        with connection:
            request = b""
            while b"\r\n\r\n" not in request:
                chunk = connection.recv(4096)
                if not chunk:
                    return
                request += chunk
            connection.sendall(
                b"HTTP/1.1 101 Switching Protocols\r\n"
                b"Upgrade: websocket\r\n"
                b"Connection: Upgrade\r\n\r\n"
            )
            connection.sendall(self._payload)
            try:
                while True:
                    chunk = connection.recv(4096)
                    if not chunk:
                        break
                    self.received += chunk
            except OSError:
                pass

    def close(self) -> None:
        self._listener.close()

    def wait_for_reply(self, minimum: int, timeout: float = 5.0) -> bytes:
        """The bytes the client sent back, once at least `minimum` arrive."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if len(self.received) >= minimum:
                return bytes(self.received)
            time.sleep(0.01)
        raise AssertionError(
            f"the client sent {len(self.received)} bytes, expected {minimum}"
        )


class WebSocketReassemblyTest(unittest.TestCase):
    def connect(self, payload: bytes, **options: object) -> WebSocket:
        server = FrameServer(payload)
        self.addCleanup(server.close)
        self.server = server
        client = WebSocket(server.url, timeout=5, **options)
        self.addCleanup(client.close)
        return client

    def test_a_single_text_frame_is_returned_whole(self) -> None:
        client = self.connect(frame(TEXT, b'{"id":1}'))

        self.assertEqual(client.receive(), '{"id":1}')

    def test_a_fragmented_text_message_is_reassembled_in_order(self) -> None:
        client = self.connect(
            frame(TEXT, b'{"id":', final=False)
            + frame(CONTINUATION, b"1,", final=False)
            + frame(CONTINUATION, b'"result":{}}')
        )

        self.assertEqual(client.receive(), '{"id":1,"result":{}}')

    def test_a_fragmented_binary_message_is_reassembled(self) -> None:
        client = self.connect(
            frame(BINARY, b'{"id"', final=False) + frame(CONTINUATION, b":2}")
        )

        self.assertEqual(client.receive(), '{"id":2}')

    def test_control_frames_interleaved_with_fragments_are_answered(self) -> None:
        client = self.connect(
            frame(TEXT, b"he", final=False)
            + frame(PING, b"keepalive")
            + frame(CONTINUATION, b"ll", final=False)
            + frame(PONG, b"")
            + frame(CONTINUATION, b"o")
        )

        self.assertEqual(client.receive(), "hello")
        # The ping must have been answered with a masked pong carrying the
        # same application data.
        reply = self.server.wait_for_reply(15)
        self.assertEqual(reply[0], 0x80 | PONG)
        self.assertEqual(reply[1] & 0x7F, len(b"keepalive"))
        mask = reply[2:6]
        unmasked = bytes(
            byte ^ mask[index % 4] for index, byte in enumerate(reply[6:15])
        )
        self.assertEqual(unmasked, b"keepalive")

    def test_two_messages_arrive_independently(self) -> None:
        client = self.connect(
            frame(TEXT, b"fir", final=False)
            + frame(CONTINUATION, b"st")
            + frame(TEXT, b"second")
        )

        self.assertEqual(client.receive(), "first")
        self.assertEqual(client.receive(), "second")

    def test_an_unexpected_continuation_frame_fails_clearly(self) -> None:
        client = self.connect(frame(CONTINUATION, b"orphan"))

        with self.assertRaises(GateFailure) as failure:
            client.receive()

        self.assertIn("continuation", str(failure.exception))

    def test_a_new_message_inside_an_unfinished_one_fails_clearly(self) -> None:
        client = self.connect(
            frame(TEXT, b"unfinished", final=False) + frame(TEXT, b"interrupting")
        )

        with self.assertRaises(GateFailure) as failure:
            client.receive()

        self.assertIn("unfinished", str(failure.exception))

    def test_a_fragmented_control_frame_fails_clearly(self) -> None:
        client = self.connect(frame(PING, b"split", final=False))

        with self.assertRaises(GateFailure) as failure:
            client.receive()

        self.assertIn("control frame", str(failure.exception))

    def test_an_oversized_reassembled_message_fails_instead_of_growing(self) -> None:
        client = self.connect(
            frame(TEXT, b"a" * 8, final=False)
            + frame(CONTINUATION, b"b" * 8, final=False)
            + frame(CONTINUATION, b"c" * 8),
            max_message_bytes=16,
        )

        with self.assertRaises(GateFailure) as failure:
            client.receive()

        self.assertIn("16", str(failure.exception))

    def test_an_oversized_announced_frame_fails_before_it_is_read(self) -> None:
        client = self.connect(announced_frame(TEXT, 1 << 40), max_message_bytes=1024)

        with self.assertRaises(GateFailure) as failure:
            client.receive()

        self.assertIn("1024", str(failure.exception))

    def test_a_close_frame_fails_instead_of_hanging(self) -> None:
        client = self.connect(frame(CLOSE, struct.pack(">H", 1006) + b"gone"))

        with self.assertRaises(GateFailure) as failure:
            client.receive()

        self.assertIn("closed", str(failure.exception))

    def test_an_unfinished_message_fails_instead_of_hanging(self) -> None:
        client = self.connect(frame(TEXT, b"truncated", final=False))

        with self.assertRaises(GateFailure) as failure:
            client.receive()

        self.assertIn("stopped answering", str(failure.exception))

    def test_a_masked_server_frame_is_refused(self) -> None:
        payload = b"masked"
        mask = b"\x01\x02\x03\x04"
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        client = self.connect(
            bytes([0x80 | TEXT, 0x80 | len(payload)]) + mask + masked
        )

        with self.assertRaises(GateFailure) as failure:
            client.receive()

        self.assertIn("masked", str(failure.exception))


if __name__ == "__main__":
    unittest.main(verbosity=2)
