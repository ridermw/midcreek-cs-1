#!/usr/bin/env bash
# Lints this repository's GitHub Actions workflows with a pinned actionlint.
#
# Usage: scripts/actionlint.sh [workflow file...]
#
# With no arguments every file under .github/workflows is checked.
#
# A workflow is a program this repository cannot run locally: the only place it
# executes is a push to main, and the only way a mistake in it announces itself
# is a run that never starts. actionlint reads the same context-availability
# rules GitHub enforces, so an expression in a scope that does not have it —
# `runner` in a job-level `env:`, say — is caught here instead of by a red run.
#
# The version is pinned, because a linter that silently changes its rules is a
# gate that silently changes its verdict. The pinned binary is cached under the
# repository's own target directory and its reported version is checked before
# it is trusted, so a stale or substituted binary can never pass as the pinned
# one.
#
# The external shellcheck and pyflakes integrations are switched off on
# purpose. They are present on GitHub's runners and absent on most developer
# machines, so leaving them on would make this gate reach a different verdict
# in CI than the one a contributor just saw pass locally.
set -euo pipefail

# The only actionlint this gate trusts.
ACTIONLINT_VERSION="1.7.7"

repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repository"

# The version becomes a directory name, so it is checked as a version and
# nothing else. A value carrying a separator or a parent reference would place
# the cache — and the binary this gate then executes — outside the repository.
if [[ ! "$ACTIONLINT_VERSION" =~ ^[0-9]+(\.[0-9]+)*$ ]]; then
  echo "refusing '$ACTIONLINT_VERSION' as a pinned actionlint version" >&2
  exit 2
fi

tools="$repository/target/tools"
cache="$tools/actionlint/$ACTIONLINT_VERSION"
binary_name="actionlint"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) binary_name="actionlint.exe" ;;
esac
binary="$cache/$binary_name"

# The cache lives inside the repository's own build output, and every
# directory on the way down to it has to really be one. A symbolic link, or a
# plain file, anywhere along `target/tools/actionlint/<version>` would place
# the binary this gate then executes outside the tree — or make the install
# fail with a message about the wrong thing entirely — so each component that
# already exists is checked rather than only the one that usually does.
for candidate in "$repository/target" "$tools" "$tools/actionlint" "$cache"; do
  [[ -e "$candidate" ]] || continue
  if [[ -L "$candidate" ]]; then
    echo "refusing a tools cache outside the repository: $candidate is a symbolic link" >&2
    exit 2
  fi
  if [[ ! -d "$candidate" ]]; then
    echo "refusing a tools cache outside the repository: $candidate is not a directory" >&2
    exit 2
  fi
  resolved="$(cd "$candidate" && pwd -P)"
  if [[ "$resolved" != "$candidate" ]]; then
    echo "refusing a tools cache outside the repository: $resolved" >&2
    exit 2
  fi
done

# The binary is executed, so it is never followed through a link either.
if [[ -L "$binary" ]]; then
  echo "refusing a tools cache outside the repository: $binary is a symbolic link" >&2
  exit 2
fi

# The binary is trusted only when it says it is the pinned version. That check
# runs on every invocation, not just after an install, so a cached binary left
# behind by an earlier pin is replaced instead of being used.
installed_version() {
  [[ -x "$binary" ]] || return 1
  "$binary" -version 2>/dev/null \
    | head -n 1 \
    | tr -d '[:space:]' \
    | sed 's/^v//'
}

if [[ "$(installed_version || true)" != "$ACTIONLINT_VERSION" ]]; then
  echo "installing actionlint v$ACTIONLINT_VERSION into $cache"
  rm -rf "$cache"
  mkdir -p "$cache"
  if command -v go >/dev/null 2>&1; then
    GOBIN="$cache" go install \
      "github.com/rhysd/actionlint/cmd/actionlint@v$ACTIONLINT_VERSION"
  else
    platform="$(uname -s)"
    machine="$(uname -m)"
    case "$platform:$machine" in
      MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64)
        archive="actionlint_${ACTIONLINT_VERSION}_windows_amd64.zip"
        checksum="7f12f1801bca3d480d67aaf7774f4c2a6359a3ca8eebe382c95c10c9704aa731"
        ;;
      MINGW*:aarch64|MSYS*:aarch64|CYGWIN*:aarch64)
        archive="actionlint_${ACTIONLINT_VERSION}_windows_arm64.zip"
        checksum="76e9514cfac18e5677aa04f3a89873c981f16a2f2353bb97372a86cd09b1f5a8"
        ;;
      MINGW*:i686|MSYS*:i686|CYGWIN*:i686)
        archive="actionlint_${ACTIONLINT_VERSION}_windows_386.zip"
        checksum="66de2b65bcee17de1866287a513cadf47d812229e976e99024e9757645258adb"
        ;;
      *)
        echo "actionlint v$ACTIONLINT_VERSION is not cached and Go is not installed" >&2
        echo "install Go, or place the pinned binary at $binary" >&2
        exit 2
        ;;
    esac
    url="https://github.com/rhysd/actionlint/releases/download/v$ACTIONLINT_VERSION/$archive"
    curl --fail --location --silent --show-error "$url" --output "$cache/$archive"
    printf '%s  %s\n' "$checksum" "$cache/$archive" | sha256sum --check --status
    unzip -q "$cache/$archive" -d "$cache"
    rm "$cache/$archive"
  fi
fi

actual="$(installed_version || true)"
if [[ "$actual" != "$ACTIONLINT_VERSION" ]]; then
  echo "refusing actionlint ${actual:-<unreadable>}: this gate is pinned to v$ACTIONLINT_VERSION" >&2
  exit 2
fi

targets=("$@")
if [[ ${#targets[@]} -eq 0 ]]; then
  while IFS= read -r workflow; do
    targets+=("$workflow")
  done < <(find .github/workflows -maxdepth 1 -type f \
    \( -name '*.yml' -o -name '*.yaml' \) | sort)
fi

if [[ ${#targets[@]} -eq 0 ]]; then
  echo "no workflow files to lint" >&2
  exit 2
fi

"$binary" \
  -no-color \
  -shellcheck= \
  -pyflakes= \
  -ignore 'unknown permission scope "copilot-requests"' \
  "${targets[@]}"
