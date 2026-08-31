#!/usr/bin/env python3
"""Repository-owned headless browser gate for the packaged WASM game.

Speaks the Chrome DevTools Protocol over a hand-rolled RFC 6455 WebSocket
client built from the Python standard library only. Nothing here downloads a
browser automation package, and nothing here needs npm.

The gate proves, independently:

* every packaged URL is served with HTTP 200;
* the game reports ``data-game-state="ready"`` within the readiness budget;
* ``#browser-errors`` captured no error and no unhandled rejection;
* the canvas is visible and 16:9 within one pixel;
* trusted Arrow/Q/E/Space input while the canvas is focused does not scroll a
  page that a neutral focus probe has just proved is genuinely scrollable;
* the canvas region of a real screenshot is nonblank and carries at least
  three approved palette classes with real variance.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import socket
import struct
import sys
import time
import urllib.error
import urllib.request
import zlib
from dataclasses import dataclass
from pathlib import Path

READY_TIMEOUT_SECONDS = 30
BROWSER_TIMEOUT_SECONDS = 30
# A DevTools answer is a JSON document; a screenshot is the largest one the
# gate ever asks for. Anything past this is a confused browser, not a message.
MAX_MESSAGE_BYTES = 64 * 1024 * 1024
MAX_CONTROL_PAYLOAD_BYTES = 125
ASPECT_TOLERANCE_PIXELS = 1.0
MINIMUM_PALETTE_CLASSES = 3
MINIMUM_CLASS_SHARE = 0.01
MINIMUM_CHANNEL_VARIANCE = 25.0
PALETTE_MATCH_DISTANCE = 48.0
# The play page reserves a whole viewport plus this many absolute pixels below
# the notes, so the positive control below can never be starved of somewhere to
# scroll by a tall window or by whichever fonts the runner happens to have.
MINIMUM_SCROLL_RESERVE_PIXELS = 240.0
# Keyboard scrolling can be animated, so a delta is never read off a wall clock:
# the gate waits for rendered frames, which is the only clock a scroll animation
# actually advances on, and gives up after this long.
KEY_SCROLL_SECONDS = 3.0
SCROLL_RESET_SECONDS = 10.0
# ``text`` is the character a key would type. Chrome only runs the default
# action for Space on the character event, so a key that types something is
# given the trusted ``char`` event a real keystroke would produce. Blink drops
# that character event when the preceding key event was default-prevented,
# which is exactly the behaviour the focused assertion relies on.
CONTROL_KEYS = (
    ("ArrowUp", 38, "ArrowUp", None),
    ("ArrowDown", 40, "ArrowDown", None),
    ("ArrowLeft", 37, "ArrowLeft", None),
    ("ArrowRight", 39, "ArrowRight", None),
    ("KeyQ", 81, "q", "q"),
    ("KeyE", 69, "e", "e"),
    ("Space", 32, " ", " "),
)
# The keys whose scrolling is a browser guarantee rather than a page detail.
# The positive control fails unless both of them move a page nobody is playing.
POSITIVE_CONTROL_KEYS = ("ArrowDown", "Space")


class GateFailure(Exception):
    """A browser gate assertion that did not hold."""


# ---------------------------------------------------------------------------
# WebSocket client
# ---------------------------------------------------------------------------


class WebSocket:
    """An RFC 6455 client with the framing the DevTools Protocol really uses.

    Messages are reassembled across continuation frames, control frames may be
    interleaved at any point, and both a single announced frame and a
    reassembled message are bounded, so a confused browser fails loudly instead
    of making the gate hang or exhaust memory.
    """

    CONTINUATION = 0x0
    TEXT = 0x1
    BINARY = 0x2
    CLOSE = 0x8
    PING = 0x9
    PONG = 0xA
    CONTROL_OPCODES = (CLOSE, PING, PONG)

    def __init__(
        self,
        url: str,
        timeout: float = BROWSER_TIMEOUT_SECONDS,
        max_message_bytes: int = MAX_MESSAGE_BYTES,
    ) -> None:
        match = re.match(r"ws://([^/:]+):(\d+)(/.*)", url)
        if not match:
            raise GateFailure(f"unsupported DevTools endpoint: {url}")
        self._max_message_bytes = max_message_bytes
        host, port, resource = match.group(1), int(match.group(2)), match.group(3)
        self._socket = socket.create_connection((host, port), timeout=timeout)
        self._socket.settimeout(timeout)
        self._buffer = b""
        key = base64.b64encode(os.urandom(16)).decode("ascii")
        request = (
            f"GET {resource} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        self._socket.sendall(request.encode("ascii"))
        while b"\r\n\r\n" not in self._buffer:
            self._read_more()
        head, self._buffer = self._buffer.split(b"\r\n\r\n", 1)
        if b"101" not in head.split(b"\r\n", 1)[0]:
            raise GateFailure(f"DevTools refused the WebSocket upgrade: {head!r}")

    def _read_more(self) -> None:
        try:
            chunk = self._socket.recv(65536)
        except TimeoutError as failure:
            raise GateFailure(
                "the DevTools WebSocket stopped answering with "
                f"{len(self._buffer)} bytes buffered: {failure}"
            ) from failure
        except OSError as failure:
            raise GateFailure(
                f"the DevTools WebSocket could not be read: {failure}"
            ) from failure
        if not chunk:
            raise GateFailure("the DevTools WebSocket closed unexpectedly")
        self._buffer += chunk

    def send(self, payload: str) -> None:
        data = payload.encode("utf-8")
        header = bytearray([0x80 | self.TEXT])
        mask = os.urandom(4)
        length = len(data)
        if length < 126:
            header.append(0x80 | length)
        elif length < (1 << 16):
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", length)
        header += mask
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(data))
        self._socket.sendall(bytes(header) + masked)

    def receive(self) -> str:
        """Returns the next complete message, reassembled across fragments."""
        message = bytearray()
        message_opcode: int | None = None
        while True:
            final, opcode, payload = self._read_frame()

            if opcode in self.CONTROL_OPCODES:
                self._handle_control(final, opcode, payload, len(message))
                continue

            if opcode == self.CONTINUATION:
                if message_opcode is None:
                    raise GateFailure(
                        "the browser sent a continuation frame with no message to "
                        f"continue (payload {payload[:64]!r})"
                    )
            elif opcode in (self.TEXT, self.BINARY):
                if message_opcode is not None:
                    raise GateFailure(
                        f"the browser started a 0x{opcode:x} message inside an "
                        f"unfinished 0x{message_opcode:x} message after "
                        f"{len(message)} bytes"
                    )
                message_opcode = opcode
            else:
                raise GateFailure(f"unsupported WebSocket opcode 0x{opcode:x}")

            message += payload
            if len(message) > self._max_message_bytes:
                raise GateFailure(
                    f"a DevTools message grew past the {self._max_message_bytes} "
                    f"byte limit at {len(message)} bytes"
                )
            if final:
                try:
                    return bytes(message).decode("utf-8")
                except UnicodeDecodeError as failure:
                    raise GateFailure(
                        f"a DevTools message was not valid UTF-8: {failure}"
                    ) from failure

    def _handle_control(
        self, final: bool, opcode: int, payload: bytes, pending: int
    ) -> None:
        if not final:
            raise GateFailure(
                f"the browser fragmented a control frame (opcode 0x{opcode:x})"
            )
        if len(payload) > MAX_CONTROL_PAYLOAD_BYTES:
            raise GateFailure(
                f"the browser sent a {len(payload)} byte control frame "
                f"(opcode 0x{opcode:x}), over the {MAX_CONTROL_PAYLOAD_BYTES} "
                "byte limit"
            )
        if opcode == self.CLOSE:
            code = struct.unpack(">H", payload[:2])[0] if len(payload) >= 2 else None
            reason = payload[2:].decode("utf-8", "replace")
            raise GateFailure(
                "the browser closed the DevTools connection "
                f"(code {code}, reason {reason!r}) after {pending} pending bytes"
            )
        if opcode == self.PING:
            self._send_pong(payload)

    def _send_pong(self, payload: bytes) -> None:
        mask = os.urandom(4)
        header = bytearray([0x80 | self.PONG, 0x80 | len(payload)]) + mask
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self._socket.sendall(bytes(header) + masked)

    def _need(self, count: int) -> None:
        while len(self._buffer) < count:
            self._read_more()

    def _read_frame(self) -> tuple[bool, int, bytes]:
        self._need(2)
        first, second = self._buffer[0], self._buffer[1]
        if first & 0x70:
            raise GateFailure(
                f"the browser set a reserved WebSocket bit (0x{first:02x}); no "
                "extension was negotiated"
            )
        final = bool(first & 0x80)
        opcode = first & 0x0F
        masked = bool(second & 0x80)
        length = second & 0x7F
        offset = 2
        if length == 126:
            self._need(4)
            length = struct.unpack(">H", self._buffer[2:4])[0]
            offset = 4
        elif length == 127:
            self._need(10)
            length = struct.unpack(">Q", self._buffer[2:10])[0]
            offset = 10
        if masked:
            raise GateFailure("the browser masked a server frame, which RFC 6455 forbids")
        if length > self._max_message_bytes:
            raise GateFailure(
                f"the browser announced a {length} byte frame, over the "
                f"{self._max_message_bytes} byte limit"
            )
        self._need(offset + length)
        payload = self._buffer[offset : offset + length]
        self._buffer = self._buffer[offset + length :]
        return final, opcode, bytes(payload)

    def close(self) -> None:
        try:
            self._socket.close()
        except OSError:
            pass


class DevTools:
    """A single-target Chrome DevTools Protocol session."""

    def __init__(self, websocket_url: str) -> None:
        self._socket = WebSocket(websocket_url)
        self._next_id = 0

    def call(self, method: str, params: dict | None = None, timeout: float = BROWSER_TIMEOUT_SECONDS) -> dict:
        self._next_id += 1
        message_id = self._next_id
        self._socket.send(json.dumps({"id": message_id, "method": method, "params": params or {}}))
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            message = json.loads(self._socket.receive())
            if message.get("id") != message_id:
                continue
            if "error" in message:
                raise GateFailure(f"{method} failed: {message['error']}")
            return message.get("result", {})
        raise GateFailure(f"{method} did not answer within {timeout:.0f}s")

    def evaluate(self, expression: str) -> object:
        result = self.call(
            "Runtime.evaluate",
            {"expression": expression, "returnByValue": True, "awaitPromise": True},
        )
        if result.get("exceptionDetails"):
            raise GateFailure(f"page evaluation threw: {result['exceptionDetails']}")
        return result["result"].get("value")

    def close(self) -> None:
        self._socket.close()


# ---------------------------------------------------------------------------
# Minimal PNG decoding
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Image:
    width: int
    height: int
    pixels: bytes  # RGB, three bytes per pixel

    def pixel(self, x: int, y: int) -> tuple[int, int, int]:
        offset = (y * self.width + x) * 3
        return (
            self.pixels[offset],
            self.pixels[offset + 1],
            self.pixels[offset + 2],
        )


def decode_png(data: bytes) -> Image:
    """Decodes the 8-bit non-interlaced RGB/RGBA PNG Chrome returns."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise GateFailure("the screenshot is not a PNG")
    offset = 8
    header: tuple[int, int, int, int] | None = None
    compressed = bytearray()
    while offset < len(data):
        (length,) = struct.unpack(">I", data[offset : offset + 4])
        kind = data[offset + 4 : offset + 8]
        payload = data[offset + 8 : offset + 8 + length]
        offset += 12 + length
        if kind == b"IHDR":
            width, height, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", payload)
            if depth != 8 or interlace != 0 or colour not in (2, 6):
                raise GateFailure(f"unsupported PNG: depth {depth} colour {colour} interlace {interlace}")
            header = (width, height, colour, 3 if colour == 2 else 4)
        elif kind == b"IDAT":
            compressed += payload
        elif kind == b"IEND":
            break
    if header is None:
        raise GateFailure("the screenshot has no PNG header")

    width, height, _, channels = header
    raw = zlib.decompress(bytes(compressed))
    stride = width * channels
    previous = bytearray(stride)
    pixels = bytearray(width * height * 3)
    position = 0
    for row in range(height):
        filter_type = raw[position]
        position += 1
        line = bytearray(raw[position : position + stride])
        position += stride
        _unfilter(filter_type, line, previous, channels)
        for column in range(width):
            source = column * channels
            destination = (row * width + column) * 3
            pixels[destination : destination + 3] = line[source : source + 3]
        previous = line
    return Image(width=width, height=height, pixels=bytes(pixels))


