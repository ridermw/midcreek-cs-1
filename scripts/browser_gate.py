#!/usr/bin/env python3
"""Repository-owned headless browser gate for the packaged WASM game.

Speaks the Chrome DevTools Protocol over a hand-rolled RFC 6455 WebSocket
client built from the Python standard library only. Nothing here downloads a
browser automation package, and nothing here needs npm.

The gate proves, independently:

* every packaged URL is served with HTTP 200;
* the game reports ``data-game-state="ready"`` within the readiness budget;
* ``#browser-errors`` exists and captured no error and no unhandled rejection;
* the canvas is visible and 16:9 within one pixel;
* trusted Arrow/Q/E/Space input while the canvas is focused does not scroll a
  page that a neutral focus probe has just proved is genuinely scrollable;
* the canvas region of a real screenshot is nonblank and carries at least
  three approved palette classes with real variance.

Every one of those assertions is then made a second time against the same
package embedded in the homepage the site generator really produced, through
the single ``iframe.play-embed`` that page carries. Nothing about that element
is copied out of the generator's source: it is discovered on the generated
page and required to point at the package this run just served.
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
import urllib.parse
import urllib.request
import zlib
from dataclasses import dataclass
from pathlib import Path

READY_TIMEOUT_SECONDS = 30
BROWSER_TIMEOUT_SECONDS = 30
# One absolute deadline bounds the whole gate, and it starts before the first
# socket is opened rather than after a session exists. Every phase below has
# its own budget too, but a browser that answers each call slowly and none of
# them slowly enough to trip one — or that trickles a single message a byte at
# a time forever — would otherwise keep the gate alive indefinitely. Past this
# the run is over, whatever it was doing.
SESSION_BUDGET_SECONDS = 600
# The DevTools upgrade response is a handful of short headers. A server that
# sends more than this before the blank line is not answering the handshake.
MAX_HANDSHAKE_BYTES = 16 * 1024
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


class Deadline:
    """One absolute instant the whole gate has to be finished by.

    It is created before anything connects, so the DNS-free loopback dial, the
    HTTP requests, the WebSocket handshake, every framed read, and every
    DevTools call all spend the same budget. Nothing here can refresh it, which
    is the point: a stream that keeps arriving slowly must not be able to buy
    itself more time simply by never stopping.
    """

    def __init__(self, budget: float = SESSION_BUDGET_SECONDS) -> None:
        self.budget = budget
        self._expires = time.monotonic() + budget

    def remaining(self) -> float:
        """Seconds left, which may be zero or negative."""
        return self._expires - time.monotonic()

    def require(self, doing: str) -> float:
        """The seconds left, or a failure naming what ran out of them."""
        remaining = self.remaining()
        if remaining <= 0.0:
            raise GateFailure(
                f"the browser gate ran past its {self.budget:.0f}s budget while "
                f"{doing}"
            )
        return remaining


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
        deadline: Deadline | None = None,
    ) -> None:
        match = re.match(r"ws://([^/:]+):(\d+)(/.*)", url)
        if not match:
            raise GateFailure(f"unsupported DevTools endpoint: {url}")
        self._max_message_bytes = max_message_bytes
        self._timeout = timeout
        self._deadline = Deadline() if deadline is None else deadline
        host, port, resource = match.group(1), int(match.group(2)), match.group(3)
        # Connecting is a network operation like any other, so it spends the
        # same budget: a host that accepts slowly cannot buy the gate more time
        # than it has left.
        remaining = self._deadline.require("connecting to the DevTools endpoint")
        try:
            self._socket = socket.create_connection(
                (host, port), timeout=min(timeout, remaining)
            )
        except OSError as failure:
            self._deadline.require("connecting to the DevTools endpoint")
            raise GateFailure(
                f"the DevTools endpoint at {host}:{port} could not be reached: {failure}"
            ) from failure
        self._socket.settimeout(min(timeout, remaining))
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
        self._sendall(request.encode("ascii"), "sending the DevTools handshake")
        # The upgrade response is read under the same deadline as everything
        # else, and its headers are capped: a server that answers with an
        # endless header block is not completing a handshake.
        while b"\r\n\r\n" not in self._buffer:
            if len(self._buffer) > MAX_HANDSHAKE_BYTES:
                raise GateFailure(
                    f"the DevTools handshake sent {len(self._buffer)} header bytes "
                    f"without finishing, over the {MAX_HANDSHAKE_BYTES} byte limit"
                )
            self._read_more()
        head, self._buffer = self._buffer.split(b"\r\n\r\n", 1)
        if b"101" not in head.split(b"\r\n", 1)[0]:
            raise GateFailure(f"DevTools refused the WebSocket upgrade: {head!r}")

    def _bound(self, doing: str) -> float:
        """Clamps the socket to what is left of the whole gate, and says so.

        Every blocking operation on this socket goes through here, so no read
        and no write can outlive the budget by staying just inside a per-call
        timeout that resets on every byte.
        """
        remaining = self._deadline.require(doing)
        bounded = min(self._timeout, remaining)
        try:
            self._socket.settimeout(bounded)
        except OSError:
            pass
        return bounded

    def _sendall(self, data: bytes, doing: str) -> None:
        self._bound(doing)
        try:
            self._socket.sendall(data)
        except TimeoutError as failure:
            # A write that timed out because the budget clamped it is a budget
            # failure, not a slow peer; say which one it was.
            self._deadline.require(doing)
            raise GateFailure(
                f"the DevTools WebSocket stopped accepting {len(data)} bytes "
                f"while {doing}: {failure}"
            ) from failure
        except OSError as failure:
            raise GateFailure(
                f"the DevTools WebSocket could not be written while {doing}: {failure}"
            ) from failure

    def _read_more(self) -> None:
        # Every read is bounded by what is left of the whole gate, so a stream
        # that trickles one byte at a time can never outlive the budget by
        # staying just inside a per-read timeout forever.
        self._bound("reading from the DevTools WebSocket")
        try:
            chunk = self._socket.recv(65536)
        except TimeoutError as failure:
            # A read that timed out because the gate's own budget clamped it is
            # a budget failure, not a quiet browser; say which one it was.
            self._deadline.require("reading from the DevTools WebSocket")
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
        self._sendall(bytes(header) + masked, "sending a DevTools command")

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
        self._sendall(bytes(header) + masked, "answering a DevTools ping")

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

    def set_timeout(self, timeout: float) -> None:
        """Bounds how long one read may block, in seconds."""
        self._timeout = max(timeout, 0.0)


class DevTools:
    """A single-target Chrome DevTools Protocol session."""

    def __init__(self, websocket_url: str, deadline: Deadline | None = None) -> None:
        self.deadline = Deadline() if deadline is None else deadline
        self._socket = WebSocket(websocket_url, deadline=self.deadline)
        self._next_id = 0

    def remaining(self) -> float:
        """Seconds left in the whole gate, which may be negative."""
        return self.deadline.remaining()

    def call(self, method: str, params: dict | None = None, timeout: float = BROWSER_TIMEOUT_SECONDS) -> dict:
        # No single call may outlive the gate it belongs to. Without this a
        # browser that answers every call just inside its own timeout keeps
        # the gate alive for as long as it likes.
        remaining = self.deadline.require(f"waiting for {method}")
        bounded = min(timeout, remaining)
        self._next_id += 1
        message_id = self._next_id
        self._socket.send(json.dumps({"id": message_id, "method": method, "params": params or {}}))
        self._socket.set_timeout(bounded)
        deadline = time.monotonic() + bounded
        while time.monotonic() < deadline:
            message = json.loads(self._socket.receive())
            if message.get("id") != message_id:
                continue
            if "error" in message:
                raise GateFailure(f"{method} failed: {message['error']}")
            return message.get("result", {})
        raise GateFailure(f"{method} did not answer within {bounded:.0f}s")

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
    """Reads the approved cel-shift palette straight from the Rust source.

    The roster is derived, never assumed: ``PaletteRole::ALL`` is the game's
    own declaration of which roles exist, so the constants parsed here have to
    be exactly the ones it names. A role added to the enum and forgotten in the
    constants — or the reverse — fails here instead of quietly shrinking the
    set of colours the canvas is allowed to be judged against.
    """
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
    declared = read_palette_roles(design_source, text)
    missing = sorted(declared - set(palette))
    extra = sorted(set(palette) - declared)
    if missing or extra:
        raise GateFailure(
            f"the approved palette in {design_source} does not match the roles "
            f"PaletteRole::ALL declares: missing {missing}, unexpected {extra}"
        )
    return palette


def read_palette_roles(design_source: Path, text: str | None = None) -> set[str]:
    """The constant names ``PaletteRole::ALL`` declares, in the game's source.

    ``Self::WorkerHiVis`` names the ``WORKER_HI_VIS`` constant, so the enum
    roster is translated into constant names rather than counted.
    """
    text = design_source.read_text(encoding="utf-8") if text is None else text
    match = re.search(
        r"pub const ALL: \[Self; (?P<count>\d+)\] = \[(?P<body>.*?)\];", text, re.DOTALL
    )
    if not match:
        raise GateFailure(f"{design_source} declares no PaletteRole::ALL roster")
    variants = re.findall(r"Self::([A-Za-z0-9]+)", match.group("body"))
    if len(variants) != int(match.group("count")):
        raise GateFailure(
            f"PaletteRole::ALL in {design_source} claims {match.group('count')} "
            f"roles and lists {len(variants)}"
        )
    return {re.sub(r"(?<!^)(?=[A-Z])", "_", variant).upper() for variant in variants}


# ---------------------------------------------------------------------------
# HTTP checks
# ---------------------------------------------------------------------------


def resolve_url(base_url: str, relative: str) -> str:
    """Joins a served path onto a base URL with every segment quoted.

    A packaged file name is a file-system name, not a URL: a space or a `#` in
    one would silently address a different resource, or none at all.
    """
    quoted = "/".join(
        urllib.parse.quote(segment, safe="") for segment in relative.lstrip("/").split("/")
    )
    return f"{base_url.rstrip('/')}/{quoted}" if quoted else f"{base_url.rstrip('/')}/"


def response_socket(response: object) -> object | None:
    """The socket underneath an ``http.client`` response, when it is reachable.

    urllib does not publish it, so it is walked defensively — but it is not
    optional: without it a body that stops arriving blocks on the timeout the
    connection was opened with, which was clamped to a budget that has since
    shrunk. A test proves this lookup really finds it on the Python that runs.
    """
    fp = getattr(response, "fp", None)
    for holder in (getattr(fp, "raw", None), fp, response):
        found = getattr(holder, "_sock", None)
        if found is not None and hasattr(found, "settimeout"):
            return found
    return None


def read_bounded(response: object, limit: int, deadline: Deadline, doing: str) -> bytes:
    """Reads at most ``limit`` bytes, spending the whole gate's budget.

    A socket timeout is an inactivity timer: a server that sends one byte
    whenever it is about to expire resets it forever. So the deadline is
    checked between chunks, and the socket is re-clamped to what is left of it
    before each one — otherwise a body that trickles and then stops dead would
    block on a timeout set when the budget was much larger.
    """
    data = bytearray()
    connection = response_socket(response)
    reader = getattr(response, "read1", None) or response.read
    while len(data) < limit:
        remaining = deadline.require(doing)
        if connection is not None:
            try:
                connection.settimeout(min(BROWSER_TIMEOUT_SECONDS, remaining))
            except OSError:
                pass
        chunk = read_chunk(reader, min(4096, limit - len(data)), deadline, doing)
        if not chunk:
            break
        data += chunk
    return bytes(data)


def read_chunk(reader: object, size: int, deadline: Deadline, doing: str) -> bytes:
    """One clamped read, with a timeout reported as what it really was."""
    try:
        return reader(size)
    except TimeoutError as failure:
        # The clamp above is what ended this read, so an expired budget is the
        # honest explanation; a peer that went quiet inside a live budget is
        # the other one.
        deadline.require(doing)
        raise GateFailure(
            f"the response stopped arriving while {doing}: {failure}"
        ) from failure
    except OSError as failure:
        raise GateFailure(
            f"the response could not be read while {doing}: {failure}"
        ) from failure


def require_http_200(base_url: str, relative: str, deadline: Deadline) -> None:
    url = resolve_url(base_url, relative)
    timeout = min(BROWSER_TIMEOUT_SECONDS, deadline.require(f"fetching {relative}"))
    request = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            if response.status != 200:
                raise GateFailure(f"{relative} returned HTTP {response.status}")
            read_bounded(response, 1024, deadline, f"reading {relative}")
    except urllib.error.URLError as failure:
        raise GateFailure(f"{relative} could not be fetched: {failure}") from failure


def packaged_urls(package: Path) -> list[str]:
    urls = []
    for path in sorted(package.rglob("*")):
        if path.is_file():
            urls.append(path.relative_to(package).as_posix())
    return urls


# ---------------------------------------------------------------------------
# Document scopes
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Scope:
    """One document the gate drives, and how JavaScript reaches into it.

    Every phase below is written once, against `__DOC__` and `__WIN__`, so the
    standalone play page and the same package embedded in the published
    homepage are proved by the same code rather than by two assertions that can
    drift apart. `__ORIGIN__` is where that document's viewport sits inside a
    screenshot of the top-level page, and `__CLIP__` is the region of that
    screenshot the document can actually be seen in, so a canvas inside a frame
    is judged in the pixels that were really captured.
    """

    label: str
    doc: str
    win: str
    origin: str
    clip: str

    def render(self, template: str) -> str:
        return (
            template.replace("__DOC__", self.doc)
            .replace("__WIN__", self.win)
            .replace("__ORIGIN__", self.origin)
            .replace("__CLIP__", self.clip)
        )


#: The top-level document: its viewport is the screenshot, so nothing offsets.
TOP_SCOPE = Scope(
    label="the standalone play page",
    doc="document",
    win="window",
    origin="({ x: 0, y: 0 })",
    clip="({ x: 0, y: 0, width: window.innerWidth, height: window.innerHeight })",
)

#: The single playable iframe on the published homepage. The gate never writes
#: this element out of the generator's source; it finds the one the generated
#: page really carries and refuses anything else.
EMBED_ELEMENT_JS = "document.getElementsByTagName('iframe')[0]"

EMBED_SCOPE = Scope(
    label="the embedded player on the published homepage",
    doc=f"({EMBED_ELEMENT_JS}.contentDocument)",
    win=f"({EMBED_ELEMENT_JS}.contentWindow)",
    origin=f"({EMBED_ELEMENT_JS}.getBoundingClientRect())",
    clip=f"({EMBED_ELEMENT_JS}.getBoundingClientRect())",
)


class Page:
    """One document, bound to the session that drives it.

    Binding the scope here rather than passing it beside the session is what
    stops a phase from ever being run against the wrong document: there is no
    call that can name one and mean the other.
    """

    def __init__(self, session: DevTools, scope: Scope) -> None:
        self.session = session
        self.scope = scope
        self.label = scope.label

    def evaluate(self, template: str) -> object:
        return self.session.evaluate(self.scope.render(template))

    def evaluate_top(self, expression: str) -> object:
        """Evaluates in the top-level document, whatever this page is.

        The screenshot belongs to the top-level page, so the scale between CSS
        pixels and captured pixels is read from there even when the canvas
        being measured lives inside a frame.
        """
        return self.session.evaluate(expression)

    def call(self, method: str, params: dict | None = None) -> dict:
        return self.session.call(method, params)


# ---------------------------------------------------------------------------
# Browser session
# ---------------------------------------------------------------------------


def open_page(cdp_port: int, url: str, deadline: Deadline) -> DevTools:
    version_url = f"http://127.0.0.1:{cdp_port}/json/version"
    while True:
        remaining = deadline.require("waiting for a DevTools endpoint")
        try:
            with urllib.request.urlopen(version_url, timeout=min(2.0, remaining)) as response:
                json.loads(read_bounded(response, 64 * 1024, deadline, "reading the DevTools version"))
                break
        except (urllib.error.URLError, ConnectionError, TimeoutError, socket.timeout):
            time.sleep(0.25)

    timeout = min(BROWSER_TIMEOUT_SECONDS, deadline.require("opening a browser target"))
    new_target = urllib.request.Request(
        f"http://127.0.0.1:{cdp_port}/json/new?{urllib.parse.quote(url, safe=':/?&=%')}",
        method="PUT",
    )
    with urllib.request.urlopen(new_target, timeout=timeout) as response:
        target = json.loads(
            read_bounded(response, 64 * 1024, deadline, "reading the new browser target")
        )
    session = DevTools(target["webSocketDebuggerUrl"], deadline=deadline)
    session.call("Page.enable")
    session.call("Runtime.enable")
    return session


GAME_STATE_JS = "__DOC__ && __DOC__.body ? __DOC__.body.dataset.gameState : 'missing'"

# A missing sink is not an empty one. The play page owns `#browser-errors`, and
# a document that lost it would report "no errors" for the rest of the run
# however loudly it was failing, so its presence is asserted before its text.
BROWSER_ERRORS_JS = """
(() => {
  const doc = __DOC__;
  const sink = doc ? doc.getElementById("browser-errors") : null;
  return { present: sink !== null, text: sink ? sink.textContent || "" : "" };
})()
"""


def write_diagnostics(session: Page, diagnostics: Path) -> None:
    diagnostics.mkdir(parents=True, exist_ok=True)
    prefix = "page" if session.scope is TOP_SCOPE else "embed"
    # Diagnostics are written from a session that has already failed, so every
    # capture is best effort: whatever went wrong with the browser must not
    # also swallow the page and the screenshot that explain it.
    try:
        html = session.evaluate_top("document.documentElement.outerHTML")
        (diagnostics / f"{prefix}.html").write_text(str(html), encoding="utf-8")
    except Exception:  # noqa: BLE001 - a failed capture is never the failure
        pass
    try:
        shot = session.call("Page.captureScreenshot", {"format": "png"})
        (diagnostics / f"{prefix}.png").write_bytes(base64.b64decode(shot["data"]))
    except Exception:  # noqa: BLE001 - a failed capture is never the failure
        pass


def wait_for_ready(session: Page, diagnostics: Path) -> float:
    started = time.monotonic()
    budget = min(READY_TIMEOUT_SECONDS, session.session.remaining())
    deadline = started + budget
    state = "missing"
    while time.monotonic() < deadline:
        state = session.evaluate(GAME_STATE_JS)
        if state == "ready":
            return time.monotonic() - started
        if state == "error":
            break
        time.sleep(0.25)
    errors = session.evaluate(BROWSER_ERRORS_JS)
    write_diagnostics(session, diagnostics)
    raise GateFailure(
        f"{session.label} reported data-game-state={state!r} instead of 'ready' "
        f"within {budget:.0f}s; captured: {errors!r}"
    )


def check_no_browser_errors(session: Page) -> None:
    captured = dict(session.evaluate(BROWSER_ERRORS_JS))
    if not captured.get("present"):
        raise GateFailure(
            f"{session.label} has no #browser-errors element, so nothing was "
            "watching for the errors this gate reports on"
        )
    if str(captured.get("text", "")).strip():
        raise GateFailure(f"{session.label} captured errors: {captured['text']!r}")


CANVAS_GEOMETRY_JS = """
(() => {
  const doc = __DOC__;
  if (!doc) return null;
  const canvas = doc.getElementById("game-canvas");
  if (!canvas) return null;
  const rect = canvas.getBoundingClientRect();
  const origin = __ORIGIN__;
  const clip = __CLIP__;
  const style = __WIN__.getComputedStyle(canvas);
  return {
    x: rect.x + origin.x,
    y: rect.y + origin.y,
    width: rect.width,
    height: rect.height,
    clip: { x: clip.x, y: clip.y, width: clip.width, height: clip.height },
    bufferWidth: canvas.width,
    bufferHeight: canvas.height,
    visible: style.visibility !== "hidden" && style.display !== "none",
  };
})()
"""


def canvas_geometry(session: Page) -> dict:
    geometry = session.evaluate(CANVAS_GEOMETRY_JS)
    if not geometry:
        raise GateFailure(f"{session.label} has no #game-canvas element")
    return geometry


def check_canvas(session: Page, geometry: dict) -> None:
    if not geometry["visible"]:
        raise GateFailure(f"the canvas in {session.label} is not visible")
    width = float(geometry["width"])
    height = float(geometry["height"])
    if width <= 0 or height <= 0:
        raise GateFailure(f"the canvas in {session.label} has no size: {width}x{height}")
    expected = height * 16.0 / 9.0
    if abs(width - expected) > ASPECT_TOLERANCE_PIXELS:
        raise GateFailure(
            f"the canvas in {session.label} is {width}x{height}, which is not 16:9 "
            f"within {ASPECT_TOLERANCE_PIXELS} pixel (expected width {expected:.2f})"
        )
    if int(geometry["bufferWidth"]) <= 0 or int(geometry["bufferHeight"]) <= 0:
        raise GateFailure(f"the canvas in {session.label} has an empty drawing buffer")


SCROLL_OFFSET_JS = "__WIN__.scrollY"

SCROLL_TO_TOP_JS = "__WIN__.scrollTo(0, 0)"

SCROLL_RESERVE_JS = "__DOC__.documentElement.scrollHeight - __WIN__.innerHeight"

SCROLL_CANVAS_INTO_VIEW_JS = (
    "__DOC__.getElementById('game-canvas')"
    ".scrollIntoView({ block: 'center', behavior: 'instant' })"
)

# A scroll animation advances once per rendered frame, so frames are the only
# clock it can be measured against. A wall clock reads a janky renderer as a
# settled page and turns a real scroll into a false zero.
NEXT_FRAMES_JS = """
new Promise((resolve) => {
  let remaining = 2;
  const step = () => (remaining-- > 0 ? __WIN__.requestAnimationFrame(step) : resolve(true));
  step();
})
"""

SCROLL_REPORT_JS = """
(() => {
  const doc = __DOC__;
  const win = __WIN__;
  const node = doc.activeElement;
  const name = node
    ? node.tagName.toLowerCase() + (node.id ? "#" + node.id : "")
    : "none";
  return {
    activeElement: name,
    scrollY: win.scrollY,
    scrollHeight: doc.documentElement.scrollHeight,
    innerHeight: win.innerHeight,
  };
})()
"""

# The probe is a neutral focus target that lives outside the canvas. A button
# or a link would consume Space itself, so the page ships a plain focusable
# region and the gate refuses to continue unless that region really owns focus.
FOCUS_SCROLL_PROBE_JS = """
(() => {
  const doc = __DOC__;
  const win = __WIN__;
  const probe = doc.querySelector("[data-scroll-probe]");
  if (!probe) return { probe: null };
  const canvas = doc.getElementById("game-canvas");
  if (canvas) {
    if (canvas.contains(probe) || probe === canvas) {
      return { probe: probe.id || "unnamed", insideCanvas: true };
    }
    canvas.blur();
  }
  probe.focus({ preventScroll: true });
  win.scrollTo(0, 0);
  return {
    probe: probe.id || "unnamed",
    insideCanvas: false,
    owned: doc.activeElement === probe,
  };
})()
"""

FOCUS_CANVAS_JS = """
(() => {
  const doc = __DOC__;
  const canvas = doc.getElementById("game-canvas");
  if (!canvas) return false;
  canvas.focus({ preventScroll: true });
  __WIN__.scrollTo(0, 0);
  return doc.activeElement === canvas;
})()
"""

CANVAS_STILL_FOCUSED_JS = (
    "__DOC__.activeElement === __DOC__.getElementById('game-canvas')"
)

# The positive control only proves anything about an unfocused canvas while the
# neutral probe is still the element receiving the keys. A page that moved
# focus part way through the sequence measured something else entirely.
PROBE_STILL_FOCUSED_JS = """
(() => {
  const doc = __DOC__;
  const probe = doc.querySelector("[data-scroll-probe]");
  return probe !== null && doc.activeElement === probe;
})()
"""


def scroll_report(session: Page) -> dict:
    """Active element, scroll offset, and the two heights, for diagnostics."""
    return dict(session.evaluate(SCROLL_REPORT_JS))


def describe_scroll(session: Page, deltas: dict[str, float] | None = None) -> str:
    report = scroll_report(session)
    detail = (
        f"in {session.label}: "
        f"active element {report['activeElement']}, "
        f"scrollY {report['scrollY']}, "
        f"scrollHeight {report['scrollHeight']}, "
        f"innerHeight {report['innerHeight']}"
    )
    if deltas is not None:
        rendered = ", ".join(f"{code} {delta:+g}" for code, delta in deltas.items())
        detail = f"{detail}; per-key scroll deltas: {rendered or 'none recorded'}"
    return detail


def wait_for_scroll_to_settle(session: Page, before: float | None = None) -> float:
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


def reset_scroll(session: Page) -> None:
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
        "the page would not come to rest at the top of the document "
        + describe_scroll(session)
    )


def press_control_key(
    session: Page, code: str, key_code: int, key: str, text: str | None
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


def press_control_keys(session: Page) -> dict[str, float]:
    """Runs the whole reviewed key sequence and records a delta for each key.

    Both phases of the assertion call this and nothing else, so the focused
    page and the probed page can never be sent different keystrokes.
    """
    deltas: dict[str, float] = {}
    for code, key_code, key, text in CONTROL_KEYS:
        reset_scroll(session)
        deltas[code] = press_control_key(session, code, key_code, key, text)
    return deltas


def check_control_keys_do_not_scroll(session: Page) -> dict:
    reserve = float(session.evaluate(SCROLL_RESERVE_JS))
    if reserve < MINIMUM_SCROLL_RESERVE_PIXELS:
        raise GateFailure(
            f"{session.label} reserves {reserve} scrollable pixels, fewer than the "
            f"{MINIMUM_SCROLL_RESERVE_PIXELS} the no-scroll assertion needs to be "
            "worth making " + describe_scroll(session)
        )

    # Positive control. The same trusted keys must really scroll this page while
    # a neutral element outside the canvas holds focus. Without it, a page that
    # simply cannot scroll would pass the assertion below for the wrong reason.
    probe = dict(session.evaluate(FOCUS_SCROLL_PROBE_JS))
    if not probe.get("probe"):
        raise GateFailure(
            f"{session.label} has no [data-scroll-probe] element, so there is no "
            "neutral focus target to prove the control keys against "
            + describe_scroll(session)
        )
    if probe.get("insideCanvas"):
        raise GateFailure(
            f"the scroll probe {probe['probe']} is inside the canvas, so focusing "
            "it would not prove anything about an unfocused page "
            + describe_scroll(session)
        )
    if not probe.get("owned"):
        raise GateFailure(
            f"the scroll probe {probe['probe']} did not take keyboard focus, so "
            "the control keys would not be proved against an unfocused canvas "
            + describe_scroll(session)
        )

    unfocused = press_control_keys(session)
    if not session.evaluate(PROBE_STILL_FOCUSED_JS):
        raise GateFailure(
            f"the scroll probe {probe['probe']} did not still own keyboard focus "
            "after the positive control, so the keys that moved the page were "
            "not the ones an unfocused canvas is judged against "
            + describe_scroll(session, unfocused)
        )
    inert = [code for code in POSITIVE_CONTROL_KEYS if unfocused.get(code, 0.0) <= 0.0]
    if inert:
        raise GateFailure(
            f"the trusted control keys {inert} did not scroll {session.label} while "
            "the neutral scroll probe held focus, so the focused no-scroll "
            "assertion would prove nothing " + describe_scroll(session, unfocused)
        )

    reset_scroll(session)
    if not session.evaluate(FOCUS_CANVAS_JS):
        raise GateFailure(
            f"the canvas in {session.label} refused keyboard focus "
            + describe_scroll(session)
        )

    focused = press_control_keys(session)
    moved = {code: delta for code, delta in focused.items() if delta != 0.0}
    if moved:
        raise GateFailure(
            f"the focused control keys {sorted(moved)} scrolled {session.label}; the "
            "reviewed keys must be owned by the game " + describe_scroll(session, focused)
        )

    resting = float(session.evaluate(SCROLL_OFFSET_JS))
    if resting != 0.0:
        raise GateFailure(
            f"{session.label} came to rest at {resting} after the focused key "
            "sequence " + describe_scroll(session, focused)
        )

    if not session.evaluate(CANVAS_STILL_FOCUSED_JS):
        raise GateFailure(
            f"the canvas in {session.label} lost focus while the game keys were "
            "pressed " + describe_scroll(session, focused)
        )

    return {
        "reserve_pixels": reserve,
        "probe": probe["probe"],
        "unfocused_deltas": unfocused,
        "focused_deltas": focused,
    }


def _intersect(canvas: dict, clip: dict) -> tuple[float, float, float, float]:
    left = max(float(canvas["x"]), float(clip["x"]))
    top = max(float(canvas["y"]), float(clip["y"]))
    right = min(
        float(canvas["x"]) + float(canvas["width"]),
        float(clip["x"]) + float(clip["width"]),
    )
    bottom = min(
        float(canvas["y"]) + float(canvas["height"]),
        float(clip["y"]) + float(clip["height"]),
    )
    return left, top, max(right, left), max(bottom, top)


def check_canvas_pixels(
    session: Page,
    palette: dict[str, tuple[int, int, int]],
    diagnostics: Path,
    artifact: str,
) -> dict:
    # Read the geometry again immediately before the capture: anything that
    # scrolled the document would otherwise make the sampled region point at
    # the page chrome instead of the canvas. In a frame the canvas is scrolled
    # into the frame's own viewport, which is also the region it can be seen in.
    session.evaluate(SCROLL_CANVAS_INTO_VIEW_JS)
    time.sleep(0.2)
    geometry = canvas_geometry(session)
    shot = session.call("Page.captureScreenshot", {"format": "png", "fromSurface": True})
    png = base64.b64decode(shot["data"])
    diagnostics.mkdir(parents=True, exist_ok=True)
    (diagnostics / artifact).write_bytes(png)
    image = decode_png(png)

    # The canvas is only judged where it can actually be seen: a frame clips
    # its document, so the region is the part of the canvas inside that frame.
    css_left, css_top, css_right, css_bottom = _intersect(geometry, geometry["clip"])
    visible = (css_right - css_left) * (css_bottom - css_top)
    drawn = float(geometry["width"]) * float(geometry["height"])
    if drawn <= 0 or visible < drawn * 0.9:
        raise GateFailure(
            f"only {visible:.0f} of {drawn:.0f} canvas pixels are inside "
            f"{session.label}; the canvas has to be visible to be judged"
        )

    ratio = image.width / max(float(session.evaluate_top("window.innerWidth")), 1.0)
    left = max(int(round(css_left * ratio)), 0)
    top = max(int(round(css_top * ratio)), 0)
    right = min(int(round(css_right * ratio)), image.width)
    bottom = min(int(round(css_bottom * ratio)), image.height)
    captured = (right - left) * (bottom - top)
    expected = visible * ratio * ratio
    if expected <= 0 or captured < expected * 0.9:
        raise GateFailure(
            f"the canvas region {left},{top}..{right},{bottom} captures "
            f"{captured} of {expected:.0f} canvas pixels in the screenshot of "
            f"{session.label}"
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
        raise GateFailure(f"the canvas region of {session.label} contained no pixels")

    variances = [squares[i] / total - (sums[i] / total) ** 2 for i in range(3)]
    if max(variances) < MINIMUM_CHANNEL_VARIANCE:
        raise GateFailure(
            f"the canvas in {session.label} is effectively blank: channel variance "
            f"{variances} is below {MINIMUM_CHANNEL_VARIANCE}"
        )

    present = sorted(
        (name for name, count in counts.items() if count >= total * MINIMUM_CLASS_SHARE),
        key=lambda name: -counts[name],
    )
    if len(present) < MINIMUM_PALETTE_CLASSES:
        raise GateFailure(
            f"the canvas in {session.label} shows {len(present)} approved palette "
            f"classes ({present}), fewer than {MINIMUM_PALETTE_CLASSES}; "
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
# The published homepage embed
# ---------------------------------------------------------------------------

# The homepage is the one the site generator really produced, so the embed is
# discovered on it rather than reconstructed from the generator's source. The
# published contract is one playable frame, carrying the `play-embed` class,
# pointing at the package this run just packaged and served.
DISCOVER_EMBED_JS = """
(() => {
  const frames = Array.from(document.getElementsByTagName("iframe"));
  if (frames.length !== 1) return { count: frames.length };
  const frame = frames[0];
  const rect = frame.getBoundingClientRect();
  return {
    count: 1,
    class: frame.className,
    classes: Array.from(frame.classList),
    src: frame.getAttribute("src"),
    resolved: frame.src,
    readable: frame.contentDocument !== null,
    width: rect.width,
    height: rect.height,
  };
})()
"""

#: The class the generated homepage has to give its playable frame.
PUBLISHED_EMBED_CLASS = "play-embed"


def discover_embed(session: Page, expected_url: str) -> dict:
    """Finds the published playable frame and proves it is the published one."""
    report = dict(session.evaluate(DISCOVER_EMBED_JS))
    if report.get("count") != 1:
        raise GateFailure(
            f"the generated homepage carries {report.get('count')} iframes; the "
            "published embed has to be exactly one for this gate to prove it"
        )
    if PUBLISHED_EMBED_CLASS not in report.get("classes", []):
        raise GateFailure(
            f"the homepage embed is {report.get('class')!r}, not the published "
            f"{PUBLISHED_EMBED_CLASS!r} contract"
        )
    if report.get("resolved") != expected_url:
        raise GateFailure(
            f"the homepage embed resolves to {report.get('resolved')!r}, not to "
            f"the published package at {expected_url!r}"
        )
    if not report.get("readable"):
        raise GateFailure(
            "the homepage embed has no reachable document, so the published game "
            "cannot be proved inside it"
        )
    if float(report.get("width", 0)) <= 0 or float(report.get("height", 0)) <= 0:
        raise GateFailure(
            f"the homepage embed is laid out at {report.get('width')}x"
            f"{report.get('height')}, so nothing in it can be seen"
        )
    return report


def prove_page(
    session: Page,
    palette: dict[str, tuple[int, int, int]],
    diagnostics: Path,
    artifact: str,
) -> dict:
    """Every assertion this gate makes, against one document.

    The standalone page and the embedded player go through this same function,
    so the published homepage is held to the readiness, error-sink, canvas,
    keyboard-ownership, and rendered-pixel contracts the direct link is, rather
    than to a weaker version of them.
    """
    elapsed = wait_for_ready(session, diagnostics)
    check_no_browser_errors(session)
    geometry = canvas_geometry(session)
    check_canvas(session, geometry)
    scroll = check_control_keys_do_not_scroll(session)
    check_no_browser_errors(session)
    pixels = check_canvas_pixels(session, palette, diagnostics, artifact)
    return {
        "ready_seconds": round(elapsed, 2),
        "canvas": {
            "width": geometry["width"],
            "height": geometry["height"],
            "buffer": [geometry["bufferWidth"], geometry["bufferHeight"]],
        },
        "pixels": pixels,
        "scroll": scroll,
    }


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def gate_arguments(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--cdp-port", type=int, required=True)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--design-source", type=Path, required=True)
    parser.add_argument("--diagnostics", type=Path, required=True)
    parser.add_argument(
        "--hub-url",
        required=True,
        help="the generated site homepage that embeds the packaged game",
    )
    return parser.parse_args(argv)


def run_gate(
    arguments: argparse.Namespace, pages: list[Page], deadline: Deadline
) -> tuple[dict, dict]:
    """Proves the package standalone, then again inside the published page."""
    palette = read_palette(arguments.design_source)

    for relative in ["", *packaged_urls(arguments.package)]:
        require_http_200(arguments.base_url, relative, deadline)
    require_http_200(arguments.hub_url, "", deadline)

    standalone = Page(
        open_page(arguments.cdp_port, resolve_url(arguments.base_url, ""), deadline),
        TOP_SCOPE,
    )
    pages.append(standalone)
    summary = prove_page(standalone, palette, arguments.diagnostics, "canvas.png")

    hub = open_page(arguments.cdp_port, resolve_url(arguments.hub_url, ""), deadline)
    homepage = Page(hub, TOP_SCOPE)
    embedded = Page(hub, EMBED_SCOPE)
    pages.append(embedded)
    expected = resolve_url(arguments.base_url, "index.html")
    embed = discover_embed(homepage, expected)
    proof = prove_page(embedded, palette, arguments.diagnostics, "embed-canvas.png")
    return summary, {
        "class": PUBLISHED_EMBED_CLASS,
        "src": embed["src"],
        "frame": {"width": embed["width"], "height": embed["height"]},
        **proof,
    }


def report_failure(pages: list[Page], diagnostics: Path, message: str) -> int:
    """Captures whatever the failed session can still show, then gives up."""
    if pages:
        write_diagnostics(pages[-1], diagnostics)
    print(f"browser gate failed: {message}", file=sys.stderr)
    return 1


def main(argv: list[str] | None = None) -> int:
    arguments = gate_arguments(argv)
    # The budget starts here, before a socket exists, so the handshake, the
    # HTTP checks, and every framed read all spend the same one.
    deadline = Deadline()
    pages: list[Page] = []
    try:
        summary, embed = run_gate(arguments, pages, deadline)
    except GateFailure as failure:
        return report_failure(pages, arguments.diagnostics, str(failure))
    # A driver defect is not a passing gate. Anything the phases above did not
    # anticipate is reported like any other failure, with the same diagnostics
    # written and the same sessions closed below.
    except Exception as failure:  # noqa: BLE001
        return report_failure(
            pages,
            arguments.diagnostics,
            f"unexpected {type(failure).__name__}: {failure}",
        )
    finally:
        for page in pages:
            page.session.close()

    # Closing the sessions and writing the reports are the last things this run
    # does, and running out of time during either of them is still running out
    # of time. The budget is therefore checked after the cleanup and after each
    # report, and anything already written is taken back: a partial report left
    # behind by an expired run is evidence the site would otherwise publish.
    #
    # `browser-gate.json` is a schema the site generator reads strictly and
    # refuses to grow, so the embed proof is kept beside it rather than added
    # to it. A field this gate invented would make the whole browser evidence
    # unreadable and silently drop it from the published run.
    reports = (
        (arguments.diagnostics / "browser-gate.json", summary),
        (arguments.diagnostics / "embed-gate.json", embed),
    )
    try:
        deadline.require("closing the browser sessions")
        arguments.diagnostics.mkdir(parents=True, exist_ok=True)
        for path, document in reports:
            path.write_text(
                json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
            deadline.require(f"writing {path.name}")
    except GateFailure as failure:
        for path, _ in reports:
            path.unlink(missing_ok=True)
        print(f"browser gate failed: {failure}", file=sys.stderr)
        return 1

    print(json.dumps({"standalone": summary, "embedded": embed}, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
