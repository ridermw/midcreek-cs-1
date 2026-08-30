#!/usr/bin/env bash
# Packages the production Bevy game for the browser.
#
# Usage: scripts/build-web.sh <output-directory>
#
# The output is a self-contained directory that works below any path prefix,
# including the project prefix a GitHub Pages project site adds.
set -euo pipefail

repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="${1:-}"
if [[ -z "$output" ]]; then
  echo "usage: scripts/build-web.sh <output-directory>" >&2
  exit 2
fi
mkdir -p "$(dirname "$output")"
output="$(cd "$(dirname "$output")" && pwd)/$(basename "$output")"

crate_name="midcreek-cs-1"
target="wasm32-unknown-unknown"

# 1. Read the locked wasm-bindgen crate version and require an exact CLI match.
locked_version="$(
  awk '
    /^name = "wasm-bindgen"$/ { found = 1; next }
    found && /^version = / { gsub(/["]/, "", $3); print $3; exit }
  ' "$repository/Cargo.lock"
)"
if [[ -z "$locked_version" ]]; then
  echo "could not read the locked wasm-bindgen version from Cargo.lock" >&2
  exit 1
fi

if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "wasm-bindgen $locked_version is required; install it with:" >&2
  echo "  cargo install wasm-bindgen-cli --version $locked_version --locked" >&2
  exit 1
fi

cli_version="$(wasm-bindgen --version | awk '{ print $2 }')"
if [[ "$cli_version" != "$locked_version" ]]; then
  echo "wasm-bindgen CLI $cli_version does not match the locked crate $locked_version" >&2
  exit 1
fi

# 2. Build the production game for the browser.
(cd "$repository" && cargo build --release --target "$target" --bin "$crate_name")

wasm_input="$repository/target/$target/release/$crate_name.wasm"
if [[ ! -f "$wasm_input" ]]; then
  echo "expected a release WASM artifact at $wasm_input" >&2
  exit 1
fi

# 3. Generate the browser bindings.
rm -rf "$output"
mkdir -p "$output"
wasm-bindgen \
  --target web \
  --no-typescript \
  --out-dir "$output" \
  --out-name game \
  --remove-name-section \
  --remove-producers-section \
  "$wasm_input"

# 4. Copy the browser shell and every generated asset the game loads.
install -m 644 "$repository/site/templates/play.html" "$output/index.html"
install -m 644 "$repository/site/static/play.js" "$output/play.js"
install -m 644 "$repository/site/static/play.css" "$output/play.css"

mkdir -p "$output/assets/generated"
for asset in "$repository/assets/generated"/*.glb; do
  [[ -f "$asset" ]] || continue
  install -m 644 "$asset" "$output/assets/generated/$(basename "$asset")"
done

# 5. Refuse to publish an incomplete or host-specific package.
for required in index.html play.js play.css game.js game_bg.wasm; do
  if [[ ! -f "$output/$required" ]]; then
    echo "packaged build is missing $required" >&2
    exit 1
  fi
done

for expected in rack technician cooling-unit infrastructure utility-props; do
  if [[ ! -f "$output/assets/generated/$expected.glb" ]]; then
    echo "packaged build is missing assets/generated/$expected.glb" >&2
    exit 1
  fi
done

for text in index.html play.js play.css game.js; do
  if grep -nE 'file://|/Users/|/home/runner|/private/var/|(^|[^[:alnum:]])[A-Za-z]:[\\/][A-Za-z0-9_.-]' "$output/$text" >&2; then
    echo "packaged $text contains an absolute host path" >&2
    exit 1
  fi
  if grep -nE '(src|href)="/|from[[:space:]]+"/|import\("/' "$output/$text" >&2; then
    echo "packaged $text contains a root-absolute URL" >&2
    exit 1
  fi
done

if grep -qF "$repository" "$output/game.js"; then
  echo "packaged game.js leaks the build host path" >&2
  exit 1
fi

echo "packaged $crate_name for $target into $output (wasm-bindgen $locked_version)"
