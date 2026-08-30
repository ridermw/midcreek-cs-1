#!/usr/bin/env python3
"""Unit tests for the repository-owned browser gate WebSocket client.

Every test drives the real client against a real loopback socket that speaks
the parts of RFC 6455 the Chrome DevTools Protocol actually uses, so frame
reassembly is proved end to end rather than asserted about in the abstract.
"""

from __future__ import annotations

import contextlib
import io
import json
import socket
import shutil
import struct
import sys
import tempfile
import threading
import time
import unittest
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from browser_gate import GateFailure, WebSocket  # noqa: E402
import browser_gate  # noqa: E402

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


# ---------------------------------------------------------------------------
# Bounded reads
# ---------------------------------------------------------------------------


class TricklingServer:
    """A loopback server that answers, slowly, and never stops."""

    def __init__(
        self, *, upgrade: bool = True, preamble: bytes = b"", chunk: bytes = b"\x00"
    ) -> None:
        self._upgrade = upgrade
        self._preamble = preamble
        self._chunk = chunk
        self._listener = socket.socket()
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", 0))
        self._listener.listen(1)
        self.port = self._listener.getsockname()[1]
        self.sent = 0
        self._stop = threading.Event()
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
                try:
                    chunk = connection.recv(4096)
                except OSError:
                    return
                if not chunk:
                    return
                request += chunk
            if self._upgrade:
                connection.sendall(
                    b"HTTP/1.1 101 Switching Protocols\r\n"
                    b"Upgrade: websocket\r\n"
                    b"Connection: Upgrade\r\n\r\n"
                )
                connection.sendall(self._preamble)
            while not self._stop.is_set():
                try:
                    connection.sendall(self._chunk)
                except OSError:
                    return
                self.sent += len(self._chunk)
                time.sleep(0.01)

    def close(self) -> None:
        self._stop.set()
        self._listener.close()


class HeaderFloodServer:
    """A loopback server that answers the upgrade with endless headers."""

    def __init__(self) -> None:
        self._listener = socket.socket()
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", 0))
        self._listener.listen(1)
        self.port = self._listener.getsockname()[1]
        self._stop = threading.Event()
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
                try:
                    chunk = connection.recv(4096)
                except OSError:
                    return
                if not chunk:
                    return
                request += chunk
            connection.sendall(b"HTTP/1.1 101 Switching Protocols\r\n")
            while not self._stop.is_set():
                try:
                    connection.sendall(b"X-Pad: " + b"p" * 1000 + b"\r\n")
                except OSError:
                    return

    def close(self) -> None:
        self._stop.set()
        self._listener.close()


class BoundedReadTest(unittest.TestCase):
    """One deadline bounds the handshake, every read, and every call."""

    def test_the_deadline_starts_before_the_handshake(self) -> None:
        server = TricklingServer()
        self.addCleanup(server.close)

        started = time.monotonic()
        with self.assertRaises(GateFailure) as failure:
            browser_gate.WebSocket(server.url, deadline=browser_gate.Deadline(0.0))

        self.assertIn("budget", str(failure.exception))
        self.assertIn("connecting", str(failure.exception))
        self.assertLess(time.monotonic() - started, 1.0)

    def test_a_trickling_stream_cannot_outlive_the_budget(self) -> None:
        """The defect this guards: every read is prompt, the message never ends.

        A byte at a time keeps each read comfortably inside its own timeout
        forever, so only a deadline that no read can refresh ends this. The
        server announces a real 60 000 byte text frame and then never finishes
        sending it, which is a perfectly well-formed message that never ends.
        """
        announced = bytes([0x01, 126]) + struct.pack(">H", 60000)
        server = TricklingServer(preamble=announced, chunk=b"a")
        self.addCleanup(server.close)
        client = browser_gate.WebSocket(
            server.url, timeout=5, deadline=browser_gate.Deadline(0.5)
        )
        self.addCleanup(client.close)

        started = time.monotonic()
        with self.assertRaises(GateFailure) as failure:
            client.receive()
        elapsed = time.monotonic() - started

        self.assertIn("budget", str(failure.exception))
        self.assertLess(elapsed, 5.0, "the trickle bought itself more time")

    def test_a_slow_handshake_cannot_outlive_the_budget(self) -> None:
        server = TricklingServer(upgrade=False, chunk=b"X")
        self.addCleanup(server.close)

        started = time.monotonic()
        with self.assertRaises(GateFailure) as failure:
            browser_gate.WebSocket(
                server.url, timeout=5, deadline=browser_gate.Deadline(0.5)
            )
        elapsed = time.monotonic() - started

        self.assertIn("budget", str(failure.exception))
        self.assertLess(elapsed, 5.0)

    def test_an_endless_header_block_is_refused_before_the_budget_runs_out(self) -> None:
        server = HeaderFloodServer()
        self.addCleanup(server.close)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.WebSocket(
                server.url, timeout=5, deadline=browser_gate.Deadline(30.0)
            )

        message = str(failure.exception)
        self.assertIn("header bytes", message)
        self.assertIn(str(browser_gate.MAX_HANDSHAKE_BYTES), message)


class SessionBudgetTest(unittest.TestCase):
    """Every DevTools call is bounded by what is left of the whole gate."""

    def session(self, payload: bytes, budget: float) -> browser_gate.DevTools:
        server = FrameServer(payload)
        self.addCleanup(server.close)
        self.server = server
        session = browser_gate.DevTools(
            server.url, deadline=browser_gate.Deadline(budget)
        )
        self.addCleanup(session.close)
        return session

    def test_a_call_past_the_budget_never_reaches_the_browser(self) -> None:
        session = self.session(b"", budget=5.0)
        session.deadline._expires = time.monotonic() - 1.0

        with self.assertRaises(GateFailure) as failure:
            session.call("Page.enable")

        self.assertIn("budget", str(failure.exception))
        self.assertEqual(
            len(self.server.received), 0, "an expired session sends nothing"
        )

    def test_a_call_is_bounded_by_the_remaining_budget_not_its_own_timeout(self) -> None:
        chatter = frame(TEXT, b'{"id":9999}') * 2000
        session = self.session(chatter, budget=0.2)

        started = time.monotonic()
        with self.assertRaises(GateFailure):
            session.call("Page.enable", timeout=browser_gate.BROWSER_TIMEOUT_SECONDS)
        elapsed = time.monotonic() - started

        self.assertLess(
            elapsed,
            5.0,
            "the call waited on its own timeout instead of the gate budget",
        )


