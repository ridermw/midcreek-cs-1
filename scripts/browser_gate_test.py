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
from unittest.mock import patch

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
# Readiness deadline
# ---------------------------------------------------------------------------


class BrowserReadinessDeadlineTest(unittest.TestCase):
    def test_ready_observed_after_the_deadline_is_not_accepted(self) -> None:
        class Session:
            def __init__(self) -> None:
                self.answers = iter(["ready", []])

            def evaluate(self, _expression: str) -> object:
                return next(self.answers)

        with (
            patch.object(
                browser_gate.time,
                "monotonic",
                side_effect=[0.0, 29.9, 30.1],
            ),
            patch.object(browser_gate, "write_diagnostics"),
            self.assertRaises(GateFailure),
        ):
            browser_gate.wait_for_ready(Session(), Path("."))


# ---------------------------------------------------------------------------
# Keyboard ownership
# ---------------------------------------------------------------------------


class FakePage:
    """A scriptable page that answers the gate's own DevTools evaluations.

    The model encodes the browser behaviour the gate was measured against: a
    key only runs its scrolling default action while the element that owns
    focus lets it through, and a key that types a character only scrolls when
    the trusted character event is dispatched alongside the raw key down.
    """

    TYPING_KEYS = {"Space", "KeyQ", "KeyE"}

    def __init__(
        self,
        *,
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
        if expression == browser_gate.NEXT_FRAMES_JS:
            self.frames += 1
            return True
        if expression == browser_gate.SCROLL_OFFSET_JS:
            return self.scroll_y
        if expression == browser_gate.SCROLL_TO_TOP_JS:
            self.scroll_y = 0.0
            return None
        if expression == browser_gate.SCROLL_RESERVE_JS:
            return self.scroll_height - self.inner_height
        if expression == browser_gate.SCROLL_REPORT_JS:
            return {
                "activeElement": self.active,
                "scrollY": self.scroll_y,
                "scrollHeight": self.scroll_height,
                "innerHeight": self.inner_height,
            }
        if expression == browser_gate.FOCUS_SCROLL_PROBE_JS:
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
        if expression == browser_gate.FOCUS_CANVAS_JS:
            self.scroll_y = 0.0
            if self.canvas_takes_focus:
                self.active = "canvas"
            return self.canvas_takes_focus
        if expression == browser_gate.CANVAS_STILL_FOCUSED_JS:
            return self.canvas_keeps_focus and self.active == "canvas"
        raise AssertionError(f"the fake page cannot answer {expression!r}")

    # -- assertions helpers ------------------------------------------------

    def sequence_for(self, owner: str) -> list[tuple[str, str]]:
        return [(kind, code) for active, kind, code in self.dispatched if active == owner]


class KeyboardOwnershipTest(unittest.TestCase):
    """Drives the real gate functions against the scriptable page."""

    def setUp(self) -> None:
        self._budget = browser_gate.KEY_SCROLL_SECONDS
        self._reset = browser_gate.SCROLL_RESET_SECONDS
        browser_gate.KEY_SCROLL_SECONDS = 0.2
        browser_gate.SCROLL_RESET_SECONDS = 1.0

    def tearDown(self) -> None:
        browser_gate.KEY_SCROLL_SECONDS = self._budget
        browser_gate.SCROLL_RESET_SECONDS = self._reset

    def test_every_control_key_is_a_trusted_rawkeydown_then_keyup(self) -> None:
        page = FakePage()
        page.active = page.probe

        browser_gate.press_control_keys(page)

        for code, _, _, text in browser_gate.CONTROL_KEYS:
            kinds = [kind for _, kind, pressed in page.dispatched if pressed == code]
            expected = ["rawKeyDown", "keyUp"] if text is None else ["rawKeyDown", "char", "keyUp"]
            self.assertEqual(kinds, expected, f"{code} was dispatched as {kinds}")

    def test_a_typing_key_carries_the_character_event_chrome_scrolls_on(self) -> None:
        page = FakePage()
        page.active = page.probe

        deltas = browser_gate.press_control_keys(page)

        characters = [code for _, kind, code in page.dispatched if kind == "char"]
        self.assertEqual(characters, ["KeyQ", "KeyE", "Space"])
        self.assertGreater(deltas["Space"], 0.0)

    def test_the_positive_control_records_a_delta_for_every_control_key(self) -> None:
        page = FakePage()

        report = browser_gate.check_control_keys_do_not_scroll(page)

        self.assertEqual(
            sorted(report["unfocused_deltas"]),
            sorted(code for code, _, _, _ in browser_gate.CONTROL_KEYS),
        )
        self.assertEqual(
            sorted(report["focused_deltas"]),
            sorted(code for code, _, _, _ in browser_gate.CONTROL_KEYS),
        )
        self.assertGreater(report["unfocused_deltas"]["ArrowDown"], 0.0)
        self.assertGreater(report["unfocused_deltas"]["Space"], 0.0)
        self.assertEqual(report["unfocused_deltas"]["ArrowLeft"], 0.0)
        self.assertEqual(set(report["focused_deltas"].values()), {0.0})
        self.assertEqual(report["probe"], "scroll-probe")

    def test_the_focused_and_unfocused_phases_dispatch_identical_sequences(self) -> None:
        page = FakePage()

        browser_gate.check_control_keys_do_not_scroll(page)

        probed = page.sequence_for("scroll-probe")
        focused = page.sequence_for("canvas")
        self.assertEqual(probed, focused)
        self.assertEqual(len(probed), 17)

    def test_a_page_without_a_scroll_probe_fails_before_the_positive_control(self) -> None:
        page = FakePage(probe=None)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.check_control_keys_do_not_scroll(page)

        message = str(failure.exception)
        self.assertIn("data-scroll-probe", message)
        self.assertEqual(page.dispatched, [])

    def test_a_probe_that_does_not_own_focus_fails(self) -> None:
        page = FakePage(probe_takes_focus=False)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.check_control_keys_do_not_scroll(page)

        message = str(failure.exception)
        self.assertIn("did not take keyboard focus", message)
        self.assertIn("active element body", message)
        self.assertEqual(page.dispatched, [])

    def test_a_probe_inside_the_canvas_fails(self) -> None:
        page = FakePage(probe_inside_canvas=True)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.check_control_keys_do_not_scroll(page)

        self.assertIn("inside the canvas", str(failure.exception))
        self.assertEqual(page.dispatched, [])

    def test_a_page_without_the_scroll_reserve_fails_before_anything_is_pressed(self) -> None:
        page = FakePage(scroll_height=1000.0, inner_height=900.0)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.check_control_keys_do_not_scroll(page)

        message = str(failure.exception)
        self.assertIn("reserves 100.0 scrollable pixels", message)
        self.assertIn(str(browser_gate.MINIMUM_SCROLL_RESERVE_PIXELS), message)
        self.assertIn("scrollHeight 1000.0", message)
        self.assertIn("innerHeight 900.0", message)
        self.assertEqual(page.dispatched, [])

    def test_a_positive_control_key_that_cannot_scroll_fails_with_per_key_deltas(self) -> None:
        page = FakePage(probe_scrolls=("ArrowDown",))

        with self.assertRaises(GateFailure) as failure:
            browser_gate.check_control_keys_do_not_scroll(page)

        message = str(failure.exception)
        self.assertIn("['Space']", message)
        self.assertIn("neutral scroll probe held focus", message)
        self.assertIn("active element scroll-probe", message)
        self.assertIn("scrollHeight 2400.0", message)
        self.assertIn("innerHeight 900.0", message)
        self.assertIn("per-key scroll deltas", message)
        self.assertIn("ArrowDown +40", message)
        self.assertIn("Space +0", message)

    def test_a_typing_key_without_its_character_event_fails_the_positive_control(self) -> None:
        page = FakePage()
        original = browser_gate.CONTROL_KEYS
        browser_gate.CONTROL_KEYS = tuple(
            (code, key_code, key, None) for code, key_code, key, _ in original
        )
        try:
            with self.assertRaises(GateFailure) as failure:
                browser_gate.check_control_keys_do_not_scroll(page)
        finally:
            browser_gate.CONTROL_KEYS = original

        self.assertIn("['Space']", str(failure.exception))

    def test_a_focused_control_key_that_scrolls_fails_with_per_key_deltas(self) -> None:
        page = FakePage(canvas_scrolls=("ArrowDown",))

        with self.assertRaises(GateFailure) as failure:
            browser_gate.check_control_keys_do_not_scroll(page)

        message = str(failure.exception)
        self.assertIn("['ArrowDown']", message)
        self.assertIn("owned by the game", message)
        self.assertIn("active element canvas", message)
        self.assertIn("per-key scroll deltas", message)
        self.assertIn("ArrowDown +40", message)

    def test_a_canvas_that_refuses_focus_fails(self) -> None:
        page = FakePage(canvas_takes_focus=False)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.check_control_keys_do_not_scroll(page)

        self.assertIn("refused keyboard focus", str(failure.exception))

    def test_a_canvas_that_loses_focus_fails(self) -> None:
        page = FakePage(canvas_keeps_focus=False)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.check_control_keys_do_not_scroll(page)

        self.assertIn("lost focus", str(failure.exception))

    def test_a_page_that_will_not_return_to_the_top_fails_loudly(self) -> None:
        class StuckPage(FakePage):
            def evaluate(self, expression: str) -> object:
                if expression == browser_gate.SCROLL_TO_TOP_JS:
                    return None
                return super().evaluate(expression)

        page = StuckPage()
        page.scroll_y = 120.0

        with self.assertRaises(GateFailure) as failure:
            browser_gate.reset_scroll(page)

        self.assertIn("would not come to rest", str(failure.exception))
        self.assertIn("scrollY 120.0", str(failure.exception))

    def test_an_animated_scroll_is_measured_at_its_resting_place(self) -> None:
        """The defect this replaced: a busy renderer read as a settled page.

        The scroll advances one step per rendered frame, so anything that
        sampled a wall clock could read a partial offset, or the starting
        offset, as the final answer.
        """

        class AnimatedPage(FakePage):
            def __init__(self) -> None:
                super().__init__()
                self.target = 0.0
                self.stall_frames = 4

            def _scroll_for(self, code: str, typed: bool) -> None:
                allowed = self.probe_scrolls if self.active == self.probe else self.canvas_scrolls
                if code not in allowed or (code in self.TYPING_KEYS and not typed):
                    return
                self.target = self.scroll_y + self.scroll_step

            def evaluate(self, expression: str) -> object:
                if expression == browser_gate.NEXT_FRAMES_JS:
                    if self.stall_frames > 0:
                        self.stall_frames -= 1
                    elif self.scroll_y < self.target:
                        self.scroll_y = min(self.scroll_y + 8.0, self.target)
                    return True
                if expression == browser_gate.SCROLL_TO_TOP_JS:
                    self.target = 0.0
                return super().evaluate(expression)

        page = AnimatedPage()
        page.active = page.probe

        delta = browser_gate.press_control_key(page, "ArrowDown", 40, "ArrowDown", None)

        self.assertEqual(delta, 40.0)
        self.assertEqual(page.scroll_y, 40.0)


class KeyboardOwnershipSourceTest(unittest.TestCase):
    """Structural guards that keep the two phases from drifting apart."""

    SOURCE = (Path(__file__).resolve().parent / "browser_gate.py").read_text(encoding="utf-8")

    def test_there_is_exactly_one_key_dispatch_call_site(self) -> None:
        self.assertEqual(self.SOURCE.count('session.call("Input.dispatchKeyEvent"'), 3)
        self.assertEqual(self.SOURCE.count("def press_control_key("), 1)
        self.assertEqual(self.SOURCE.count("def press_control_keys("), 1)

    def test_both_phases_go_through_the_same_helper(self) -> None:
        body = self.SOURCE.split("def check_control_keys_do_not_scroll(")[1]
        self.assertEqual(body.count("press_control_keys(session)"), 2)
        self.assertNotIn("Input.dispatchKeyEvent", body)

    def test_the_positive_control_names_the_keys_the_browser_guarantees(self) -> None:
        self.assertEqual(browser_gate.POSITIVE_CONTROL_KEYS, ("ArrowDown", "Space"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