def _unfilter(filter_type: int, line: bytearray, previous: bytearray, channels: int) -> None:
    if filter_type == 0:
        return
    for index in range(len(line)):
        left = line[index - channels] if index >= channels else 0
        up = previous[index]
        upper_left = previous[index - channels] if index >= channels else 0
        if filter_type == 1:
            line[index] = (line[index] + left) & 0xFF
        elif filter_type == 2:
            line[index] = (line[index] + up) & 0xFF
        elif filter_type == 3:
            line[index] = (line[index] + ((left + up) >> 1)) & 0xFF
        elif filter_type == 4:
            line[index] = (line[index] + _paeth(left, up, upper_left)) & 0xFF
        else:
            raise GateFailure(f"unsupported PNG filter {filter_type}")


def _paeth(left: int, up: int, upper_left: int) -> int:
    estimate = left + up - upper_left
    distance_left = abs(estimate - left)
    distance_up = abs(estimate - up)
    distance_upper_left = abs(estimate - upper_left)
    if distance_left <= distance_up and distance_left <= distance_upper_left:
        return left
    if distance_up <= distance_upper_left:
        return up
    return upper_left


# ---------------------------------------------------------------------------
# Approved palette
# ---------------------------------------------------------------------------


def read_palette(design_source: Path) -> dict[str, tuple[int, int, int]]:
    """Reads the approved cel-shift palette straight from the Rust source."""
    text = design_source.read_text(encoding="utf-8")
    pattern = re.compile(
        r"pub const (?P<name>[A-Z0-9_]+): Srgba = srgba_u8!\("
        r"0x(?P<r>[0-9A-Fa-f]{2}), 0x(?P<g>[0-9A-Fa-f]{2}), 0x(?P<b>[0-9A-Fa-f]{2})\)"
    )
    palette = {
        match.group("name"): (
            int(match.group("r"), 16),
            int(match.group("g"), 16),
            int(match.group("b"), 16),
        )
        for match in pattern.finditer(text)
    }
    if len(palette) != 17:
        raise GateFailure(
            f"expected the 17 approved palette roles in {design_source}, found {len(palette)}"
        )
    return palette


