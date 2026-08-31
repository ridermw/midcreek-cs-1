#!/usr/bin/env python3
"""Render an honest status-only site when Publish cannot verify git history.

This writes the *current* tree and nothing else. It never copies a previous
publication forward: `sitegen assemble` is the one place a verified game or a
verified frame is carried, and it does that exactly once, from the previous
tree straight into the assembled output. Run with `--publication degraded`,
that assembly retains the last verified domains under this page instead of
treating this page as the replacement for them.

So the only decision left here is what the page may say. It may link the
retained game when the previous publication's own `last-green.json` parses,
names that tree, and names a complete playable package — and otherwise it says
nothing about a game at all. A previous tree that is corrupt, symlinked,
truncated, forged, or simply absent costs this page a sentence; it never costs
the run its publication.
"""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import shutil
import stat
import sys
import tempfile
from pathlib import Path, PurePosixPath

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

#: Every field `sitegen` writes into the manifest that vouches for a published
#: game. A document shaped like anything else was not written by the generator,
#: so it proves nothing about the directory beside it.
LAST_GREEN_FIELDS = {
    "source_commit",
    "semantic_visual_hash",
    "game_files",
    "screenshot_files",
}

#: The manifest is a small JSON document. Anything larger is not read at all,
#: so a hostile or truncated previous publication cannot be streamed into this
#: process before it is refused.
MAX_MANIFEST_BYTES = 1 << 20

#: The most game files a manifest may name. The check below stats each named
#: file exactly once, so this is the hard ceiling on the work a previous
#: publication can ask of this generator.
MAX_GAME_FILES = 4096

#: The one file the status page would link, if it linked anything.
PLAYABLE_ENTRY = "play/index.html"

#: The complete fixed package roster `sitegen` requires before publication.
REQUIRED_PLAYABLE_FILES = (
    PLAYABLE_ENTRY,
    "play/play.js",
    "play/play.css",
    "play/game.js",
    "play/game_bg.wasm",
)

#: The package must also contain at least one generated asset below this root.
PLAYABLE_ASSETS = "play/assets/"

#: The directory the previous publication keeps its game in.
PLAYABLE_ROOT = "play"

#: The manifest that names it.
LAST_GREEN_FILE = "last-green.json"


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


def read_manifest(path: Path) -> dict[str, object] | str:
    """The parsed last-green manifest, or the reason it cannot be trusted."""
    if path.is_symlink() or not path.is_file():
        return f"{path.name} is missing or is not a plain file"
    try:
        size = path.stat().st_size
        if size > MAX_MANIFEST_BYTES:
            return f"{path.name} is {size} bytes, over the {MAX_MANIFEST_BYTES} byte limit"
        with path.open("rb") as handle:
            raw = handle.read(MAX_MANIFEST_BYTES + 1)
    except OSError as failure:
        return f"{path.name} could not be read: {failure}"
    if len(raw) > MAX_MANIFEST_BYTES:
        return f"{path.name} grew past the {MAX_MANIFEST_BYTES} byte limit while it was read"
    try:
        manifest = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as failure:
        return f"{path.name} is not the JSON this generator writes: {failure}"
    if not isinstance(manifest, dict) or set(manifest) != LAST_GREEN_FIELDS:
        return f"{path.name} is not a publication manifest"
    return manifest


def named_game_file(previous: Path, root: str, entry: object) -> str | None:
    """The reason one manifest entry cannot be trusted, or ``None``."""
    if not isinstance(entry, str) or not entry:
        return f"{entry!r} is not a published path"
    if "\x00" in entry or "\\" in entry:
        return f"{entry!r} is not a published path"
    relative = PurePosixPath(entry)
    if (
        relative.is_absolute()
        or len(relative.parts) < 2
        or relative.parts[0] != PLAYABLE_ROOT
        or ".." in relative.parts
        or "." in relative.parts
    ):
        return f"{entry!r} is not inside {PLAYABLE_ROOT}/"
    candidate = previous.joinpath(*relative.parts)
    if candidate.is_symlink():
        return f"{entry} is a symbolic link"
    try:
        status = os.lstat(candidate)
    except OSError as failure:
        return f"{entry} is not published: {failure}"
    if not stat.S_ISREG(status.st_mode):
        return f"{entry} is not a plain file"
    try:
        resolved = os.path.realpath(candidate)
    except OSError as failure:
        return f"{entry} could not be resolved: {failure}"
    try:
        contained = os.path.commonpath([root, resolved]) == root
    except ValueError:
        contained = False
    if not contained:
        return f"{entry} resolves outside {PLAYABLE_ROOT}/"
    return None


