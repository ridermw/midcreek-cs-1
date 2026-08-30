#!/usr/bin/env python3
"""Render an honest status-only site when Publish cannot verify git history."""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import shutil
import sys
import tempfile
from pathlib import Path

STATUSES = {
    "passed": ("Passed", "passed"),
    "failed": ("Failed", "failed"),
    "skipped_dependency": ("Not run", "skipped"),
}
WORKFLOW_FIELDS = {"source_commit", "run_url", "native", "web", "gates"}
GATE_FIELDS = {
    "name",
    "status",
    "passed",
    "failed",
    "duration_ms",
    "artifact_url",
}


class FallbackError(Exception):
    """The degraded publication could not be produced safely."""


def parse_arguments(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workflow", required=True, type=Path)
    parser.add_argument("--previous", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args(argv)


def require_fields(value: object, expected: set[str], label: str) -> dict[str, object]:
    if not isinstance(value, dict):
        raise FallbackError(f"{label} must be a JSON object")
    actual = set(value)
    if actual != expected:
        raise FallbackError(
            f"{label} fields differ: missing {sorted(expected - actual)}, "
            f"unexpected {sorted(actual - expected)}"
        )
    return value


def require_status(value: object, label: str) -> str:
    if not isinstance(value, str) or value not in STATUSES:
        raise FallbackError(f"{label} has unsupported status {value!r}")
    return value


def read_workflow(path: Path) -> dict[str, object]:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as failure:
        raise FallbackError(f"{path}: {failure}") from failure
    workflow = require_fields(raw, WORKFLOW_FIELDS, "workflow")

    source_commit = workflow["source_commit"]
    if not isinstance(source_commit, str) or re.fullmatch(
        r"[0-9a-fA-F]{40}", source_commit
    ) is None:
        raise FallbackError("workflow source_commit must be a full hexadecimal SHA")

    run_url = workflow["run_url"]
    if not isinstance(run_url, str) or not run_url.startswith("https://github.com/"):
        raise FallbackError("workflow run_url must be an https://github.com/ URL")

    require_status(workflow["native"], "workflow.native")
    require_status(workflow["web"], "workflow.web")

    gates = workflow["gates"]
    if not isinstance(gates, list):
        raise FallbackError("workflow.gates must be a JSON array")
    for index, raw_gate in enumerate(gates):
        gate = require_fields(raw_gate, GATE_FIELDS, f"workflow.gates[{index}]")
        name = gate["name"]
        if (
            not isinstance(name, str)
            or not name.strip()
            or len(name) > 96
            or any(ord(character) < 32 for character in name)
        ):
            raise FallbackError(f"workflow.gates[{index}].name is not publishable")
        require_status(gate["status"], f"workflow.gates[{index}]")
        for field in ("passed", "failed", "duration_ms"):
            value = gate[field]
            if type(value) is not int or value < 0:
                raise FallbackError(
                    f"workflow.gates[{index}].{field} must be a non-negative integer"
                )
        artifact_url = gate["artifact_url"]
        if artifact_url is not None and (
            not isinstance(artifact_url, str)
            or not artifact_url.startswith("https://github.com/")
        ):
            raise FallbackError(
                f"workflow.gates[{index}].artifact_url is not publishable"
            )
    return workflow


def reject_symlinks(path: Path) -> None:
    if path.is_symlink():
        raise FallbackError(f"{path} is a symbolic link")
    if not path.is_dir():
        return
    for directory, names, files in os.walk(path, followlinks=False):
        root = Path(directory)
        for name in [*names, *files]:
            candidate = root / name
            if candidate.is_symlink():
                raise FallbackError(f"{candidate} is a symbolic link")


def retained_game(previous: Path | None) -> tuple[Path | None, Path | None]:
    if previous is None:
        return None, None
    if previous.is_symlink() or not previous.is_dir():
        raise FallbackError(f"{previous} is not a directory")

    package = previous / "play"
    manifest = previous / "last-green.json"
    reject_symlinks(package)
    reject_symlinks(manifest)
    package_exists = package.exists() or package.is_symlink()
    manifest_exists = manifest.exists() or manifest.is_symlink()
    if not package_exists and not manifest_exists:
        return None, None
    if not package.is_dir() or not manifest.is_file():
        raise FallbackError(f"{previous} has an incomplete last-green playable domain")
    return package, manifest


def render_status(workflow: dict[str, object], has_game: bool) -> str:
    source_commit = str(workflow["source_commit"])
    run_url = html.escape(str(workflow["run_url"]), quote=True)
    native_label, native_class = STATUSES[str(workflow["native"])]
    web_label, web_class = STATUSES[str(workflow["web"])]
    rows = []
    for raw_gate in workflow["gates"]:
        gate = raw_gate
        label, css_class = STATUSES[str(gate["status"])]
        rows.append(
            "<tr>"
            f"<th scope=\"row\">{html.escape(str(gate['name']))}</th>"
            f"<td class=\"{css_class}\">{label}</td>"
            f"<td>{gate['duration_ms']} ms</td>"
            "</tr>"
        )
    rows.append(
        '<tr><th scope="row">Publish history bound</th>'
        '<td class="failed">Failed</td><td>0 ms</td></tr>'
    )
    game = (
        '<p><a href="play/index.html">Open the last verified playable build</a>. '
        "It was retained unchanged from the previous publication.</p>"
        if has_game
        else "<p>No previous verified game was available; this is a status-only publication.</p>"
    )
    return f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="description" content="Current publication failure status for the Cell Shift Data Center POC.">
  <title>Cell Shift Data Center - Publication status</title>
  <style>
    :root {{ color-scheme: dark; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }}
    body {{ margin: 0; background: #101820; color: #f4f7f8; }}
    main {{ width: min(920px, calc(100% - 2rem)); margin: 0 auto; padding: 3rem 0; }}
    .panel {{ border: 1px solid #52616b; border-radius: 12px; background: #17232d; padding: 1.25rem; }}
    .status {{ display: flex; flex-wrap: wrap; gap: 1rem; margin: 1.5rem 0; }}
    .status span {{ display: block; color: #b7c5cd; font-size: .8rem; text-transform: uppercase; }}
    .status strong {{ display: block; margin-top: .25rem; }}
    .passed {{ color: #7ee787; }}
    .failed {{ color: #ff7b72; }}
    .skipped {{ color: #d2a8ff; }}
    table {{ width: 100%; border-collapse: collapse; margin-top: 1rem; }}
    th, td {{ padding: .65rem; border-top: 1px solid #384957; text-align: left; }}
    a {{ color: #79c0ff; }}
    code {{ overflow-wrap: anywhere; }}
  </style>
</head>
<body>
  <main>
    <section class="panel" aria-labelledby="status-title">
      <p>Current source <code>{html.escape(source_commit[:8])}</code></p>
      <h1 id="status-title">Publication history unavailable</h1>
      <p>The bounded history repair did not complete, so this checkout could not
      prove that every commit referenced by <code>docs/progress.json</code> is
      available. This page therefore withholds current progress and commit links
      instead of treating unverified references as valid.</p>
      <div class="status" aria-label="Current workflow status">
        <div><span>Native</span><strong class="{native_class}">{native_label}</strong></div>
        <div><span>Web</span><strong class="{web_class}">{web_label}</strong></div>
        <div><span>Publish</span><strong class="failed">Failed</strong></div>
      </div>
      {game}
      <p><a href="{run_url}">Open the current workflow run</a></p>
    </section>
    <section class="panel" aria-labelledby="gates-title">
      <h2 id="gates-title">Recorded gates</h2>
      <table>
        <thead><tr><th scope="col">Gate</th><th scope="col">Status</th><th scope="col">Duration</th></tr></thead>
        <tbody>{''.join(rows)}</tbody>
      </table>
    </section>
  </main>
</body>
</html>
"""


def write_site(
    workflow_path: Path, previous: Path | None, output: Path
) -> None:
    workflow = read_workflow(workflow_path)
    package, manifest = retained_game(previous)
    if output.exists():
        raise FallbackError(f"{output} already exists")
    output.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(
        tempfile.mkdtemp(prefix=f".{output.name}-history-failure-", dir=output.parent)
    )
    try:
        upstream_passed = workflow["native"] == "passed" and workflow["web"] == "passed"
        if upstream_passed and package is not None and manifest is not None:
            shutil.copytree(package, staging / "play", symlinks=True)
            shutil.copy2(manifest, staging / "last-green.json")
        (staging / "index.html").write_text(
            render_status(workflow, package is not None),
            encoding="utf-8",
        )
        staging.replace(output)
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def main(argv: list[str] | None = None) -> int:
    arguments = parse_arguments(argv)
    try:
        write_site(arguments.workflow, arguments.previous, arguments.output)
    except (FallbackError, OSError) as failure:
        print(f"history failure site could not be written: {failure}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
