#!/usr/bin/env python3
"""Unit tests for the repository-owned browser gate WebSocket client.

Every test drives the real client against a real loopback socket that speaks
the parts of RFC 6455 the Chrome DevTools Protocol actually uses, so frame
reassembly is proved end to end rather than asserted about in the abstract.
"""

from __future__ import annotations

import socket
import shutil
import struct
import sys
import tempfile
import threading
import time
import unittest
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
        if expression == browser_gate.PROBE_STILL_FOCUSED_JS:
            return self.probe is not None and self.active == self.probe
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

    def test_a_probe_that_loses_focus_during_the_positive_control_fails(self) -> None:
        """The positive control only measures what the probe actually received.

        A page that moved focus part way through the sequence scrolled for some
        other reason, so the focused no-scroll phase that follows would be
        compared against a measurement of something else.
        """

        class DriftingPage(FakePage):
            def evaluate(self, expression: str) -> object:
                if expression == browser_gate.PROBE_STILL_FOCUSED_JS:
                    return False
                return super().evaluate(expression)

        with self.assertRaises(GateFailure) as failure:
            browser_gate.check_control_keys_do_not_scroll(DriftingPage())

        self.assertIn("did not still own keyboard focus", str(failure.exception))

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

    def test_every_key_dispatch_goes_through_the_single_press_helper(self) -> None:
        """One keystroke helper, and the three events one keystroke really is.

        The three dispatch calls are the raw key down, the character event, and
        the key up of a single press, all inside `press_control_key`. A fourth
        would be a second way to send a key, which is how the focused and
        unfocused phases start proving different things.
        """
        self.assertEqual(self.SOURCE.count('session.call("Input.dispatchKeyEvent"'), 3)
        self.assertEqual(self.SOURCE.count("def press_control_key("), 1)
        self.assertEqual(self.SOURCE.count("def press_control_keys("), 1)

    def test_both_phases_go_through_the_same_helper(self) -> None:
        body = self.SOURCE.split("def check_control_keys_do_not_scroll(")[1]
        self.assertEqual(body.count("press_control_keys(session)"), 2)
        self.assertNotIn("Input.dispatchKeyEvent", body)

    def test_the_positive_control_names_the_keys_the_browser_guarantees(self) -> None:
        self.assertEqual(browser_gate.POSITIVE_CONTROL_KEYS, ("ArrowDown", "Space"))


# ---------------------------------------------------------------------------
# Session budget
# ---------------------------------------------------------------------------


class SessionBudgetTest(unittest.TestCase):
    """Every DevTools call is bounded by what is left of the whole session."""

    def session(self, payload: bytes, budget: float) -> browser_gate.DevTools:
        server = FrameServer(payload)
        self.addCleanup(server.close)
        self.server = server
        session = browser_gate.DevTools(server.url, budget=budget)
        self.addCleanup(session.close)
        return session

    def test_a_call_past_the_budget_never_reaches_the_browser(self) -> None:
        session = self.session(b"", budget=0.0)

        with self.assertRaises(GateFailure) as failure:
            session.call("Page.enable")

        self.assertIn("budget", str(failure.exception))
        self.assertEqual(
            len(self.server.received), 0, "an expired session sends nothing"
        )

    def test_a_call_is_bounded_by_the_remaining_budget_not_its_own_timeout(self) -> None:
        """The defect this guards: a browser that answers, but never the caller.

        Every answer here belongs to somebody else and then the browser goes
        quiet, so the call's own thirty second timeout — and the blocking read
        inside it — would keep the gate waiting far past the session budget.
        """
        chatter = frame(TEXT, b'{"id":9999}') * 2000
        session = self.session(chatter, budget=0.2)

        started = time.monotonic()
        with self.assertRaises(GateFailure):
            session.call("Page.enable", timeout=browser_gate.BROWSER_TIMEOUT_SECONDS)
        elapsed = time.monotonic() - started

        self.assertLess(
            elapsed,
            5.0,
            "the call waited on its own timeout instead of the session budget",
        )


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
# The published hub embed
# ---------------------------------------------------------------------------


HUB_SOURCE = (
    'format!(r#"<div class="play-frame">\n'
    '  <iframe class="play-embed" src="play/index.html" title="Playable build" '
    'loading="lazy"></iframe>\n'
    "</div>"#)\n"
)