# ---------------------------------------------------------------------------
# Keyboard ownership
# ---------------------------------------------------------------------------


class FakePage:
    """A scriptable page that answers the gate's own DevTools evaluations.

    The model encodes the browser behaviour the gate was measured against: a
    key only runs its scrolling default action while the element that owns
    focus lets it through, and a key that types a character only scrolls when
    the trusted character event is dispatched alongside the raw key down.

    It is bound to a scope, so the very same fake proves the phases against the
    standalone document and against the embedded one.
    """

    TYPING_KEYS = {"Space", "KeyQ", "KeyE"}

    def __init__(
        self,
        *,
        scope: browser_gate.Scope = browser_gate.TOP_SCOPE,
        scroll_height: float = 2400.0,
        inner_height: float = 900.0,
        probe: str | None = "scroll-probe",
        probe_inside_canvas: bool = False,
        probe_takes_focus: bool = True,
        probe_scrolls: tuple[str, ...] = ("ArrowDown", "Space"),
        canvas_scrolls: tuple[str, ...] = (),
        canvas_takes_focus: bool = True,
        canvas_keeps_focus: bool = True,
        scroll_step: float = 40.0,
    ) -> None:
        self.scope = scope
        self.scroll_height = scroll_height
        self.inner_height = inner_height
        self.probe = probe
        self.probe_inside_canvas = probe_inside_canvas
        self.probe_takes_focus = probe_takes_focus
        self.probe_scrolls = probe_scrolls
        self.canvas_scrolls = canvas_scrolls
        self.canvas_takes_focus = canvas_takes_focus
        self.canvas_keeps_focus = canvas_keeps_focus
        self.scroll_step = scroll_step

        self.scroll_y = 0.0
        self.active = "body"
        self.frames = 0
        self.dispatched: list[tuple[str, str, str]] = []
        self._pending: dict | None = None

    # -- Chrome DevTools Protocol surface ----------------------------------

    @property
    def label(self) -> str:
        return self.scope.label

    def js(self, template: str) -> str:
        return template

    def call(self, method: str, params: dict | None = None) -> dict:
        if method != "Input.dispatchKeyEvent":
            raise AssertionError(f"the gate should not call {method} here")
        params = params or {}
        kind = params["type"]
        code = params["code"]
        self.dispatched.append((self.active, kind, code))
        if kind == "rawKeyDown":
            self._pending = {"code": code, "typed": False}
        elif kind == "char":
            if self._pending and self._pending["code"] == code:
                self._pending["typed"] = True
        elif kind == "keyUp":
            pending, self._pending = self._pending, None
            if pending and pending["code"] == code:
                self._scroll_for(code, pending["typed"])
        return {}

    def _scroll_for(self, code: str, typed: bool) -> None:
        allowed = self.probe_scrolls if self.active == self.probe else self.canvas_scrolls
        if code not in allowed:
            return
        if code in self.TYPING_KEYS and not typed:
            return
        self.scroll_y = min(self.scroll_y + self.scroll_step, self.scroll_height)

    def evaluate(self, expression: str) -> object:
        if expression == self.js(browser_gate.NEXT_FRAMES_JS):
            self.frames += 1
            return True
        if expression == self.js(browser_gate.SCROLL_OFFSET_JS):
            return self.scroll_y
        if expression == self.js(browser_gate.SCROLL_TO_TOP_JS):
            self.scroll_y = 0.0
            return None
        if expression == self.js(browser_gate.SCROLL_RESERVE_JS):
            return self.scroll_height - self.inner_height
        if expression == self.js(browser_gate.SCROLL_REPORT_JS):
            return {
                "activeElement": self.active,
                "scrollY": self.scroll_y,
                "scrollHeight": self.scroll_height,
                "innerHeight": self.inner_height,
            }
        if expression == self.js(browser_gate.FOCUS_SCROLL_PROBE_JS):
            if self.probe is None:
                return {"probe": None}
            if self.probe_inside_canvas:
                return {"probe": self.probe, "insideCanvas": True}
            self.scroll_y = 0.0
            if self.probe_takes_focus:
                self.active = self.probe
            return {
                "probe": self.probe,
                "insideCanvas": False,
                "owned": self.probe_takes_focus,
            }
        if expression == self.js(browser_gate.FOCUS_CANVAS_JS):
            self.scroll_y = 0.0
            if self.canvas_takes_focus:
                self.active = "canvas"
            return self.canvas_takes_focus
        if expression == self.js(browser_gate.CANVAS_STILL_FOCUSED_JS):
            return self.canvas_keeps_focus and self.active == "canvas"
        if expression == self.js(browser_gate.PROBE_STILL_FOCUSED_JS):
            return self.probe is not None and self.active == self.probe
        raise AssertionError(f"the fake page cannot answer {expression!r}")

    # -- assertions helpers ------------------------------------------------

    def sequence_for(self, owner: str) -> list[tuple[str, str]]:
        return [(kind, code) for active, kind, code in self.dispatched if active == owner]


