#!/usr/bin/env bash
# Runs one named gate, measures it, and records what it did.
#
# Usage: scripts/run-gate.sh <results-file> <gate name> -- <command> [args...]
#
# The gate's own output goes straight to the caller's log, so a failure is
# diagnosed from the workflow run exactly as it always was. One line is
# appended to the results file:
#
#   {"name":"Clippy lints","status":"failed","duration_ms":48213}
#
# Nothing else is recorded: no command line, no captured output, and no value
# from the machine the gate ran on. That file is the only thing that crosses
# the boundary between a runner and the public site.
#
# This script always exits 0. Every named gate in a job has to run and be
# recorded even after an earlier one failed, so the job reaches its verdict
# once, from the complete record, in a final step of its own.
set -uo pipefail

results="${1:-}"
name="${2:-}"
separator="${3:-}"
shift 3 2>/dev/null || true

if [[ -z "$results" || -z "$name" || "$separator" != "--" || $# -eq 0 ]]; then
  echo "usage: scripts/run-gate.sh <results-file> <gate name> -- <command> [args...]" >&2
  exit 2
fi

# The name is published verbatim inside a JSON string and then inside HTML, so
# it is restricted to a plain label rather than escaped on the way out.
if [[ ! "$name" =~ ^[A-Za-z0-9][A-Za-z0-9\ .,()+-]*$ || ${#name} -gt 96 ]]; then
  echo "refusing $name as a gate name: use a short plain label" >&2
  exit 2
fi

mkdir -p "$(dirname "$results")"

# Bash 5 keeps the clock in the shell; older shells borrow Python's, which
# every other script in this repository already requires.
now_ms() {
  if [[ -n "${EPOCHREALTIME:-}" ]]; then
    local value="${EPOCHREALTIME/,/.}"
    printf '%s' "$(( ${value%.*} * 1000 + 10#${value#*.} / 1000 ))"
    return
  fi
  python3 -c 'import time; print(int(time.time() * 1000))'
}

printf '\n=== gate: %s ===\n' "$name"
started="$(now_ms)"
"$@"
code=$?
finished="$(now_ms)"

duration=$(( finished - started ))
if (( duration < 0 )); then
  duration=0
fi

if (( code == 0 )); then
  status="passed"
else
  status="failed"
fi

printf '{"name":"%s","status":"%s","duration_ms":%s}\n' \
  "$name" "$status" "$duration" >>"$results"
printf '=== gate: %s %s in %s ms ===\n' "$name" "$status" "$duration"
exit 0