class HubEmbedContractTest(unittest.TestCase):
    """The embed under test is read from the generator that publishes it."""

    def source(self, text: str) -> Path:
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, directory, True)
        path = directory / "sitegen.rs"
        path.write_text(text, encoding="utf-8")
        return path

    def test_the_embed_is_read_out_of_the_generator(self) -> None:
        embed = browser_gate.read_play_embed(self.source(HUB_SOURCE))

        self.assertEqual(embed.class_name, "play-embed")
        self.assertEqual(embed.src, "play/index.html")
        self.assertIn('loading="lazy"', embed.tag)

    def test_the_real_generator_still_renders_exactly_one_embed(self) -> None:
        sitegen = Path(__file__).resolve().parent.parent / "src" / "sitegen.rs"

        embed = browser_gate.read_play_embed(sitegen)

        self.assertEqual(embed.class_name, "play-embed")
        self.assertEqual(embed.src, "play/index.html")

    def test_a_generator_with_no_embed_is_refused(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            browser_gate.read_play_embed(self.source("fn render_play() {}"))

        self.assertIn("0 playable iframes", str(failure.exception))

    def test_a_generator_with_two_embeds_is_refused(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            browser_gate.read_play_embed(self.source(HUB_SOURCE + HUB_SOURCE))

        self.assertIn("2 playable iframes", str(failure.exception))

    def test_the_harness_hosts_the_generators_own_tag(self) -> None:
        embed = browser_gate.read_play_embed(self.source(HUB_SOURCE))
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, directory, True)
        page = directory / "index.html"

        browser_gate.write_hub_page(page, embed)

        written = page.read_text(encoding="utf-8")
        self.assertIn(embed.tag, written)
        self.assertIn(".play-embed {", written)

    def test_the_harness_never_overwrites_a_published_page(self) -> None:
        embed = browser_gate.read_play_embed(self.source(HUB_SOURCE))
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, directory, True)
        page = directory / "index.html"
        page.write_text("the real hub", encoding="utf-8")

        with self.assertRaises(GateFailure) as failure:
            browser_gate.write_hub_page(page, embed)

        self.assertIn("refusing to overwrite", str(failure.exception))
        self.assertEqual(page.read_text(encoding="utf-8"), "the real hub")


class FakeHub:
    """A hub document with one scriptable playable iframe in it."""

    RESOLVED = "http://127.0.0.1:9/site/play/index.html"

    def __init__(
        self,
        *,
        count: int = 1,
        same_origin: bool = True,
        resolved: str | None = None,
        states: tuple[str, ...] = ("ready",),
        errors: str = "",
        canvas: dict | None = None,
    ) -> None:
        self.count = count
        self.same_origin = same_origin
        self.resolved = self.RESOLVED if resolved is None else resolved
        self.states = list(states)
        self.errors = errors
        self.canvas = (
            {
                "width": 1152.0,
                "height": 648.0,
                "bufferWidth": 1152,
                "bufferHeight": 648,
                "visible": True,
            }
            if canvas is None
            else canvas
        )
        self.reads = 0

    def call(self, method: str, params: dict | None = None) -> dict:
        if method == "Page.captureScreenshot":
            raise GateFailure("the fake hub takes no screenshots")
        raise AssertionError(f"the fake hub should not call {method}")

    def evaluate(self, expression: str) -> object:
        if expression == "document.documentElement.outerHTML":
            return "<html></html>"
        self.reads += 1
        if self.count != 1:
            return {"count": self.count}
        if not self.same_origin:
            return {"count": 1, "sameOrigin": False, "resolved": self.resolved}
        state = self.states[min(self.reads, len(self.states)) - 1]
        return {
            "count": 1,
            "sameOrigin": True,
            "resolved": self.resolved,
            "state": state,
            "errors": self.errors,
            "canvas": self.canvas,
        }