class FakeSocket:
    """A socket that records its bounds and never actually goes anywhere."""

    def __init__(self, *, handshake: bytes = b"", sendall_blocks: bool = False) -> None:
        self.timeouts: list[float] = []
        self.sent = bytearray()
        self.closed = False
        self._inbox = bytearray(handshake)
        self._sendall_blocks = sendall_blocks

    def settimeout(self, timeout: float) -> None:
        self.timeouts.append(timeout)

    def sendall(self, data: bytes) -> None:
        if self._sendall_blocks:
            raise TimeoutError("timed out")
        self.sent += data

    def recv(self, size: int) -> bytes:
        if not self._inbox:
            raise TimeoutError("timed out")
        chunk = bytes(self._inbox[:size])
        del self._inbox[:size]
        return chunk

    def close(self) -> None:
        self.closed = True


UPGRADE = (
    b"HTTP/1.1 101 Switching Protocols\r\n"
    b"Upgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
)


class TricklingBodyServer:
    """An HTTP server that answers 200 and then trickles a body forever."""

    def __init__(self) -> None:
        self._listener = socket.socket()
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", 0))
        self._listener.listen(1)
        self.port = self._listener.getsockname()[1]
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def _serve(self) -> None:
        try:
            connection, _ = self._listener.accept()
        except OSError:
            return
        with connection:
            try:
                connection.recv(4096)
                connection.sendall(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n"
                )
                while not self._stop.is_set():
                    # One byte at a time: every read is prompt, the body never
                    # ends, and an inactivity timer would never fire.
                    connection.sendall(b"x")
                    time.sleep(0.01)
            except OSError:
                return

    def close(self) -> None:
        self._stop.set()
        self._listener.close()


class BoundedWriteTest(unittest.TestCase):
    """Connecting and writing spend the same budget as reading."""

    def fake_connect(self, socket_factory) -> list:
        captured: list = []

        def connect(address, timeout=None):
            captured.append(timeout)
            return socket_factory()

        original = browser_gate.socket.create_connection
        browser_gate.socket.create_connection = connect
        self.addCleanup(
            setattr, browser_gate.socket, "create_connection", original
        )
        return captured

    def test_connecting_is_clamped_to_the_remaining_budget(self) -> None:
        captured = self.fake_connect(lambda: FakeSocket(handshake=UPGRADE))

        browser_gate.WebSocket(
            "ws://127.0.0.1:1/devtools/page/1",
            timeout=30,
            deadline=browser_gate.Deadline(0.5),
        )

        self.assertEqual(len(captured), 1)
        self.assertLessEqual(captured[0], 0.5)

    def test_a_connect_that_never_answers_is_a_gate_failure(self) -> None:
        def refuse():
            raise TimeoutError("timed out")

        self.fake_connect(refuse)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.WebSocket(
                "ws://127.0.0.1:1/devtools/page/1",
                timeout=30,
                deadline=browser_gate.Deadline(5.0),
            )

        self.assertIn("could not be reached", str(failure.exception))

    def test_a_handshake_that_cannot_be_written_is_bounded(self) -> None:
        fake = FakeSocket(handshake=UPGRADE, sendall_blocks=True)
        self.fake_connect(lambda: fake)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.WebSocket(
                "ws://127.0.0.1:1/devtools/page/1",
                timeout=30,
                deadline=browser_gate.Deadline(0.5),
            )

        self.assertIn("stopped accepting", str(failure.exception))
        self.assertTrue(fake.timeouts, "the write should have been bounded first")
        self.assertLessEqual(max(fake.timeouts), 0.5)

    def test_a_command_that_cannot_be_written_is_bounded(self) -> None:
        fake = FakeSocket(handshake=UPGRADE)
        self.fake_connect(lambda: fake)
        client = browser_gate.WebSocket(
            "ws://127.0.0.1:1/devtools/page/1",
            timeout=30,
            deadline=browser_gate.Deadline(0.5),
        )
        fake._sendall_blocks = True

        with self.assertRaises(GateFailure) as failure:
            client.send('{"id":1}')

        self.assertIn("stopped accepting", str(failure.exception))

    def test_a_write_past_the_budget_never_reaches_the_socket(self) -> None:
        fake = FakeSocket(handshake=UPGRADE)
        self.fake_connect(lambda: fake)
        client = browser_gate.WebSocket(
            "ws://127.0.0.1:1/devtools/page/1",
            timeout=30,
            deadline=browser_gate.Deadline(30.0),
        )
        sent = len(fake.sent)
        client._deadline._expires = time.monotonic() - 1.0

        with self.assertRaises(GateFailure) as failure:
            client.send('{"id":1}')

        self.assertIn("budget", str(failure.exception))
        self.assertEqual(len(fake.sent), sent, "an expired gate writes nothing")