# ---------------------------------------------------------------------------
# HTTP checks
# ---------------------------------------------------------------------------


def require_http_200(base_url: str, relative: str) -> None:
    url = f"{base_url.rstrip('/')}/{relative.lstrip('/')}"
    request = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(request, timeout=BROWSER_TIMEOUT_SECONDS) as response:
            if response.status != 200:
                raise GateFailure(f"{relative} returned HTTP {response.status}")
            response.read(1024)
    except urllib.error.URLError as failure:
        raise GateFailure(f"{relative} could not be fetched: {failure}") from failure


def packaged_urls(package: Path) -> list[str]:
    urls = []
    for path in sorted(package.rglob("*")):
        if path.is_file():
            urls.append(path.relative_to(package).as_posix())
    return urls


# ---------------------------------------------------------------------------
# Browser session
# ---------------------------------------------------------------------------


def open_page(cdp_port: int, url: str) -> DevTools:
    deadline = time.monotonic() + BROWSER_TIMEOUT_SECONDS
    version_url = f"http://127.0.0.1:{cdp_port}/json/version"
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(version_url, timeout=2) as response:
                json.loads(response.read())
                break
        except (urllib.error.URLError, ConnectionError, TimeoutError, socket.timeout):
            time.sleep(0.25)
    else:
        raise GateFailure(f"the browser never opened a DevTools endpoint on port {cdp_port}")

    new_target = urllib.request.Request(
        f"http://127.0.0.1:{cdp_port}/json/new?{url}", method="PUT"
    )
    with urllib.request.urlopen(new_target, timeout=BROWSER_TIMEOUT_SECONDS) as response:
        target = json.loads(response.read())
    return DevTools(target["webSocketDebuggerUrl"])