class HubEmbedGateTest(unittest.TestCase):
    """Drives the real hub phase against the scriptable hub document."""

    EMBED = browser_gate.PlayEmbed(
        class_name="play-embed",
        src="play/index.html",
        tag='<iframe class="play-embed" src="play/index.html"></iframe>',
    )

    def setUp(self) -> None:
        self._ready = browser_gate.READY_TIMEOUT_SECONDS
        browser_gate.READY_TIMEOUT_SECONDS = 0.3
        self.diagnostics = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.diagnostics, True)

    def tearDown(self) -> None:
        browser_gate.READY_TIMEOUT_SECONDS = self._ready

    def check(self, hub: FakeHub) -> dict:
        return browser_gate.check_hub_embed(
            hub, self.EMBED, FakeHub.RESOLVED, self.diagnostics
        )

    def test_a_ready_embed_reports_the_element_it_proved(self) -> None:
        summary = self.check(FakeHub())

        self.assertEqual(summary["class"], "play-embed")
        self.assertEqual(summary["src"], "play/index.html")
        self.assertEqual(summary["resolved"], FakeHub.RESOLVED)
        self.assertEqual(summary["canvas"]["buffer"], [1152, 648])

    def test_an_embed_that_starts_loading_is_waited_for(self) -> None:
        summary = self.check(FakeHub(states=("loading", "loading", "ready")))

        self.assertEqual(summary["resolved"], FakeHub.RESOLVED)

    def test_a_hub_without_exactly_one_embed_fails(self) -> None:
        for count in (0, 2):
            with self.assertRaises(GateFailure) as failure:
                self.check(FakeHub(count=count))
            self.assertIn(f"{count} iframe.play-embed", str(failure.exception))

    def test_an_embed_the_hub_cannot_read_fails(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            self.check(FakeHub(same_origin=False))

        self.assertIn("not readable", str(failure.exception))

    def test_an_embed_pointing_somewhere_else_fails(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            self.check(FakeHub(resolved="http://127.0.0.1:9/site/old/index.html"))

        self.assertIn("not to the published package", str(failure.exception))

    def test_an_embed_that_never_reaches_ready_fails_with_what_it_captured(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            self.check(FakeHub(states=("loading",), errors="rack.glb failed to load"))

        message = str(failure.exception)
        self.assertIn("data-game-state='loading'", message)
        self.assertIn("rack.glb failed to load", message)

    def test_an_embed_that_reported_error_fails_at_once(self) -> None:
        hub = FakeHub(states=("error",))

        started = time.monotonic()
        with self.assertRaises(GateFailure):
            self.check(hub)

        self.assertLess(time.monotonic() - started, 0.3)

    def test_an_embed_that_captured_an_error_while_ready_still_fails(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            self.check(FakeHub(errors="unhandled rejection"))

        self.assertIn("captured errors", str(failure.exception))

    def test_an_embed_without_a_canvas_fails(self) -> None:
        with self.assertRaises(GateFailure) as failure:
            self.check(FakeHub(canvas={}))

        self.assertIn("no #game-canvas", str(failure.exception))

    def test_an_embedded_canvas_that_is_not_sixteen_by_nine_fails(self) -> None:
        squashed = {
            "width": 900.0,
            "height": 648.0,
            "bufferWidth": 900,
            "bufferHeight": 648,
            "visible": True,
        }

        with self.assertRaises(GateFailure) as failure:
            self.check(FakeHub(canvas=squashed))

        self.assertIn("not 16:9", str(failure.exception))


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
            "http://127.0.0.1:9/site/play",
            "--cdp-port",
            "9",
            "--package",
            str(package),
            "--design-source",
            str(design),
            "--diagnostics",
            str(self.diagnostics),
            *(extra or []),
        ]

    def test_an_unexpected_driver_exception_is_a_failed_gate(self) -> None:
        def explode(arguments: object, sessions: list) -> dict:
            raise ZeroDivisionError("the driver divided by zero")

        original = browser_gate.run_gate
        browser_gate.run_gate = explode
        self.addCleanup(setattr, browser_gate, "run_gate", original)

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

        def open_then_fail(arguments: object, sessions: list) -> dict:
            sessions.append(Session())
            raise GateFailure("the page never arrived")

        original = browser_gate.run_gate
        browser_gate.run_gate = open_then_fail
        self.addCleanup(setattr, browser_gate, "run_gate", original)
        original_diagnostics = browser_gate.write_diagnostics
        browser_gate.write_diagnostics = lambda session, path: None
        self.addCleanup(setattr, browser_gate, "write_diagnostics", original_diagnostics)

        code = browser_gate.main(self.arguments())

        self.assertEqual(code, 1)
        self.assertEqual(closed, ["closed"])

    def test_the_hub_arguments_are_used_together_or_not_at_all(self) -> None:
        with self.assertRaises(SystemExit):
            browser_gate.gate_arguments(
                self.arguments(["--hub-url", "http://127.0.0.1:9/site/"])
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