class BoundedBodyTest(unittest.TestCase):
    """An HTTP body spends the total budget, not an inactivity timer."""

    def test_a_trickled_body_cannot_outlive_the_budget(self) -> None:
        server = TricklingBodyServer()
        self.addCleanup(server.close)
        deadline = browser_gate.Deadline(0.5)

        started = time.monotonic()
        with self.assertRaises(GateFailure) as failure:
            browser_gate.require_http_200(server.url, "", deadline)
        elapsed = time.monotonic() - started

        self.assertIn("budget", str(failure.exception))
        self.assertLess(elapsed, 5.0, "the trickled body reset the timer")

    def test_a_fetch_past_the_budget_is_never_attempted(self) -> None:
        server = TricklingBodyServer()
        self.addCleanup(server.close)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.require_http_200(server.url, "", browser_gate.Deadline(0.0))

        self.assertIn("budget", str(failure.exception))

    def test_a_real_response_exposes_the_socket_the_reads_are_clamped_on(self) -> None:
        """The clamp is only a guarantee if the socket is really reachable."""
        server = TricklingBodyServer()
        self.addCleanup(server.close)

        with urllib.request.urlopen(server.url, timeout=5) as response:
            connection = browser_gate.response_socket(response)

            self.assertIsNotNone(
                connection, "the body reads cannot be clamped to the deadline"
            )
            self.assertTrue(hasattr(connection, "settimeout"))

    def test_a_body_without_a_reclampable_socket_is_never_read(self) -> None:
        """Losing the socket must not fall back to the request's stale timeout."""

        class Response:
            def __init__(self) -> None:
                self.reads = 0

            def read1(self, size: int) -> bytes:
                self.reads += 1
                return b"body"

        response = Response()

        with self.assertRaises(GateFailure) as failure:
            browser_gate.read_bounded(
                response, 1_000_000, browser_gate.Deadline(30.0), "reading"
            )

        self.assertIn("re-clamped", str(failure.exception))
        self.assertEqual(response.reads, 0, "an unbounded body read must never start")

    def test_a_body_is_never_read_when_reclamping_the_socket_fails(self) -> None:
        """A dead socket cannot silently retain the timeout from urlopen."""

        class BrokenSocket:
            def settimeout(self, timeout: float) -> None:
                raise OSError("socket is closed")

        class Raw:
            _sock = BrokenSocket()

        class Response:
            fp = Raw()

            def __init__(self) -> None:
                self.reads = 0

            def read1(self, size: int) -> bytes:
                self.reads += 1
                return b"body"

        response = Response()

        with self.assertRaises(GateFailure) as failure:
            browser_gate.read_bounded(
                response, 1_000_000, browser_gate.Deadline(30.0), "reading"
            )

        self.assertIn("could not be re-clamped", str(failure.exception))
        self.assertIn("socket is closed", str(failure.exception))
        self.assertEqual(response.reads, 0, "a stale-timeout body read must never start")

    def test_every_body_read_is_re_clamped_to_what_is_left(self) -> None:
        """The defect this guards: a body that trickles and then stops dead.

        The connection was opened with the budget that was left at the time.
        Without re-clamping, a read that starts much later blocks on that
        original, far larger timeout.
        """

        class RecordingSocket:
            def __init__(self) -> None:
                self.timeouts: list[float] = []

            def settimeout(self, timeout: float) -> None:
                self.timeouts.append(timeout)

        class Raw:
            def __init__(self, connection: RecordingSocket) -> None:
                self._sock = connection

        class Response:
            def __init__(self, connection: RecordingSocket) -> None:
                self.fp = Raw(connection)
                self._chunks = [b"a" * 4096, b"b" * 4096, b""]

            def read1(self, size: int) -> bytes:
                return self._chunks.pop(0)

        connection = RecordingSocket()
        deadline = browser_gate.Deadline(30.0)

        browser_gate.read_bounded(Response(connection), 1_000_000, deadline, "reading")

        self.assertGreaterEqual(
            len(connection.timeouts), 3, "each read should have been clamped"
        )
        self.assertTrue(all(value <= 30.0 for value in connection.timeouts))
        self.assertTrue(
            connection.timeouts[-1] <= connection.timeouts[0],
            "the clamp should shrink with the remaining budget",
        )


