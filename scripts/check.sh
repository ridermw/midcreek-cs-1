#!/usr/bin/env bash
# The largest clean gate this repository can run locally.
#
# Usage: scripts/check.sh
#
# It runs, in order:
#
#   1. rustfmt in check mode;
#   2. Clippy over every target and feature with warnings denied;
#   3. the autonomous asset generator in --check mode;
#   4. sitegen validation of the published progress data;
#   5. every pure, integration, and site test;
#   6. the rendered verification contract, which launches the real game and
#      analyses its fourteen real frames;
#   7. the packaged WebAssembly build and its headless browser gate, when a
#      browser and the pinned wasm-bindgen toolchain are available;
#   8. the release build.
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

# The rendered contract launches the compiled game, so it needs the assets to
# resolve from the repository rather than from the test binary's directory.
export BEVY_ASSET_ROOT="$repository"

# ---------------------------------------------------------------------------
# Renderer availability
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
  Darwin) ;;
  *)
    echo "unsupported platform $(uname -s) for the rendered contract" >&2
    exit 1
    ;;
esac

# ---------------------------------------------------------------------------
# Pure gates
# ---------------------------------------------------------------------------

step "rustfmt"
cargo fmt --all --check

step "clippy"
cargo clippy --all-targets --all-features -- -D warnings

step "generated assets are current"
cargo run --quiet --bin assetgen -- --check

step "published progress data is consistent"
cargo run --quiet --bin sitegen -- validate \
  --progress docs/progress.json \
  --plan docs/implementation-plan.md \
  --repository .

step "pure and integration tests"
cargo test --lib --bins
cargo test --test asset_contract
cargo test --test app_contract
cargo test --test sitegen_contract
cargo test --test pages_assembly_contract

# ---------------------------------------------------------------------------
# Rendered gate
# ---------------------------------------------------------------------------

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

if command -v wasm-bindgen >/dev/null 2>&1 \
  && rustup target list --installed | grep -qx wasm32-unknown-unknown \
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