def wait_for_ready(session: DevTools, diagnostics: Path) -> float:
    deadline = time.monotonic() + READY_TIMEOUT_SECONDS
    state = "missing"
    while time.monotonic() < deadline:
        state = session.evaluate("document.body ? document.body.dataset.gameState : 'missing'")
        observed_at = time.monotonic()
        if state == "ready" and observed_at <= deadline:
            return READY_TIMEOUT_SECONDS - (deadline - observed_at)
        if state in ("ready", "error"):
            break
        time.sleep(0.25)
    errors = session.evaluate(
        "(document.getElementById('browser-errors') || {}).textContent || ''"
    )
    write_diagnostics(session, diagnostics)
    raise GateFailure(
        f"the game reported data-game-state={state!r} instead of 'ready' "
        f"within {READY_TIMEOUT_SECONDS}s; captured: {errors!r}"
    )


def write_diagnostics(session: DevTools, diagnostics: Path) -> None:
    diagnostics.mkdir(parents=True, exist_ok=True)
    try:
        html = session.evaluate("document.documentElement.outerHTML")
        (diagnostics / "page.html").write_text(str(html), encoding="utf-8")
    except GateFailure:
        pass
    try:
        shot = session.call("Page.captureScreenshot", {"format": "png"})
        (diagnostics / "page.png").write_bytes(base64.b64decode(shot["data"]))
    except GateFailure:
        pass