class KeyboardOwnershipTest(unittest.TestCase):
    """Drives the real gate functions against the scriptable page."""

    SCOPE = browser_gate.TOP_SCOPE

    def setUp(self) -> None:
        self._budget = browser_gate.KEY_SCROLL_SECONDS
        self._reset = browser_gate.SCROLL_RESET_SECONDS
        browser_gate.KEY_SCROLL_SECONDS = 0.2
        browser_gate.SCROLL_RESET_SECONDS = 1.0

    def tearDown(self) -> None:
        browser_gate.KEY_SCROLL_SECONDS = self._budget
        browser_gate.SCROLL_RESET_SECONDS = self._reset

    def page(self, **options: object) -> FakePage:
        return FakePage(scope=self.SCOPE, **options)

    def press(self, page: FakePage) -> dict[str, float]:
        return browser_gate.press_control_keys(page)

    def prove(self, page: FakePage) -> dict:
        return browser_gate.check_control_keys_do_not_scroll(page)

    def test_every_control_key_is_a_trusted_rawkeydown_then_keyup(self) -> None:
        page = self.page()
        page.active = page.probe

        self.press(page)

        for code, _, _, text in browser_gate.CONTROL_KEYS:
            kinds = [kind for _, kind, pressed in page.dispatched if pressed == code]
            expected = ["rawKeyDown", "keyUp"] if text is None else ["rawKeyDown", "char", "keyUp"]
            self.assertEqual(kinds, expected, f"{code} was dispatched as {kinds}")

    def test_a_typing_key_carries_the_character_event_chrome_scrolls_on(self) -> None:
        page = self.page()
        page.active = page.probe

        deltas = self.press(page)

        self.assertEqual(deltas["Space"], 40.0)

    def test_the_positive_control_records_a_delta_for_every_control_key(self) -> None:
        page = self.page()

        summary = self.prove(page)

        self.assertEqual(
            sorted(summary["unfocused_deltas"]),
            sorted(code for code, _, _, _ in browser_gate.CONTROL_KEYS),
        )
        self.assertEqual(summary["probe"], "scroll-probe")
        self.assertEqual(summary["reserve_pixels"], 1500.0)
        for code in browser_gate.POSITIVE_CONTROL_KEYS:
            self.assertGreater(summary["unfocused_deltas"][code], 0.0)
        self.assertTrue(all(delta == 0.0 for delta in summary["focused_deltas"].values()))

    def test_the_focused_and_unfocused_phases_dispatch_identical_sequences(self) -> None:
        page = self.page()

        self.prove(page)

        self.assertEqual(
            page.sequence_for("scroll-probe"),
            page.sequence_for("canvas"),
            "the two phases must send the same keystrokes",
        )

    def test_a_page_without_a_scroll_probe_fails_before_the_positive_control(self) -> None:
        page = self.page(probe=None)

        with self.assertRaises(GateFailure) as failure:
            self.prove(page)

        self.assertIn("[data-scroll-probe]", str(failure.exception))
        self.assertEqual(page.dispatched, [])

    def test_a_probe_that_does_not_own_focus_fails(self) -> None:
        page = self.page(probe_takes_focus=False)

        with self.assertRaises(GateFailure) as failure:
            self.prove(page)

        self.assertIn("did not take keyboard focus", str(failure.exception))
        self.assertEqual(page.dispatched, [])

    def test_a_probe_inside_the_canvas_fails(self) -> None:
        page = self.page(probe_inside_canvas=True)

        with self.assertRaises(GateFailure) as failure:
            self.prove(page)

        self.assertIn("inside the canvas", str(failure.exception))

    def test_a_page_without_the_scroll_reserve_fails_before_anything_is_pressed(self) -> None:
        page = self.page(scroll_height=1000.0, inner_height=900.0)

        with self.assertRaises(GateFailure) as failure:
            self.prove(page)

        message = str(failure.exception)
        self.assertIn("reserves 100.0 scrollable pixels", message)
        self.assertEqual(page.dispatched, [])

    def test_a_positive_control_key_that_cannot_scroll_fails_with_per_key_deltas(self) -> None:
        page = self.page(probe_scrolls=("ArrowDown",))

        with self.assertRaises(GateFailure) as failure:
            self.prove(page)

        message = str(failure.exception)
        self.assertIn("['Space']", message)
        self.assertIn("ArrowDown +40", message)

    def test_a_typing_key_without_its_character_event_fails_the_positive_control(self) -> None:
        class SilentCharPage(FakePage):
            def call(self, method: str, params: dict | None = None) -> dict:
                if (params or {}).get("type") == "char":
                    return {}
                return super().call(method, params)

        page = SilentCharPage(scope=self.SCOPE)

        with self.assertRaises(GateFailure) as failure:
            self.prove(page)

        self.assertIn("['Space']", str(failure.exception))

    def test_a_focused_control_key_that_scrolls_fails_with_per_key_deltas(self) -> None:
        page = self.page(canvas_scrolls=("ArrowDown",))

        with self.assertRaises(GateFailure) as failure:
            self.prove(page)

        message = str(failure.exception)
        self.assertIn("['ArrowDown']", message)
        self.assertIn("must be owned by the game", message)

    def test_a_canvas_that_refuses_focus_fails(self) -> None:
        page = self.page(canvas_takes_focus=False)

        with self.assertRaises(GateFailure) as failure:
            self.prove(page)

        self.assertIn("refused keyboard focus", str(failure.exception))

    def test_a_canvas_that_loses_focus_fails(self) -> None:
        page = self.page(canvas_keeps_focus=False)

        with self.assertRaises(GateFailure) as failure:
            self.prove(page)

        self.assertIn("lost focus", str(failure.exception))

    def test_a_probe_that_loses_focus_during_the_positive_control_fails(self) -> None:
        """The positive control only measures what the probe actually received."""

        class DriftingPage(FakePage):
            def evaluate(self, expression: str) -> object:
                if expression == self.js(browser_gate.PROBE_STILL_FOCUSED_JS):
                    return False
                return super().evaluate(expression)

        with self.assertRaises(GateFailure) as failure:
            self.prove(DriftingPage(scope=self.SCOPE))

        self.assertIn("did not still own keyboard focus", str(failure.exception))

    def test_a_page_that_will_not_return_to_the_top_fails_loudly(self) -> None:
        class StuckPage(FakePage):
            def evaluate(self, expression: str) -> object:
                if expression == self.js(browser_gate.SCROLL_TO_TOP_JS):
                    return None
                return super().evaluate(expression)

        page = StuckPage(scope=self.SCOPE)
        page.scroll_y = 120.0

        with self.assertRaises(GateFailure) as failure:
            browser_gate.reset_scroll(page)

        self.assertIn("would not come to rest", str(failure.exception))
        self.assertIn("scrollY 120.0", str(failure.exception))

    def test_an_animated_scroll_is_measured_at_its_resting_place(self) -> None:
        """The defect this replaced: a busy renderer read as a settled page."""

        class AnimatedPage(FakePage):
            def __init__(self, **options: object) -> None:
                super().__init__(**options)
                self.target = 0.0
                self.stall_frames = 4

            def _scroll_for(self, code: str, typed: bool) -> None:
                allowed = self.probe_scrolls if self.active == self.probe else self.canvas_scrolls
                if code not in allowed or (code in self.TYPING_KEYS and not typed):
                    return
                self.target = self.scroll_y + self.scroll_step

            def evaluate(self, expression: str) -> object:
                if expression == self.js(browser_gate.NEXT_FRAMES_JS):
                    if self.stall_frames > 0:
                        self.stall_frames -= 1
                    elif self.scroll_y < self.target:
                        self.scroll_y = min(self.scroll_y + 8.0, self.target)
                    return True
                if expression == self.js(browser_gate.SCROLL_TO_TOP_JS):
                    self.target = 0.0
                return super().evaluate(expression)

        page = AnimatedPage(scope=self.SCOPE)
        page.active = page.probe

        delta = browser_gate.press_control_key(page, "ArrowDown", 40, "ArrowDown", None)

        self.assertEqual(delta, 40.0)
        self.assertEqual(page.scroll_y, 40.0)


class EmbeddedKeyboardOwnershipTest(KeyboardOwnershipTest):
    """Every assertion above, made against the embedded player instead.

    The published homepage runs the same package in an iframe, and this is what
    stops that from being proved by a weaker set of checks: the whole keyboard
    ownership suite is re-run through the embedded scope's JavaScript.
    """

    SCOPE = browser_gate.EMBED_SCOPE

    def test_the_embedded_scope_reaches_into_the_frame(self) -> None:
        rendered = self.SCOPE.render(browser_gate.SCROLL_OFFSET_JS)

        self.assertIn("contentWindow", rendered)
        self.assertNotIn("__WIN__", rendered)


