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

python=""
for candidate in python3 python; do
  if command -v "$candidate" >/dev/null 2>&1 \
    && "$candidate" -c 'import json, re, sys; raise SystemExit(sys.version_info < (3, 8))' \
      >/dev/null 2>&1; then
    python="$candidate"
    break
  fi
done
if [[ -z "$python" ]]; then
  echo "Python is required to read the published progress document" >&2
  exit 1
fi

# Exactly the fields the generator resolves as commits, and exactly the values
# it accepts in them. `resolve_commit_ref` takes `HEAD`, or forty hexadecimal
# digits naming a commit it can reach. Any other forty-hex string in the
# document — a reference image digest, a semantic visual hash — is not a commit
# and is deliberately not looked for here, because deepening history for it
# would be deepening for nothing.
referenced_commits() {
  "$python" - "$progress" <<'PY'
import json
import re
import sys

COMMIT_FIELDS = ("completed_commit", "resolved_commit")
SYMBOLIC = {"HEAD"}
FULL_SHA = re.compile(r"[0-9a-fA-F]{40}")

with open(sys.argv[1], encoding="utf-8-sig") as handle:
    document = json.load(handle)

seen = set()


def walk(node):
    if isinstance(node, dict):
        for key, value in node.items():
            if key in COMMIT_FIELDS and isinstance(value, str):
                candidate = value.strip()
                # A symbolic reference resolves against the checkout itself and
                # needs no history behind it. Anything that is neither symbolic
                # nor a full SHA is a value the generator refuses on its own;
                # deepening cannot make it valid, so it is not reported here.
                if candidate not in SYMBOLIC and FULL_SHA.fullmatch(candidate):
                    seen.add(candidate.lower())
            walk(value)
    elif isinstance(node, list):
        for value in node:
            walk(value)


walk(document)
for sha in sorted(seen):
    print(sha)
PY
}

# The generator accepts a commit only when it is in `git rev-list --all`, so
# that is what is checked here. Object presence alone is not the same test: a
# commit can be present and still be outside the reachable set the generator
# resolves against. `rev-list` prints lower case, and the references above are
# already folded to it.
unreachable_commits() {
  local known referenced sha
  known="$(git rev-list --all | tr -d '\r')"
  if ! referenced="$(referenced_commits)"; then
    echo "failed to read published commits from $progress" >&2
    return 1
  fi
  while IFS= read -r sha; do
    [[ -n "$sha" ]] || continue
    grep -qxF "$sha" <<<"$known" || printf '%s\n' "$sha"
  done <<<"${referenced//$'\r'/}"
}

# Deepening pulls history for the branch this run is publishing and nothing
# else. A bare `git fetch --deepen origin` would deepen every remote branch —
# including the generated `pages-live` branch, whose every revision carries a
# 34 MB WebAssembly module — and dropping the filter would fetch each of those
# blobs in full. The refspec and the filter are both part of the fix, not
# decoration.
source_branch() {
  local branch="${GITHUB_REF_NAME:-}"
  if [[ -z "$branch" || "$branch" == "HEAD" ]]; then
    branch="$(git rev-parse --abbrev-ref HEAD | tr -d '\r')"
  fi
  printf '%s' "$branch"
}

deepen_source_branch() {
  local branch refspec
  branch="$(source_branch)"
  if [[ -z "$branch" || "$branch" == "HEAD" ]]; then
    echo "refusing to deepen a detached HEAD: no source branch to fetch" >&2
    return 1
  fi
  refspec="+refs/heads/${branch}:refs/remotes/origin/${branch}"
  git fetch --deepen="$depth" --filter=blob:none --no-tags origin "$refspec"
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
  echo "deepening $(source_branch) by $depth commits (round $round/$rounds)"
  deepen_source_branch
done

echo "every published commit is reachable in $(git rev-list --count HEAD) checked-out commits"
