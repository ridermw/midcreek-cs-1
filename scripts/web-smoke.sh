#!/usr/bin/env bash
# Headless browser gate for the packaged WASM game.
#
# Usage: scripts/web-smoke.sh <packaged-directory> [diagnostics-directory]
#
# Serves the package from a loopback HTTP server on an available port, launches
# a headless Chrome/Chromium with a private profile, and hands the DevTools
# endpoint to the repository-owned Python gate. Diagnostics are retained on
# failure and the exact server and browser PIDs are always terminated.
#
# The diagnostics directory is destructive, so it is canonicalized, contained
# to the repository build root or the workflow runner temporary directory, and
# only ever cleaned when a previous run of this script created it.
set -euo pipefail

repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
package="${1:-}"
if [[ -z "$package" ]]; then
  echo "usage: scripts/web-smoke.sh <packaged-directory> [diagnostics-directory]" >&2
  exit 2
fi

real_path() {
  python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$1"
}

# 1. Contain the diagnostics directory before anything can be removed.
# The default path below the build root belongs to this script; any path a
# caller names must carry the marker a previous run of this script left.
if [[ -n "${2:-}" ]]; then
  diagnostics_input="$2"
  caller_supplied=1
else
  diagnostics_input="$repository/target/web-smoke"
  caller_supplied=0
fi
sentinel=".midcreek-web-smoke"
case "$diagnostics_input" in
  "")
    echo "refusing an empty path as a diagnostics directory" >&2
    exit 1
    ;;
esac
if [[ -L "$diagnostics_input" ]]; then
  echo "refusing the symbolic link $diagnostics_input as a diagnostics directory" >&2
  exit 1
fi

diagnostics="$(real_path "$diagnostics_input")"
trusted_roots=("$(real_path "$repository/target")")
if [[ -n "${RUNNER_TEMP:-}" && -d "$RUNNER_TEMP" ]]; then
  trusted_roots+=("$(real_path "$RUNNER_TEMP")")
fi
contained=0
for root in "${trusted_roots[@]}"; do
  if [[ "$diagnostics" == "$root"/* ]]; then
    contained=1
  fi
done
if (( contained == 0 )); then
  echo "refusing to write diagnostics outside ${trusted_roots[*]}: $diagnostics_input" >&2
  exit 1
fi

if [[ -e "$diagnostics" ]]; then
  if [[ ! -d "$diagnostics" ]]; then
    echo "refusing to clean $diagnostics: it is not a directory" >&2
    exit 1
  fi
  if (( caller_supplied == 1 )) && [[ ! -f "$diagnostics/$sentinel" ]]; then
    echo "refusing to clean $diagnostics: this script did not create it" >&2
    exit 1
  fi
  rm -rf "$diagnostics"
fi
mkdir -p "$diagnostics"
: >"$diagnostics/$sentinel"

# 2. Only then look at the package itself.
package="$(cd "$package" && pwd -P)"
if [[ ! -f "$package/index.html" || ! -f "$package/game_bg.wasm" ]]; then
  echo "$package is not a packaged web build" >&2
  exit 1
fi

# Discover a browser on macOS or Linux without downloading one.
find_browser() {
  local candidate
  for candidate in \
    "${CHROME:-}" \
    "$(command -v google-chrome-stable || true)" \
    "$(command -v google-chrome || true)" \
    "$(command -v chromium-browser || true)" \
    "$(command -v chromium || true)" \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium" \
    "/usr/bin/google-chrome" \
    "/usr/bin/chromium-browser" \
    "/usr/bin/chromium"; do
    if [[ -n "$candidate" && -x "$candidate" ]]; then
      printf '%s' "$candidate"
      return 0
    fi
  done
  return 1
}

browser="$(find_browser)" || {
  echo "no Chrome or Chromium executable was found; set CHROME to one" >&2
  exit 1
}

free_port() {
  python3 - <<'PY'
import socket
with socket.socket() as probe:
    probe.bind(("127.0.0.1", 0))
    print(probe.getsockname()[1])
PY
}

http_port="$(free_port)"
cdp_port="$(free_port)"
profile="$(mktemp -d "${TMPDIR:-/tmp}/midcreek-chrome-XXXXXX")"
serve_root="$(mktemp -d "${TMPDIR:-/tmp}/midcreek-serve-XXXXXX")"

server_pid=""
browser_pid=""

cleanup() {
  local status=$?
  if [[ -n "$browser_pid" ]] && kill -0 "$browser_pid" 2>/dev/null; then
    kill "$browser_pid" 2>/dev/null || true
    wait "$browser_pid" 2>/dev/null || true
  fi
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$profile" "$serve_root"
  if [[ "$status" -eq 0 ]]; then
    rm -f "$diagnostics/server.log" "$diagnostics/browser.log"
  else
    echo "browser gate diagnostics retained in $diagnostics" >&2
  fi
  return "$status"
}
trap cleanup EXIT INT TERM

# The published site is a project page, so the game must work below a prefix.
prefix="midcreek-cs-1"
mkdir -p "$serve_root/$prefix/play"
cp -R "$package/." "$serve_root/$prefix/play/"

python3 -m http.server "$http_port" --bind 127.0.0.1 --directory "$serve_root" \
  >"$diagnostics/server.log" 2>&1 &
server_pid=$!

base_url="http://127.0.0.1:$http_port/$prefix/play"
for _ in $(seq 1 100); do
  if curl -fsS -o /dev/null "$base_url/index.html"; then
    break
  fi
  sleep 0.1
done
if ! curl -fsS -o /dev/null "$base_url/index.html"; then
  echo "the loopback server never served $base_url/index.html" >&2
  exit 1
fi

"$browser" \
  --headless=new \
  --remote-debugging-port="$cdp_port" \
  --remote-allow-origins=* \
  --user-data-dir="$profile" \
  --no-first-run \
  --no-default-browser-check \
  --disable-extensions \
  --disable-background-networking \
  --disable-sync \
  --hide-scrollbars=false \
  --window-size=1280,1024 \
  --use-gl=angle \
  --use-angle=swiftshader \
  --enable-unsafe-swiftshader \
  --enable-features=Vulkan \
  --disable-dev-shm-usage \
  --no-sandbox \
  about:blank \
  >"$diagnostics/browser.log" 2>&1 &
browser_pid=$!

python3 "$repository/scripts/browser_gate.py" \
  --base-url "$base_url" \
  --cdp-port "$cdp_port" \
  --package "$package" \
  --design-source "$repository/src/design.rs" \
  --diagnostics "$diagnostics"