class ScopeCoverageTest(unittest.TestCase):
    """The two scopes go through one implementation, not two."""

    def test_every_phase_template_is_scope_relative(self) -> None:
        source = (Path(__file__).resolve().parent / "browser_gate.py").read_text(
            encoding="utf-8"
        )
        body = source.split("# Document scopes")[1].split("# Entry point")[0]

        self.assertNotIn("document.getElementById(\"game-canvas\")", body)
        self.assertNotIn("window.scrollY", body)

    def test_the_embedded_scope_is_proved_by_the_same_functions(self) -> None:
        for template in (
            browser_gate.GAME_STATE_JS,
            browser_gate.BROWSER_ERRORS_JS,
            browser_gate.CANVAS_GEOMETRY_JS,
            browser_gate.SCROLL_RESERVE_JS,
            browser_gate.FOCUS_CANVAS_JS,
        ):
            rendered = browser_gate.EMBED_SCOPE.render(template)
            self.assertNotIn("__DOC__", rendered)
            self.assertNotIn("__WIN__", rendered)
            self.assertNotIn("__ORIGIN__", rendered)
            self.assertNotIn("__CLIP__", rendered)


class KeyboardOwnershipSourceTest(unittest.TestCase):
    """Structural guards that keep the two phases from drifting apart."""

    SOURCE = (Path(__file__).resolve().parent / "browser_gate.py").read_text(encoding="utf-8")

    def test_every_key_dispatch_goes_through_the_single_press_helper(self) -> None:
        """One keystroke helper, and the three events one keystroke really is."""
        self.assertEqual(self.SOURCE.count('session.call("Input.dispatchKeyEvent"'), 3)
        self.assertEqual(self.SOURCE.count("def press_control_key("), 1)
        self.assertEqual(self.SOURCE.count("def press_control_keys("), 1)

    def test_both_phases_go_through_the_same_helper(self) -> None:
        body = self.SOURCE.split("def check_control_keys_do_not_scroll(")[1]
        self.assertEqual(body.count("press_control_keys(session)"), 2)
        self.assertNotIn("Input.dispatchKeyEvent", body)

    def test_the_positive_control_names_the_keys_the_browser_guarantees(self) -> None:
        self.assertEqual(browser_gate.POSITIVE_CONTROL_KEYS, ("ArrowDown", "Space"))

    def test_the_standalone_and_embedded_proofs_are_one_function(self) -> None:
        body = self.SOURCE.split("def run_gate(")[1]

        self.assertEqual(body.count("prove_page("), 2)
        self.assertIn("TOP_SCOPE", body)
        self.assertIn("EMBED_SCOPE", body)


# ---------------------------------------------------------------------------
# Derived contracts
# ---------------------------------------------------------------------------


class PaletteRosterTest(unittest.TestCase):
    """The approved palette is the game's declared roster, never a count."""

    ROLES = """
        pub const RACK_WHITE: Srgba = srgba_u8!(0xFB, 0xFC, 0xFD);
        pub const WORKER_HI_VIS: Srgba = srgba_u8!(0xC8, 0xD9, 0x4A);
        impl PaletteRole {
            pub const ALL: [Self; 2] = [
                Self::RackWhite,
                Self::WorkerHiVis,
            ];
        }
    """

    def source(self, text: str) -> Path:
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, directory, True)
        path = directory / "design.rs"
        path.write_text(text, encoding="utf-8")
        return path

    def test_the_declared_roster_becomes_the_expected_constant_names(self) -> None:
        roles = browser_gate.read_palette_roles(self.source(self.ROLES))

        self.assertEqual(roles, {"RACK_WHITE", "WORKER_HI_VIS"})

    def test_the_real_design_source_matches_its_own_roster(self) -> None:
        design = Path(__file__).resolve().parent.parent / "src" / "design.rs"

        palette = browser_gate.read_palette(design)

        self.assertEqual(set(palette), browser_gate.read_palette_roles(design))

    def test_a_role_without_a_constant_fails_instead_of_shrinking_the_palette(self) -> None:
        text = self.ROLES.replace(
            "pub const WORKER_HI_VIS: Srgba = srgba_u8!(0xC8, 0xD9, 0x4A);", ""
        )

        with self.assertRaises(GateFailure) as failure:
            browser_gate.read_palette(self.source(text))

        self.assertIn("WORKER_HI_VIS", str(failure.exception))

    def test_a_constant_outside_the_roster_fails_instead_of_being_judged_against(self) -> None:
        text = self.ROLES + "\npub const STRAY_PINK: Srgba = srgba_u8!(0xFF, 0x00, 0xFF);\n"

        with self.assertRaises(GateFailure) as failure:
            browser_gate.read_palette(self.source(text))

        self.assertIn("STRAY_PINK", str(failure.exception))

    def test_a_roster_that_miscounts_itself_is_refused(self) -> None:
        text = self.ROLES.replace("[Self; 2]", "[Self; 3]")

        with self.assertRaises(GateFailure) as failure:
            browser_gate.read_palette_roles(self.source(text))

        self.assertIn("claims 3", str(failure.exception))


class ServedUrlTest(unittest.TestCase):
    """A packaged file name is a file name; a served path is a URL."""

    def test_every_segment_is_quoted_and_the_separators_are_not(self) -> None:
        url = browser_gate.resolve_url("http://127.0.0.1:9/site/play", "a b/c#d.html")

        self.assertEqual(url, "http://127.0.0.1:9/site/play/a%20b/c%23d.html")

    def test_the_package_root_is_the_base_url_itself(self) -> None:
        self.assertEqual(
            browser_gate.resolve_url("http://127.0.0.1:9/site/play", ""),
            "http://127.0.0.1:9/site/play/",
        )


# ---------------------------------------------------------------------------
# The published homepage embed
# ---------------------------------------------------------------------------


