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
  page that is genuinely scrollable;
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
ASPECT_TOLERANCE_PIXELS = 1.0
MINIMUM_PALETTE_CLASSES = 3
MINIMUM_CLASS_SHARE = 0.01
MINIMUM_CHANNEL_VARIANCE = 25.0
PALETTE_MATCH_DISTANCE = 48.0
CONTROL_KEYS = (
    ("ArrowUp", 38, "ArrowUp"),
    ("ArrowDown", 40, "ArrowDown"),
    ("ArrowLeft", 37, "ArrowLeft"),
    ("ArrowRight", 39, "ArrowRight"),
    ("KeyQ", 81, "q"),
    ("KeyE", 69, "e"),
    ("Space", 32, " "),
)


class GateFailure(Exception):
    """A browser gate assertion that did not hold."""


# ---------------------------------------------------------------------------
# WebSocket client
# ---------------------------------------------------------------------------


class WebSocket:
    """The smallest RFC 6455 text client the DevTools Protocol needs."""

    def __init__(self, url: str, timeout: float = BROWSER_TIMEOUT_SECONDS) -> None:
        match = re.match(r"ws://([^/:]+):(\d+)(/.*)", url)
        if not match:
            raise GateFailure(f"unsupported DevTools endpoint: {url}")
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
        chunk = self._socket.recv(65536)
        if not chunk:
            raise GateFailure("the DevTools WebSocket closed unexpectedly")
        self._buffer += chunk

    def send(self, payload: str) -> None:
        data = payload.encode("utf-8")
        header = bytearray([0x81])
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
        while True:
            frame = self._read_frame()
            if frame is None:
                continue
            opcode, payload = frame
            if opcode == 0x1:
                return payload.decode("utf-8")
            if opcode == 0x8:
                raise GateFailure("the browser closed the DevTools connection")
            if opcode == 0x9:
                self._send_pong(payload)

    def _send_pong(self, payload: bytes) -> None:
        mask = os.urandom(4)
        header = bytearray([0x8A, 0x80 | len(payload)]) + mask
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self._socket.sendall(bytes(header) + masked)

    def _need(self, count: int) -> None:
        while len(self._buffer) < count:
            self._read_more()

    def _read_frame(self) -> tuple[int, bytes] | None:
        self._need(2)
        first, second = self._buffer[0], self._buffer[1]
        opcode = first & 0x0F
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
        self._need(offset + length)
        payload = self._buffer[offset : offset + length]
        self._buffer = self._buffer[offset + length :]
        return opcode, bytes(payload)

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
        if state == "ready":
            return READY_TIMEOUT_SECONDS - (deadline - time.monotonic())
        if state == "error":
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


def press_control_keys(session: DevTools) -> None:
    for code, key_code, key in CONTROL_KEYS:
        for event_type in ("keyDown", "keyUp"):
            session.call(
                "Input.dispatchKeyEvent",
                {
                    "type": event_type,
                    "code": code,
                    "key": key,
                    "windowsVirtualKeyCode": key_code,
                    "nativeVirtualKeyCode": key_code,
                },
            )
        time.sleep(0.05)


def check_control_keys_do_not_scroll(session: DevTools) -> None:
    scrollable = session.evaluate(
        "document.documentElement.scrollHeight - window.innerHeight"
    )
    if float(scrollable) <= 0:
        raise GateFailure(
            "the play page is not scrollable, so the no-scroll assertion would be vacuous"
        )

    # Positive control: the same trusted keys must really scroll this page when
    # the canvas is not focused. Without it, a page that simply cannot scroll
    # would pass the assertion below for the wrong reason.
    session.evaluate(
        """
        (() => {
          document.getElementById("game-canvas").blur();
          document.body.focus();
          window.scrollTo(0, 0);
        })()
        """
    )
    press_control_keys(session)
    unfocused_scroll = float(session.evaluate("window.scrollY"))
    if unfocused_scroll <= 0.0:
        raise GateFailure(
            "the trusted control keys did not scroll the unfocused page, so the "
            "focused no-scroll assertion would prove nothing"
        )

    focused = session.evaluate(
        """
        (() => {
          const canvas = document.getElementById("game-canvas");
          canvas.focus({ preventScroll: true });
          window.scrollTo(0, 0);
          return document.activeElement === canvas;
        })()
        """
    )
    if not focused:
        raise GateFailure("the canvas refused keyboard focus")

    press_control_keys(session)

    scroll = session.evaluate("window.scrollY")
    if float(scroll) != 0.0:
        raise GateFailure(
            f"a focused control key scrolled the page to {scroll}; the reviewed keys must be owned by the game"
        )

    still_focused = session.evaluate(
        "document.activeElement === document.getElementById('game-canvas')"
    )
    if not still_focused:
        raise GateFailure("the canvas lost focus while the game keys were pressed")


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
        check_control_keys_do_not_scroll(session)
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