def canvas_geometry(session: DevTools) -> dict:
    geometry = session.evaluate(
        """
        (() => {
          const canvas = document.getElementById("game-canvas");
          if (!canvas) return null;
          const rect = canvas.getBoundingClientRect();
          const style = window.getComputedStyle(canvas);
          return {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            bufferWidth: canvas.width,
            bufferHeight: canvas.height,
            visible: style.visibility !== "hidden" && style.display !== "none",
            devicePixelRatio: window.devicePixelRatio,
          };
        })()
        """
    )
    if not geometry:
        raise GateFailure("the play page has no #game-canvas element")
    return geometry


def check_canvas(geometry: dict) -> None:
    if not geometry["visible"]:
        raise GateFailure("the canvas is not visible")
    width = float(geometry["width"])
    height = float(geometry["height"])
    if width <= 0 or height <= 0:
        raise GateFailure(f"the canvas has no size: {width}x{height}")
    expected = height * 16.0 / 9.0
    if abs(width - expected) > ASPECT_TOLERANCE_PIXELS:
        raise GateFailure(
            f"the canvas is {width}x{height}, which is not 16:9 within "
            f"{ASPECT_TOLERANCE_PIXELS} pixel (expected width {expected:.2f})"
        )
    if int(geometry["bufferWidth"]) <= 0 or int(geometry["bufferHeight"]) <= 0:
        raise GateFailure("the canvas has an empty drawing buffer")


def check_no_browser_errors(session: DevTools) -> None:
    captured = session.evaluate(
        "(document.getElementById('browser-errors') || {}).textContent || ''"
    )
    if str(captured).strip():
        raise GateFailure(f"the browser captured errors: {captured!r}")


SCROLL_OFFSET_JS = "window.scrollY"

SCROLL_TO_TOP_JS = "window.scrollTo(0, 0)"

SCROLL_RESERVE_JS = "document.documentElement.scrollHeight - window.innerHeight"

# A scroll animation advances once per rendered frame, so frames are the only
# clock it can be measured against. A wall clock reads a janky renderer as a
# settled page and turns a real scroll into a false zero.
NEXT_FRAMES_JS = """
new Promise((resolve) => {
  let remaining = 2;
  const step = () => (remaining-- > 0 ? requestAnimationFrame(step) : resolve(true));
  step();
})
"""

SCROLL_REPORT_JS = """
(() => {
  const node = document.activeElement;
  const name = node
    ? node.tagName.toLowerCase() + (node.id ? "#" + node.id : "")
    : "none";
  return {
    activeElement: name,
    scrollY: window.scrollY,
    scrollHeight: document.documentElement.scrollHeight,
    innerHeight: window.innerHeight,
  };
})()
"""