def playable_refusal(previous: Path | None) -> str | None:
    """Why the previous playable domain may not be claimed, or ``None``.

    Nothing here can stop the degraded publication. Every answer but ``None``
    is a reason for the page to say less, never a reason to publish nothing:
    an unreadable, symlinked, truncated, or forged previous tree costs this run
    a sentence, not its status.

    A claim is earned by provenance, never by existence. The manifest the
    generator wrote has to parse, name this exact tree, and name every required
    file plus an asset; the files it names have to be plain files really inside
    the package. Anything short of that leaves the domain unclaimed and
    untouched, for assembly to retain or refuse on its own terms.
    """
    if previous is None:
        return "no previous publication was checked out"
    if previous.is_symlink() or not previous.is_dir():
        return f"{previous} is not a directory"

    package = previous / PLAYABLE_ROOT
    if package.is_symlink() or not package.is_dir():
        return f"the previous publication has no plain {PLAYABLE_ROOT}/ directory"

    manifest = read_manifest(previous / LAST_GREEN_FILE)
    if isinstance(manifest, str):
        return manifest

    source_commit = manifest["source_commit"]
    if not isinstance(source_commit, str) or not source_commit.strip():
        return f"{LAST_GREEN_FILE} names no source commit"
    game_files = manifest["game_files"]
    if not isinstance(game_files, list) or not game_files:
        return f"{LAST_GREEN_FILE} names no game files"
    if len(game_files) > MAX_GAME_FILES:
        return (
            f"{LAST_GREEN_FILE} names {len(game_files)} game files, "
            f"over the {MAX_GAME_FILES} file limit"
        )

    try:
        root = os.path.realpath(package)
    except OSError as failure:
        return f"{PLAYABLE_ROOT}/ could not be resolved: {failure}"

    named = set()
    for entry in game_files:
        refusal = named_game_file(previous, root, entry)
        if refusal is not None:
            return f"{LAST_GREEN_FILE} {refusal}"
        named.add(PurePosixPath(entry).as_posix())

    for required in REQUIRED_PLAYABLE_FILES:
        if required not in named:
            return f"{LAST_GREEN_FILE} does not name {required}"
    if not any(entry.startswith(PLAYABLE_ASSETS) for entry in named):
        return f"{LAST_GREEN_FILE} does not name a file below {PLAYABLE_ASSETS}"
    return None


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
        "Assembly retains it unchanged from the previous publication; this run "
        "published no game of its own.</p>"
        if has_game
        else "<p>This publication links no playable build.</p>"
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


def write_site(workflow_path: Path, previous: Path | None, output: Path) -> None:
    """Writes the current degraded surface, and only that.

    Not one byte of the previous publication is copied here. `sitegen assemble`
    already lays a validated previous tree down under this one exactly once,
    and a second copy through this script would double the transfer, duplicate
    a trust boundary that is stricter than anything Python does here, and dress
    the retained game up as this run's own.
    """
    workflow = read_workflow(workflow_path)
    refusal = playable_refusal(previous)
    if refusal is not None:
        print(
            f"the retained playable build is not claimed: {refusal}",
            file=sys.stderr,
        )
    if output.exists() or output.is_symlink():
        raise FallbackError(f"{output} already exists")
    output.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(
        tempfile.mkdtemp(prefix=f".{output.name}-history-failure-", dir=output.parent)
    )
    try:
        (staging / "index.html").write_text(
            render_status(workflow, refusal is None),
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