class FakeHomepage:
    """A generated homepage, as the discovery step sees it."""

    PUBLISHED = "http://127.0.0.1:9/midcreek-cs-1/play/index.html"

    def __init__(self, **overrides: object) -> None:
        self.report = {
            "count": 1,
            "class": "play-embed",
            "classes": ["play-embed"],
            "src": "play/index.html",
            "resolved": self.PUBLISHED,
            "readable": True,
            "width": 1152.0,
            "height": 648.0,
        }
        self.report.update(overrides)

    scope = browser_gate.TOP_SCOPE
    label = browser_gate.TOP_SCOPE.label

    def evaluate(self, expression: str) -> object:
        if expression == browser_gate.DISCOVER_EMBED_JS:
            if self.report["count"] != 1:
                return {"count": self.report["count"]}
            return self.report
        raise AssertionError(f"the fake homepage cannot answer {expression!r}")


class EmbedDiscoveryTest(unittest.TestCase):
    """The embed is found on the generated page, never quoted from source."""

    def discover(self, page: FakeHomepage) -> dict:
        return browser_gate.discover_embed(page, FakeHomepage.PUBLISHED)

    def test_the_published_embed_is_accepted(self) -> None:
        report = self.discover(FakeHomepage())

        self.assertEqual(report["src"], "play/index.html")

    def test_a_homepage_without_exactly_one_embed_fails(self) -> None:
        for count in (0, 2):
            with self.assertRaises(GateFailure) as failure:
                self.discover(FakeHomepage(count=count))
            self.assertIn(f"{count} iframes", str(failure.exception))

    def test_an_embed_without_the_published_class_fails(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            self.discover(FakeHomepage(classes=["preview"], **{"class": "preview"}))

        self.assertIn("play-embed", str(failure.exception))

    def test_an_embed_pointing_somewhere_else_fails(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            self.discover(
                FakeHomepage(resolved="http://127.0.0.1:9/midcreek-cs-1/old/index.html")
            )

        self.assertIn("not to the published package", str(failure.exception))

    def test_an_embed_the_page_cannot_read_fails(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            self.discover(FakeHomepage(readable=False))

        self.assertIn("no reachable document", str(failure.exception))

    def test_an_embed_with_no_layout_fails(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            self.discover(FakeHomepage(height=0.0))

        self.assertIn("nothing in it can be seen", str(failure.exception))

    def test_the_discovery_never_reads_the_generator_source(self) -> None:
        source = (Path(__file__).resolve().parent / "browser_gate.py").read_text(
            encoding="utf-8"
        )

        self.assertNotIn("sitegen.rs", source)
        self.assertNotIn("<iframe", source)


# ---------------------------------------------------------------------------
# Entry point behaviour
# ---------------------------------------------------------------------------


class EntryPointTest(unittest.TestCase):
    """A driver defect is a failed gate, not a passing one."""

    def arguments(self, extra: list[str] | None = None) -> list[str]:
        design = Path(__file__).resolve().parent.parent / "src" / "design.rs"
        package = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, package, True)
        self.diagnostics = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.diagnostics, True)
        return [
            "--base-url",
            "http://127.0.0.1:9/midcreek-cs-1/play",
            "--cdp-port",
            "9",
            "--package",
            str(package),
            "--design-source",
            str(design),
            "--diagnostics",
            str(self.diagnostics),
            "--hub-url",
            "http://127.0.0.1:9/midcreek-cs-1/",
            *(extra or []),
        ]

    def replace_run_gate(self, replacement: object) -> None:
        original = browser_gate.run_gate
        browser_gate.run_gate = replacement
        self.addCleanup(setattr, browser_gate, "run_gate", original)

    def test_an_unexpected_driver_exception_is_a_failed_gate(self) -> None:
        def explode(arguments: object, sessions: list, deadline: object) -> tuple:
            raise ZeroDivisionError("the driver divided by zero")

        self.replace_run_gate(explode)

        code = browser_gate.main(self.arguments())

        self.assertEqual(code, 1, "an unexpected exception must not pass the gate")
        self.assertFalse(
            (self.diagnostics / "browser-gate.json").exists(),
            "a failed gate never writes the summary a green run publishes",
        )

    def test_every_opened_session_is_closed_whatever_happened(self) -> None:
        closed: list[str] = []

        class Session:
            def close(self) -> None:
                closed.append("closed")

        class OpenedPage:
            session = Session()
            scope = browser_gate.TOP_SCOPE

        def open_then_fail(arguments: object, sessions: list, deadline: object) -> tuple:
            sessions.append(OpenedPage())
            raise GateFailure("the page never arrived")

        self.replace_run_gate(open_then_fail)
        original_diagnostics = browser_gate.write_diagnostics
        browser_gate.write_diagnostics = lambda session, path: None
        self.addCleanup(setattr, browser_gate, "write_diagnostics", original_diagnostics)

        code = browser_gate.main(self.arguments())

        self.assertEqual(code, 1)
        self.assertEqual(closed, ["closed"])

    def test_the_published_report_carries_only_the_fields_its_reader_accepts(self) -> None:
        """The defect this guards: one invented field drops all browser evidence.

        `BrowserGateReport` in the site generator is `deny_unknown_fields`, so
        an extra key here would make the whole browser evidence unreadable and
        silently strip it from the published run. The embed proof therefore
        lives beside that report, never inside it.
        """
        proof = {"class": "play-embed", "src": "play/index.html"}
        summary = {
            "ready_seconds": 5.0,
            "canvas": {"width": 1152, "height": 648, "buffer": [1152, 648]},
            "pixels": {
                "region": [0, 0, 1, 1],
                "sampled_pixels": 1,
                "variance": [1.0, 1.0, 1.0],
                "palette_classes": ["INK"],
                "unmatched_share": 0.0,
            },
            "scroll": {
                "reserve_pixels": 1.0,
                "probe": "scroll-probe",
                "unfocused_deltas": {},
                "focused_deltas": {},
            },
        }

        self.replace_run_gate(lambda arguments, sessions, deadline: (summary, proof))
        code = browser_gate.main(self.arguments())

        self.assertEqual(code, 0)
        published = json.loads(
            (self.diagnostics / "browser-gate.json").read_text(encoding="utf-8")
        )
        self.assertEqual(
            sorted(published), ["canvas", "pixels", "ready_seconds", "scroll"]
        )
        beside = json.loads(
            (self.diagnostics / "embed-gate.json").read_text(encoding="utf-8")
        )
        self.assertEqual(beside["class"], "play-embed")

    def test_the_generated_homepage_is_required(self) -> None:
        arguments = self.arguments()
        del arguments[arguments.index("--hub-url") : arguments.index("--hub-url") + 2]

        with self.assertRaises(SystemExit):
            browser_gate.gate_arguments(arguments)

    def test_a_run_that_finished_past_its_budget_is_not_a_pass(self) -> None:
        """The defect this guards: every phase passed, but far too late.

        A gate that ran out of time proved nothing about a browser anybody will
        wait for, and the report it would write is the one a green run
        publishes.
        """
        summary = {"canvas": {}, "pixels": {}, "ready_seconds": 1.0, "scroll": {}}

        def slow(arguments: object, sessions: list, deadline: object) -> tuple:
            deadline._expires = time.monotonic() - 1.0
            return summary, {"class": "play-embed"}

        self.replace_run_gate(slow)

        code = browser_gate.main(self.arguments())

        self.assertEqual(code, 1)
        self.assertFalse((self.diagnostics / "browser-gate.json").exists())
        self.assertFalse((self.diagnostics / "embed-gate.json").exists())

    def test_expiring_while_the_sessions_close_is_not_a_pass(self) -> None:
        """Cleanup is inside the budget too, not after it."""
        summary = {"canvas": {}, "pixels": {}, "ready_seconds": 1.0, "scroll": {}}
        expired: list[str] = []

        def open_then_expire(arguments: object, sessions: list, deadline: object) -> tuple:
            class Session:
                def close(self) -> None:
                    expired.append("closed")
                    deadline._expires = time.monotonic() - 1.0

            class OpenedPage:
                session = Session()
                scope = browser_gate.TOP_SCOPE

            sessions.append(OpenedPage())
            return summary, {"class": "play-embed"}

        self.replace_run_gate(open_then_expire)

        code = browser_gate.main(self.arguments())

        self.assertEqual(expired, ["closed"])
        self.assertEqual(code, 1)
        self.assertFalse((self.diagnostics / "browser-gate.json").exists())

    def test_expiring_between_the_reports_leaves_neither_behind(self) -> None:
        """A half-written pair is evidence the site would otherwise publish."""
        summary = {"canvas": {}, "pixels": {}, "ready_seconds": 1.0, "scroll": {}}

        class ExpiringDeadline(browser_gate.Deadline):
            """Expires the moment the first report has been written."""

            def __init__(self, budget: float = 600.0) -> None:
                super().__init__(budget)
                self.checks = 0

            def require(self, doing: str) -> float:
                self.checks += 1
                if self.checks > 2:
                    self._expires = time.monotonic() - 1.0
                return super().require(doing)

        original = browser_gate.Deadline
        browser_gate.Deadline = ExpiringDeadline
        self.addCleanup(setattr, browser_gate, "Deadline", original)
        self.replace_run_gate(
            lambda arguments, sessions, deadline: (summary, {"class": "play-embed"})
        )

        code = browser_gate.main(self.arguments())

        self.assertEqual(code, 1)
        self.assertFalse((self.diagnostics / "browser-gate.json").exists())
        self.assertFalse((self.diagnostics / "embed-gate.json").exists())

    def test_expiring_while_serializing_the_combined_result_is_not_a_pass(self) -> None:
        """The final JSON serialization spends the same absolute budget."""
        summary = {"canvas": {}, "pixels": {}, "ready_seconds": 1.0, "scroll": {}}
        active: list[browser_gate.Deadline] = []
        calls = 0
        original_dumps = browser_gate.json.dumps

        def succeed(arguments: object, sessions: list, deadline: object) -> tuple:
            active.append(deadline)
            return summary, {"class": "play-embed"}

        def expire_on_combined_result(*args: object, **kwargs: object) -> str:
            nonlocal calls
            calls += 1
            rendered = original_dumps(*args, **kwargs)
            if calls == 3:
                active[0]._expires = time.monotonic() - 1.0
            return rendered

        self.replace_run_gate(succeed)
        browser_gate.json.dumps = expire_on_combined_result
        self.addCleanup(setattr, browser_gate.json, "dumps", original_dumps)

        with contextlib.redirect_stdout(io.StringIO()):
            code = browser_gate.main(self.arguments())

        self.assertEqual(code, 1)
        self.assertFalse((self.diagnostics / "browser-gate.json").exists())
        self.assertFalse((self.diagnostics / "embed-gate.json").exists())

    def test_expiring_while_flushing_stdout_is_not_a_pass(self) -> None:
        """Returning success means the final observable write finished in budget."""
        summary = {"canvas": {}, "pixels": {}, "ready_seconds": 1.0, "scroll": {}}
        active: list[browser_gate.Deadline] = []

        def succeed(arguments: object, sessions: list, deadline: object) -> tuple:
            active.append(deadline)
            return summary, {"class": "play-embed"}

        class ExpiringStdout(io.StringIO):
            def flush(self) -> None:
                active[0]._expires = time.monotonic() - 1.0
                super().flush()

        self.replace_run_gate(succeed)

        with contextlib.redirect_stdout(ExpiringStdout()):
            code = browser_gate.main(self.arguments())

        self.assertEqual(code, 1)
        self.assertFalse((self.diagnostics / "browser-gate.json").exists())
        self.assertFalse((self.diagnostics / "embed-gate.json").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