# The probe is a neutral focus target that lives outside the canvas. A button
# or a link would consume Space itself, so the page ships a plain focusable
# region and the gate refuses to continue unless that region really owns focus.
FOCUS_SCROLL_PROBE_JS = """
(() => {
  const probe = document.querySelector("[data-scroll-probe]");
  if (!probe) return { probe: null };
  const canvas = document.getElementById("game-canvas");
  if (canvas) {
    if (canvas.contains(probe) || probe === canvas) {
      return { probe: probe.id || "unnamed", insideCanvas: true };
    }
    canvas.blur();
  }
  probe.focus({ preventScroll: true });
  window.scrollTo(0, 0);
  return {
    probe: probe.id || "unnamed",
    insideCanvas: false,
    owned: document.activeElement === probe,
  };
})()
"""

FOCUS_CANVAS_JS = """
(() => {
  const canvas = document.getElementById("game-canvas");
  if (!canvas) return false;
  canvas.focus({ preventScroll: true });
  window.scrollTo(0, 0);
  return document.activeElement === canvas;
})()
"""

CANVAS_STILL_FOCUSED_JS = (
    "document.activeElement === document.getElementById('game-canvas')"
)


def scroll_report(session: DevTools) -> dict:
    """Active element, scroll offset, and the two heights, for diagnostics."""
    return dict(session.evaluate(SCROLL_REPORT_JS))


def describe_scroll(session: DevTools, deltas: dict[str, float] | None = None) -> str:
    report = scroll_report(session)
    detail = (
        f"active element {report['activeElement']}, "
        f"scrollY {report['scrollY']}, "
        f"scrollHeight {report['scrollHeight']}, "
        f"innerHeight {report['innerHeight']}"
    )
    if deltas is not None:
        rendered = ", ".join(f"{code} {delta:+g}" for code, delta in deltas.items())
        detail = f"{detail}; per-key scroll deltas: {rendered or 'none recorded'}"
    return detail


def wait_for_scroll_to_settle(session: DevTools, before: float | None = None) -> float:
    """Waits out an animated scroll and returns where the page came to rest.

    Frames are the clock, because a scroll animation advances once per rendered
    frame and can therefore hide entirely between two wall-clock samples on a
    busy renderer. When ``before`` is given, a page that has not moved yet is
    not mistaken for a page that will never move: the wait watches for the
    scroll to start before it waits for the scroll to stop.
    """
    deadline = time.monotonic() + KEY_SCROLL_SECONDS
    session.evaluate(NEXT_FRAMES_JS)
    current = float(session.evaluate(SCROLL_OFFSET_JS))
    while before is not None and current == before and time.monotonic() < deadline:
        session.evaluate(NEXT_FRAMES_JS)
        current = float(session.evaluate(SCROLL_OFFSET_JS))
    while time.monotonic() < deadline:
        session.evaluate(NEXT_FRAMES_JS)
        following = float(session.evaluate(SCROLL_OFFSET_JS))
        if following == current:
            return current
        current = following
    return current


def reset_scroll(session: DevTools) -> None:
    """Returns the page to the top and proves it stayed there.

    A keyboard scroll is an animation that outlives a single ``scrollTo``, so
    the page is sent back to the top until it settles there.
    """
    deadline = time.monotonic() + SCROLL_RESET_SECONDS
    while time.monotonic() < deadline:
        session.evaluate(SCROLL_TO_TOP_JS)
        if wait_for_scroll_to_settle(session) == 0.0:
            return
    raise GateFailure(
        "the play page would not come to rest at the top of the document: "
        + describe_scroll(session)
    )


def press_control_key(
    session: DevTools, code: str, key_code: int, key: str, text: str | None
) -> float:
    """Dispatches one trusted keystroke and reports how far the page scrolled.

    The sequence is the one a real keystroke produces: a raw key down, the
    character event when the key types something, and a key up.
    """
    before = float(session.evaluate(SCROLL_OFFSET_JS))
    identity = {
        "code": code,
        "key": key,
        "windowsVirtualKeyCode": key_code,
        "nativeVirtualKeyCode": key_code,
    }
    session.call("Input.dispatchKeyEvent", {"type": "rawKeyDown", **identity})
    if text is not None:
        session.call("Input.dispatchKeyEvent", {"type": "char", "text": text, **identity})
    session.call("Input.dispatchKeyEvent", {"type": "keyUp", **identity})
    return wait_for_scroll_to_settle(session, before) - before


