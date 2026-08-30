#!/usr/bin/env bash
# Proves the checked-out history reaches every commit the site publishes.
#
# Usage: scripts/ensure-history.sh [progress-document]
#
# The published timeline resolves the commit each finished task names, and the
# generator resolves it against `git rev-list --all`. A full clone satisfies
# that by fetching everything, which grows without bound and drags every branch
# — including the generated `pages-live` branch and its 34 MB WebAssembly
# modules — onto the runner on every push.
#
# A finite checkout depth is bounded but not automatically correct, so this
# script closes the gap: it checks exactly what the generator checks, deepens a
# bounded number of times when the depth turns out to be short, and fails
# loudly rather than letting a run publish a timeline with an unresolved
# commit in it.
set -euo pipefail

repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repository"

progress="${1:-docs/progress.json}"
depth="${HISTORY_DEPTH:-250}"
rounds="${HISTORY_DEEPEN_ROUNDS:-4}"

if [[ ! -f "$progress" ]]; then
  echo "no published progress document at $progress" >&2
  exit 1
fi

# Every full commit SHA the document names, wherever it names it. `HEAD` and
# any other symbolic reference is resolved by the generator itself and needs no
# history beyond the checkout.
referenced_commits() {
  python3 - "$progress" <<'PY'
import json
import re
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    document = json.load(handle)

seen = set()


def walk(node):
    if isinstance(node, dict):
        for value in node.values():
            walk(value)
    elif isinstance(node, list):
        for value in node:
            walk(value)
    elif isinstance(node, str) and re.fullmatch(r"[0-9a-f]{40}", node):
        seen.add(node)


walk(document)
for sha in sorted(seen):
    print(sha)
PY
}

# The generator accepts a commit only when it is in `git rev-list --all`, so
# that is what is checked here. Object presence alone is not the same test: a
# commit can be present and still be outside the reachable set the generator
# resolves against.
unreachable_commits() {
  local known sha
  known="$(git rev-list --all)"
  while IFS= read -r sha; do
    [[ -n "$sha" ]] || continue
    grep -qxF "$sha" <<<"$known" || printf '%s\n' "$sha"
  done < <(referenced_commits)
}

round=0
while true; do
  absent="$(unreachable_commits)"
  if [[ -z "$absent" ]]; then
    break
  fi
  if (( round >= rounds )); then
    echo "the checked-out history does not reach these published commits:" >&2
    while IFS= read -r sha; do
      echo "  $sha" >&2
    done <<<"$absent"
    echo "deepened $rounds times from $depth; raise HISTORY_DEPTH past" \
      "$(( depth * (rounds + 1) )) commits" >&2
    exit 1
  fi
  round=$(( round + 1 ))
  echo "deepening the checked-out history by $depth commits (round $round/$rounds)"
  git fetch --deepen="$depth" --filter=blob:none origin \
    || git fetch --deepen="$depth" origin
done

echo "every published commit is reachable in $(git rev-list --count HEAD) checked-out commits"
