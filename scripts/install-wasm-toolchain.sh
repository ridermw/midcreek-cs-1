#!/usr/bin/env bash
# Installs the WebAssembly toolchain the packaged browser game is built with.
#
# Usage: scripts/install-wasm-toolchain.sh
#
# The wasm-bindgen CLI has to match the wasm-bindgen crate exactly, so the
# version is read from Cargo.lock rather than named here. scripts/build-web.sh
# reads the same value and refuses a mismatch, so a drift between the two is a
# hard failure rather than a subtly broken package.
set -euo pipefail

repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repository"

locked="$(
  awk '
    /^name = "wasm-bindgen"$/ { found = 1; next }
    found && /^version = / { gsub(/["]/, "", $3); print $3; exit }
  ' Cargo.lock
)"
if [[ -z "$locked" ]]; then
  echo "could not read the locked wasm-bindgen version from Cargo.lock" >&2
  exit 1
fi

echo "locked wasm-bindgen: $locked"
rustup target add wasm32-unknown-unknown

if command -v wasm-bindgen >/dev/null 2>&1 \
  && [[ "$(wasm-bindgen --version | awk '{ print $2 }')" == "$locked" ]]; then
  echo "wasm-bindgen $locked is already installed"
  exit 0
fi

cargo install wasm-bindgen-cli --version "$locked" --locked