def press_control_keys(session: DevTools) -> dict[str, float]:
    """Runs the whole reviewed key sequence and records a delta for each key.

    Both phases of the assertion call this and nothing else, so the focused
    page and the probed page can never be sent different keystrokes.
    """
    deltas: dict[str, float] = {}
    for code, key_code, key, text in CONTROL_KEYS:
        reset_scroll(session)
        deltas[code] = press_control_key(session, code, key_code, key, text)
    return deltas


def check_control_keys_do_not_scroll(session: DevTools) -> dict:
    reserve = float(session.evaluate(SCROLL_RESERVE_JS))
    if reserve < MINIMUM_SCROLL_RESERVE_PIXELS:
        raise GateFailure(
            f"the play page reserves {reserve} scrollable pixels, fewer than the "
            f"{MINIMUM_SCROLL_RESERVE_PIXELS} the no-scroll assertion needs to be "
            "worth making: " + describe_scroll(session)
        )

    # Positive control. The same trusted keys must really scroll this page while
    # a neutral element outside the canvas holds focus. Without it, a page that
    # simply cannot scroll would pass the assertion below for the wrong reason.
    probe = dict(session.evaluate(FOCUS_SCROLL_PROBE_JS))
    if not probe.get("probe"):
        raise GateFailure(
            "the play page has no [data-scroll-probe] element, so there is no "
            "neutral focus target to prove the control keys against: "
            + describe_scroll(session)
        )
    if probe.get("insideCanvas"):
        raise GateFailure(
            f"the scroll probe {probe['probe']} is inside the canvas, so focusing "
            "it would not prove anything about an unfocused page: "
            + describe_scroll(session)
        )
    if not probe.get("owned"):
        raise GateFailure(
            f"the scroll probe {probe['probe']} did not take keyboard focus, so "
            "the control keys would not be proved against an unfocused canvas: "
            + describe_scroll(session)
        )

    unfocused = press_control_keys(session)
    inert = [code for code in POSITIVE_CONTROL_KEYS if unfocused.get(code, 0.0) <= 0.0]
    if inert:
        raise GateFailure(
            f"the trusted control keys {inert} did not scroll the page while the "
            "neutral scroll probe held focus, so the focused no-scroll assertion "
            "would prove nothing: " + describe_scroll(session, unfocused)
        )

    reset_scroll(session)
    if not session.evaluate(FOCUS_CANVAS_JS):
        raise GateFailure("the canvas refused keyboard focus: " + describe_scroll(session))

    focused = press_control_keys(session)
    moved = {code: delta for code, delta in focused.items() if delta != 0.0}
    if moved:
        raise GateFailure(
            f"the focused control keys {sorted(moved)} scrolled the page; the "
            "reviewed keys must be owned by the game: "
            + describe_scroll(session, focused)
        )

    resting = float(session.evaluate(SCROLL_OFFSET_JS))
    if resting != 0.0:
        raise GateFailure(
            f"the page came to rest at {resting} after the focused key sequence: "
            + describe_scroll(session, focused)
        )

    if not session.evaluate(CANVAS_STILL_FOCUSED_JS):
        raise GateFailure(
            "the canvas lost focus while the game keys were pressed: "
            + describe_scroll(session, focused)
        )

    return {
        "reserve_pixels": reserve,
        "probe": probe["probe"],
        "unfocused_deltas": unfocused,
        "focused_deltas": focused,
    }


