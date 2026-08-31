#!/usr/bin/env bash
# The largest clean gate this repository can run locally.
#
# Usage: scripts/check.sh
#
# It runs, in order:
#
#   1. actionlint over every workflow, pinned to one version;
#   2. the browser-gate unit suite against loopback and fake sockets;
#   3. rustfmt in check mode;
#   4. Clippy over every target and feature with warnings denied;
#   5. the autonomous asset generator in --check mode;
#   6. sitegen validation of the published progress data;
#   7. the history suite, including the degraded publication fallback composed
#      with the real assembler;
#   8. every pure, integration, and site test;
#   9. the rendered verification contract, which launches the real game and
#      analyses its fourteen real frames;
#  10. the packaged WebAssembly build and its headless browser gate, when a
#      browser and the pinned wasm-bindgen toolchain are available;
#  11. the release build.
#
# The rendered contract needs a real renderer. On a headless Linux machine that
# means Xvfb: a missing display or a missing renderer is a hard failure here,
# never a skip, because a skipped render gate is indistinguishable from a
# passing one.
set -euo pipefail

repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repository"

step() {
  printf '\n=== %s ===\n' "$1"
}

python=""
for candidate in python3 python; do
  if command -v "$candidate" >/dev/null 2>&1 \
    && "$candidate" -c 'import sys; raise SystemExit(sys.version_info < (3, 8))' \
      >/dev/null 2>&1; then
    python="$candidate"
    break
  fi
done
if [[ -z "$python" ]]; then
  echo "Python 3.8 or newer is required by the local clean gate" >&2
  exit 1
fi

# The rendered contract launches the compiled game, so it needs the assets to
# resolve from the repository rather than from the test binary's directory.
export BEVY_ASSET_ROOT="$repository"

# ---------------------------------------------------------------------------
# Pure gates
# ---------------------------------------------------------------------------

# The workflow is the one program here that can only run on a push to main, so
# it is linted before anything expensive: a run that cannot start proves
# nothing about the gates it was supposed to carry.
step "workflow lint"
./scripts/actionlint.sh

step "browser gate unit tests"
"$python" scripts/browser_gate_test.py

step "rustfmt"
cargo fmt --all --check

step "clippy"
cargo clippy --all-targets --all-features -- -D warnings

step "generated assets are current"
cargo run --quiet --bin assetgen -- --check

# The published timeline resolves the commit every finished task names, so the
# checkout has to reach those commits before anything validates them. On a
# shallow clone this repairs the history first; under `set -e` a validation run
# against a short clone would end the gate before it could.
step "published history bound"
./scripts/ensure-history.sh

step "published progress data is consistent"
cargo run --quiet --bin sitegen -- validate \
  --progress docs/progress.json \
  --plan docs/implementation-plan.md \
  --repository .

# The history suite composes the degraded publication fallback with the real
# assembler, so it runs once the step above has built `sitegen` for it.
step "history bound tests"
"$python" scripts/ensure_history_test.py

step "pure and integration tests"
cargo test --lib --bins
cargo test --test asset_contract
cargo test --test app_contract
cargo test --test sitegen_contract
cargo test --test pages_assembly_contract

# ---------------------------------------------------------------------------
# Rendered gate
# ---------------------------------------------------------------------------

# The rendered contract launches real game windows. Its tests are serialized so
# a heavy analysis thread can never starve a running child into its watchdog.
render_command=(cargo test --test render_contract -- --test-threads=1 --nocapture)
case "$(uname -s)" in
  Linux)
    if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
      if ! command -v xvfb-run >/dev/null 2>&1; then
        echo "the rendered contract needs a display; install Xvfb (xvfb-run) or export DISPLAY" >&2
        exit 1
      fi
      render_command=(xvfb-run -a "${render_command[@]}")
    fi
    ;;
  Darwin|MINGW*|MSYS*|CYGWIN*) ;;
  *)
    echo "unsupported platform $(uname -s) for the rendered contract" >&2
    exit 1
    ;;
esac

step "rendered verification contract"
"${render_command[@]}"

# ---------------------------------------------------------------------------
# Browser gate
# ---------------------------------------------------------------------------

web_package="$repository/target/check-web"

browser_available() {
  local candidate
  for candidate in \
    "${CHROME:-}" \
    "$(command -v google-chrome-stable || true)" \
    "$(command -v google-chrome || true)" \
    "$(command -v chromium-browser || true)" \
    "$(command -v chromium || true)" \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium"; do
    if [[ -n "$candidate" && -x "$candidate" ]]; then
      return 0
    fi
  done
  return 1
}

# `rustup target list --installed | grep -q ...` is a false skip waiting to
# happen. `grep -q` exits at the first match, `rustup` is then killed by
# SIGPIPE, and `pipefail` reports the whole pipeline as failed — so an
# installed target reads as a missing one and the browser gate below is skipped
# while the run still looks green. That is the one failure this script must
# never have, so the list is captured before anything reads it and nothing is
# ever asked to write into a closed pipe.
wasm_target_installed() {
  local installed
  installed="$(rustup target list --installed)" || return 1
  grep -qx wasm32-unknown-unknown <<<"$installed"
}

if command -v wasm-bindgen >/dev/null 2>&1 \
  && wasm_target_installed \
  && browser_available; then
  step "playable web build and browser gate"
  ./scripts/build-web.sh "$web_package"
  ./scripts/web-smoke.sh "$web_package"
else
  step "playable web build and browser gate"
  echo "skipping: this machine has no pinned wasm-bindgen, wasm32 target, or browser."
  echo "the Pages workflow runs this gate on every push; it is not optional there."
fi

# ---------------------------------------------------------------------------
# Release build
# ---------------------------------------------------------------------------

step "release build"
cargo build --release

printf '\nall gates passed\n'