def check_canvas_pixels(
    session: DevTools,
    palette: dict[str, tuple[int, int, int]],
    diagnostics: Path,
) -> dict:
    # Read the geometry again immediately before the capture: anything that
    # scrolled the page would otherwise make the sampled region point at the
    # page chrome instead of the canvas.
    session.evaluate(
        "document.getElementById('game-canvas')"
        ".scrollIntoView({ block: 'center', behavior: 'instant' })"
    )
    time.sleep(0.2)
    geometry = canvas_geometry(session)
    shot = session.call("Page.captureScreenshot", {"format": "png", "fromSurface": True})
    png = base64.b64decode(shot["data"])
    diagnostics.mkdir(parents=True, exist_ok=True)
    (diagnostics / "canvas.png").write_bytes(png)
    image = decode_png(png)

    ratio = image.width / max(float(session.evaluate("window.innerWidth")), 1.0)
    left = max(int(round(float(geometry["x"]) * ratio)), 0)
    top = max(int(round(float(geometry["y"]) * ratio)), 0)
    right = min(int(round((float(geometry["x"]) + float(geometry["width"])) * ratio)), image.width)
    bottom = min(
        int(round((float(geometry["y"]) + float(geometry["height"])) * ratio)), image.height
    )
    captured = (right - left) * (bottom - top)
    expected = float(geometry["width"]) * float(geometry["height"]) * ratio * ratio
    if expected <= 0 or captured < expected * 0.9:
        raise GateFailure(
            f"the canvas region {left},{top}..{right},{bottom} captures "
            f"{captured} of {expected:.0f} canvas pixels in the screenshot"
        )

    names = list(palette)
    colours = [palette[name] for name in names]
    counts = {name: 0 for name in names}
    unmatched = 0
    total = 0
    sums = [0.0, 0.0, 0.0]
    squares = [0.0, 0.0, 0.0]
    step = max(1, (right - left) // 240)
    for y in range(top, bottom, step):
        for x in range(left, right, step):
            red, green, blue = image.pixel(x, y)
            total += 1
            for channel, value in enumerate((red, green, blue)):
                sums[channel] += value
                squares[channel] += float(value) * value
            best_name = None
            best_distance = PALETTE_MATCH_DISTANCE
            for name, (pr, pg, pb) in zip(names, colours):
                distance = ((red - pr) ** 2 + (green - pg) ** 2 + (blue - pb) ** 2) ** 0.5
                if distance < best_distance:
                    best_distance = distance
                    best_name = name
            if best_name is None:
                unmatched += 1
            else:
                counts[best_name] += 1

    if total == 0:
        raise GateFailure("the canvas region contained no sampled pixels")

    variances = [squares[i] / total - (sums[i] / total) ** 2 for i in range(3)]
    if max(variances) < MINIMUM_CHANNEL_VARIANCE:
        raise GateFailure(
            f"the canvas is effectively blank: channel variance {variances} is below "
            f"{MINIMUM_CHANNEL_VARIANCE}"
        )

    present = sorted(
        (name for name, count in counts.items() if count >= total * MINIMUM_CLASS_SHARE),
        key=lambda name: -counts[name],
    )
    if len(present) < MINIMUM_PALETTE_CLASSES:
        raise GateFailure(
            f"the canvas shows {len(present)} approved palette classes "
            f"({present}), fewer than {MINIMUM_PALETTE_CLASSES}; "
            f"unmatched share {unmatched / total:.3f}"
        )

    return {
        "region": [left, top, right, bottom],
        "sampled_pixels": total,
        "variance": variances,
        "palette_classes": present,
        "unmatched_share": unmatched / total,
    }


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--cdp-port", type=int, required=True)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--design-source", type=Path, required=True)
    parser.add_argument("--diagnostics", type=Path, required=True)
    arguments = parser.parse_args()

    palette = read_palette(arguments.design_source)

    for relative in ["", *packaged_urls(arguments.package)]:
        require_http_200(arguments.base_url, relative)

    session = open_page(arguments.cdp_port, arguments.base_url.rstrip("/") + "/")
    try:
        session.call("Page.enable")
        session.call("Runtime.enable")
        elapsed = wait_for_ready(session, arguments.diagnostics)
        check_no_browser_errors(session)
        geometry = canvas_geometry(session)
        check_canvas(geometry)
        scroll = check_control_keys_do_not_scroll(session)
        check_no_browser_errors(session)
        pixels = check_canvas_pixels(session, palette, arguments.diagnostics)
    except GateFailure as failure:
        write_diagnostics(session, arguments.diagnostics)
        print(f"browser gate failed: {failure}", file=sys.stderr)
        session.close()
        return 1
    finally:
        pass

    summary = {
        "ready_seconds": round(elapsed, 2),
        "canvas": {
            "width": geometry["width"],
            "height": geometry["height"],
            "buffer": [geometry["bufferWidth"], geometry["bufferHeight"]],
        },
        "pixels": pixels,
        "scroll": scroll,
    }
    arguments.diagnostics.mkdir(parents=True, exist_ok=True)
    (arguments.diagnostics / "browser-gate.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps(summary, indent=2, sort_keys=True))
    session.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
