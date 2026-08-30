#!/usr/bin/env python3
"""Tests for the bounded history the published timeline needs.

Every test builds a real repository on disk and runs the real script against
it. Nothing here reaches the network: the "remote" is another directory, and
the one test that deepens does it over `file://`.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "ensure-history.sh"
FALLBACK = Path(__file__).resolve().parent / "history_failure_site.py"

#: Forty hexadecimal digits that name nothing in any repository here.
ABSENT = "0" * 39 + "1"
ABSENT_UPPER = "A" * 39 + "B"


def git(repository: Path, *arguments: str, check: bool = True) -> str:
    result = subprocess.run(
        ["git", *arguments],
        cwd=repository,
        capture_output=True,
        text=True,
        check=check,
    )
    return result.stdout.strip()


def make_repository(directory: Path, commits: int = 3) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    git(directory, "init", "--quiet", "--initial-branch=main")
    git(directory, "config", "user.name", "gate")
    git(directory, "config", "user.email", "gate@example.invalid")
    git(directory, "config", "uploadpack.allowfilter", "true")
    git(directory, "config", "uploadpack.allowanysha1inwant", "true")
    (directory / "scripts").mkdir(exist_ok=True)
    shutil.copy2(SCRIPT, directory / "scripts" / SCRIPT.name)
    (directory / "docs").mkdir(exist_ok=True)
    for index in range(commits):
        (directory / f"file-{index}.txt").write_text(str(index), encoding="utf-8")
        git(directory, "add", "-A")
        git(directory, "commit", "--quiet", "-m", f"commit {index}")
    return directory


def write_progress(repository: Path, document: object) -> None:
    (repository / "docs").mkdir(exist_ok=True)
    (repository / "docs" / "progress.json").write_text(
        json.dumps(document), encoding="utf-8"
    )


def run_script(repository: Path, **environment: str) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env.pop("GITHUB_REF_NAME", None)
    # Nothing may be deepened unless a test asks for it, so the default is a
    # check that reports what it found and fetches nothing at all.
    env.setdefault("HISTORY_DEEPEN_ROUNDS", "0")
    env.update(environment)
    return subprocess.run(
        ["bash", str(repository / "scripts" / SCRIPT.name)],
        cwd=repository,
        capture_output=True,
        text=True,
        env=env,
    )


class CommitFieldParsingTest(unittest.TestCase):
    """Only the fields the generator resolves, and only values it accepts."""

    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root, True)
        self.repository = make_repository(self.root / "repository")

    def demanded(self, document: object) -> set[str]:
        """The commits the script says the history has to reach."""
        write_progress(self.repository, document)
        result = run_script(self.repository)
        if result.returncode == 0:
            return set()
        return {
            line.strip()
            for line in result.stderr.splitlines()
            if len(line.strip()) == 40
        }

    # -- positive fixtures -------------------------------------------------

    def test_a_lowercase_completed_commit_is_demanded(self) -> None:
        document = {"tasks": [{"id": "one", "completed_commit": ABSENT}]}

        self.assertEqual(self.demanded(document), {ABSENT})

    def test_an_uppercase_resolved_commit_is_demanded_in_lower_case(self) -> None:
        """`rev-list` prints lower case, so the comparison has to fold."""
        document = {"challenges": [{"id": "c", "resolved_commit": ABSENT_UPPER}]}

        self.assertEqual(self.demanded(document), {ABSENT_UPPER.lower()})

    def test_both_commit_fields_are_read_however_deeply_they_are_nested(self) -> None:
        document = {
            "project": {"tasks": [{"completed_commit": ABSENT}]},
            "extra": [[{"challenges": [{"resolved_commit": ABSENT_UPPER}]}]],
        }

        self.assertEqual(self.demanded(document), {ABSENT, ABSENT_UPPER.lower()})

    def test_a_reachable_commit_is_not_demanded(self) -> None:
        head = git(self.repository, "rev-parse", "HEAD")
        document = {"tasks": [{"completed_commit": head}]}

        write_progress(self.repository, document)
        result = run_script(self.repository)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("every published commit is reachable", result.stdout)

    # -- negative fixtures -------------------------------------------------

    def test_the_symbolic_head_reference_needs_no_history(self) -> None:
        write_progress(self.repository, {"tasks": [{"completed_commit": "HEAD"}]})

        result = run_script(self.repository)

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_an_unrelated_forty_hex_value_is_not_a_commit(self) -> None:
        """A content digest is not history, and must not deepen anything."""
        document = {
            "references": {"key_art_sha256": ABSENT},
            "evidence": {"semantic_visual_hash": ABSENT_UPPER},
            "tasks": [{"id": "one", "status": "done"}],
        }

        self.assertEqual(self.demanded(document), set())

    def test_a_commit_field_the_generator_would_refuse_is_left_to_it(self) -> None:
        """Deepening cannot make `not-a-commit` valid, so it is not demanded."""
        document = {
            "tasks": [
                {"completed_commit": "not-a-commit"},
                {"completed_commit": ABSENT[:39]},
                {"completed_commit": ""},
            ]
        }

        self.assertEqual(self.demanded(document), set())

    def test_a_commit_shaped_value_under_another_key_is_ignored(self) -> None:
        document = {"tasks": [{"id": "one", "started_commit": ABSENT}]}

        self.assertEqual(self.demanded(document), set())


class DeepeningTest(unittest.TestCase):
    """Deepening fetches this branch, filtered, and nothing else."""

    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root, True)
        self.origin = make_repository(self.root / "origin", commits=8)
        # A second branch stands in for `pages-live`: a branch this check must
        # never fetch, however deep it has to go on the branch it is publishing.
        git(self.origin, "checkout", "--quiet", "-b", "pages-live")
        (self.origin / "huge.bin").write_text("x" * 4096, encoding="utf-8")
        git(self.origin, "add", "-A")
        git(self.origin, "commit", "--quiet", "-m", "generated publication")
        git(self.origin, "checkout", "--quiet", "main")

        self.clone = self.root / "clone"
        subprocess.run(
            [
                "git",
                "clone",
                "--quiet",
                "--depth=1",
                "--single-branch",
                "--branch=main",
                f"file://{self.origin}",
                str(self.clone),
            ],
            check=True,
            capture_output=True,
        )
        (self.clone / "scripts").mkdir(exist_ok=True)
        shutil.copy2(SCRIPT, self.clone / "scripts" / SCRIPT.name)

    def stub_git(self, log: Path) -> Path:
        """A `git` that records every fetch and then runs the real one."""
        directory = self.root / "stub"
        directory.mkdir(exist_ok=True)
        real = shutil.which("git")
        stub = directory / "git"
        stub.write_text(
            "#!/usr/bin/env bash\n"
            'if [[ "${1:-}" == "fetch" ]]; then\n'
            f'  printf "%s\\n" "$*" >>"{log}"\n'
            "fi\n"
            f'exec "{real}" "$@"\n',
            encoding="utf-8",
        )
        stub.chmod(0o755)
        return directory

    def test_a_short_clone_is_deepened_until_the_commit_is_reachable(self) -> None:
        oldest = git(self.origin, "rev-list", "--max-parents=0", "HEAD")
        write_progress(self.clone, {"tasks": [{"completed_commit": oldest}]})
        log = self.root / "fetches.log"

        before = run_script(self.clone, HISTORY_DEEPEN_ROUNDS="0")
        self.assertEqual(before.returncode, 1, "the shallow clone should be short")

        stub = self.stub_git(log)
        result = run_script(
            self.clone,
            HISTORY_DEPTH="4",
            HISTORY_DEEPEN_ROUNDS="4",
            PATH=f"{stub}{os.pathsep}{os.environ['PATH']}",
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("every published commit is reachable", result.stdout)
        self.fetches = log.read_text(encoding="utf-8").splitlines()
        self.assertTrue(self.fetches, "the script should have deepened")
        for fetch in self.fetches:
            self.assertIn("--deepen=4", fetch)
            self.assertIn("--filter=blob:none", fetch)
            self.assertIn("--no-tags", fetch)
            self.assertIn("+refs/heads/main:refs/remotes/origin/main", fetch)

    def test_deepening_never_brings_the_generated_branch_with_it(self) -> None:
        oldest = git(self.origin, "rev-list", "--max-parents=0", "HEAD")
        write_progress(self.clone, {"tasks": [{"completed_commit": oldest}]})

        result = run_script(self.clone, HISTORY_DEPTH="4", HISTORY_DEEPEN_ROUNDS="4")

        self.assertEqual(result.returncode, 0, result.stderr)
        refs = git(self.clone, "for-each-ref", "--format=%(refname)")
        self.assertNotIn("pages-live", refs, refs)
        published = git(self.origin, "rev-parse", "pages-live")
        # Asking `cat-file` about a missing object would lazily fetch it from
        # the promisor remote and answer its own question, so the local object
        # store is enumerated instead.
        local = git(self.clone, "cat-file", "--batch-check", "--batch-all-objects")
        self.assertNotIn(
            published,
            local,
            "deepening pulled the generated branch's publication into the clone",
        )

    def test_the_workflow_branch_name_is_honoured_when_head_is_detached(self) -> None:
        oldest = git(self.origin, "rev-list", "--max-parents=0", "HEAD")
        write_progress(self.clone, {"tasks": [{"completed_commit": oldest}]})
        git(self.clone, "checkout", "--quiet", "--detach", "HEAD")
        log = self.root / "detached.log"
        stub = self.stub_git(log)

        result = run_script(
            self.clone,
            GITHUB_REF_NAME="main",
            HISTORY_DEPTH="4",
            HISTORY_DEEPEN_ROUNDS="4",
            PATH=f"{stub}{os.pathsep}{os.environ['PATH']}",
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        for fetch in log.read_text(encoding="utf-8").splitlines():
            self.assertIn("+refs/heads/main:refs/remotes/origin/main", fetch)

    def test_a_detached_head_with_no_named_branch_is_refused(self) -> None:
        oldest = git(self.origin, "rev-list", "--max-parents=0", "HEAD")
        write_progress(self.clone, {"tasks": [{"completed_commit": oldest}]})
        git(self.clone, "checkout", "--quiet", "--detach", "HEAD")

        result = run_script(self.clone, HISTORY_DEEPEN_ROUNDS="4")

        self.assertEqual(result.returncode, 1)
        self.assertIn("detached HEAD", result.stderr)


class DeepeningSourceTest(unittest.TestCase):
    """The refspec and the filter are the fix, so they are pinned."""

    SOURCE = SCRIPT.read_text(encoding="utf-8")

    def test_the_deepen_names_one_branch_and_keeps_the_filter(self) -> None:
        self.assertIn(
            'git fetch --deepen="$depth" --filter=blob:none --no-tags origin "$refspec"',
            self.SOURCE,
        )
        self.assertIn(
            'refspec="+refs/heads/${branch}:refs/remotes/origin/${branch}"',
            self.SOURCE,
        )

    def test_there_is_no_unfiltered_or_branchless_fallback(self) -> None:
        """The defect this replaced: a fallback that fetched everything.

        A bare `git fetch --deepen origin` deepens every remote branch, and
        without the filter it downloads every blob on each of them.
        """
        fetches = [
            line.strip()
            for line in self.SOURCE.splitlines()
            if "git fetch" in line and not line.strip().startswith("#")
        ]

        self.assertEqual(len(fetches), 1, fetches)
        self.assertIn("--filter=blob:none", fetches[0])
        self.assertIn("$refspec", fetches[0])

    def test_only_the_generators_own_commit_fields_are_read(self) -> None:
        self.assertIn('COMMIT_FIELDS = ("completed_commit", "resolved_commit")', self.SOURCE)
        self.assertIn('SYMBOLIC = {"HEAD"}', self.SOURCE)
        self.assertIn("[0-9a-fA-F]{40}", self.SOURCE)


class CleanGateOrderTest(unittest.TestCase):
    """The local gate repairs history before anything validates against it."""

    CHECK = (SCRIPT.parent / "check.sh").read_text(encoding="utf-8")

    def test_the_history_bound_runs_before_progress_validation(self) -> None:
        """The defect this guards: `set -e` ending the gate on a short clone.

        `sitegen validate` resolves the commit every finished task names. On a
        shallow checkout that fails, and it fails hard, so the run never
        reaches the step that would have deepened the history for it.
        """
        history = self.CHECK.index("./scripts/ensure-history.sh")
        validate = self.CHECK.index("sitegen -- validate")

        self.assertLess(
            history, validate, "history must be repaired before it is validated"
        )

    def test_the_history_bound_and_its_tests_are_both_in_the_clean_gate(self) -> None:
        self.assertIn("./scripts/ensure-history.sh", self.CHECK)
        self.assertIn("python3 scripts/ensure_history_test.py", self.CHECK)

    def test_the_browser_gate_unit_suite_runs_before_any_browser_can_launch(self) -> None:
        unit = self.CHECK.index("python3 scripts/browser_gate_test.py")
        renderer_probe = self.CHECK.index('render_command=(')
        browser = self.CHECK.index('step "playable web build and browser gate"')

        self.assertLess(unit, renderer_probe)
        self.assertLess(unit, browser)

    def test_the_workflow_lint_still_runs_before_the_expensive_gates(self) -> None:
        """Preserved from the contract this file's ordering rule sits beside."""
        lint = self.CHECK.index("./scripts/actionlint.sh")
        clippy = self.CHECK.index("cargo clippy")

        self.assertLess(lint, clippy)


class HistoryFailureSiteTest(unittest.TestCase):
    """A short Publish checkout still produces an honest current status page."""

    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root, True)
        self.workflow = self.root / "workflow.json"
        self.output = self.root / "current"
        self.document = {
            "source_commit": "1" * 40,
            "run_url": "https://github.com/ridermw/midcreek-cs-1/actions/runs/123",
            "native": "passed",
            "web": "failed",
            "gates": [
                {
                    "name": "Workflow lint",
                    "status": "passed",
                    "passed": 1,
                    "failed": 0,
                    "duration_ms": 12,
                    "artifact_url": None,
                },
                {
                    "name": "Headless browser gate",
                    "status": "failed",
                    "passed": 0,
                    "failed": 1,
                    "duration_ms": 34,
                    "artifact_url": None,
                },
            ],
        }
        self.workflow.write_text(json.dumps(self.document), encoding="utf-8")

    def run_fallback(
        self, previous: Path | None = None
    ) -> subprocess.CompletedProcess[str]:
        arguments = [
            sys.executable,
            str(FALLBACK),
            "--workflow",
            str(self.workflow),
            "--output",
            str(self.output),
        ]
        if previous is not None:
            arguments.extend(["--previous", str(previous)])
        return subprocess.run(arguments, capture_output=True, text=True)

    def test_the_fallback_publishes_current_failure_and_retains_the_last_green_game(
        self,
    ) -> None:
        self.document["web"] = "passed"
        self.document["gates"][1].update(
            {"status": "passed", "passed": 1, "failed": 0}
        )
        self.workflow.write_text(json.dumps(self.document), encoding="utf-8")
        previous = self.root / "previous"
        (previous / "play" / "assets").mkdir(parents=True)
        for relative in (
            "play/index.html",
            "play/play.js",
            "play/play.css",
            "play/game.js",
            "play/game_bg.wasm",
            "play/assets/rack.glb",
        ):
            path = previous / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"retained {relative}", encoding="utf-8")
        (previous / "last-green.json").write_text(
            '{"source_commit":"old"}', encoding="utf-8"
        )
        (previous / "index.html").write_text(
            "STALE STATUS MUST NOT SURVIVE", encoding="utf-8"
        )

        result = self.run_fallback(previous)

        self.assertEqual(result.returncode, 0, result.stderr)
        page = (self.output / "index.html").read_text(encoding="utf-8")
        self.assertIn("Publication history unavailable", page)
        self.assertIn("11111111", page)
        self.assertIn('Native</span><strong class="passed">Passed', page)
        self.assertIn('Web</span><strong class="passed">Passed', page)
        self.assertIn("Publish history bound", page)
        self.assertIn("Headless browser gate", page)
        self.assertNotIn("STALE STATUS MUST NOT SURVIVE", page)
        self.assertEqual(
            (self.output / "play" / "game_bg.wasm").read_text(encoding="utf-8"),
            "retained play/game_bg.wasm",
        )
        self.assertEqual(
            (self.output / "last-green.json").read_text(encoding="utf-8"),
            '{"source_commit":"old"}',
        )

    def test_a_first_run_failure_publishes_status_without_inventing_a_game(
        self,
    ) -> None:
        result = self.run_fallback()

        self.assertEqual(result.returncode, 0, result.stderr)
        page = (self.output / "index.html").read_text(encoding="utf-8")
        self.assertIn("No previous verified game was available", page)
        self.assertIn('Web</span><strong class="failed">Failed', page)
        self.assertFalse((self.output / "play").exists())
        self.assertFalse((self.output / "last-green.json").exists())

    def test_a_symlinked_retained_game_is_refused(self) -> None:
        previous = self.root / "previous"
        previous.mkdir()
        target = self.root / "outside"
        target.mkdir()
        (previous / "play").symlink_to(target, target_is_directory=True)

        result = self.run_fallback(previous)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symbolic link", result.stderr)
        self.assertFalse((self.output / "index.html").exists())


class WorkflowGateOrderTest(unittest.TestCase):
    """Nothing that can fail runs before the result root exists."""

    WORKFLOW = (
        SCRIPT.parent.parent / ".github" / "workflows" / "pages.yml"
    ).read_text(encoding="utf-8")

    def job(self, name: str) -> str:
        jobs = self.WORKFLOW[self.WORKFLOW.index("\njobs:\n") :]
        headers = list(re.finditer(r"(?m)^  ([A-Za-z_][A-Za-z0-9_-]*):\n", jobs))
        for index, header in enumerate(headers):
            if header.group(1) != name:
                continue
            end = headers[index + 1].start() if index + 1 < len(headers) else len(jobs)
            return jobs[header.start() : end]
        self.fail(f"workflow should declare the {name} job")

    def test_each_measured_job_resolves_its_result_root_first(self) -> None:
        """The defect this guards: a job that died with nothing to report.

        A step that fails before `RESULT` and `GATES` exist leaves the summary
        with no gate file to read and no root to write, so the run publishes a
        failure that cannot say which gate it was.
        """
        for name, root in (("verify", "native"), ("build-web", "web")):
            job = self.job(name)
            resolve = job.index(f"- name: Resolve the {root} result root")
            for later in ("ensure-history.sh", "run-gate.sh", "apt-get"):
                self.assertLess(
                    resolve,
                    job.index(later),
                    f"{name} should resolve its result root before {later}",
                )

    def test_history_validation_is_a_measured_gate_in_both_jobs(self) -> None:
        for name in ("verify", "build-web"):
            job = self.job(name)
            self.assertIn(
                'run-gate.sh "$GATES" "Published history bound" -- \\', job, name
            )
        self.assertIn(
            'run-gate.sh "$GATES" "History bound tests" -- \\', self.job("verify")
        )

    def test_each_recorded_row_count_matches_its_jobs_named_gates(self) -> None:
        for name in ("verify", "build-web"):
            job = self.job(name)
            expected = re.findall(r"(?m)^\s+expected=(\d+)$", job)
            calls = re.findall(
                r"(?m)^\s+\./scripts/run-gate\.sh(?:\s|$)", job
            )
            self.assertEqual(len(expected), 1, f"{name} should declare one count")
            self.assertEqual(
                int(expected[0]),
                len(calls),
                f"{name} should count only its own named gates",
            )

    def test_verify_cannot_borrow_build_web_gates_or_counts(self) -> None:
        verify = self.job("verify")

        self.assertNotIn("Resolve the web result root", verify)
        self.assertNotIn("WebAssembly toolchain", verify)
        self.assertNotIn("expected=4", verify)

    def test_the_browser_gate_unit_suite_is_a_named_verify_gate(self) -> None:
        verify = self.job("verify")
        web = self.job("build-web")

        self.assertIn(
            'run-gate.sh "$GATES" "Browser gate unit tests" -- \\', verify
        )
        self.assertIn("python3 scripts/browser_gate_test.py", verify)
        self.assertNotIn("python3 scripts/browser_gate_test.py", web)
        self.assertLess(
            verify.index("python3 scripts/browser_gate_test.py"),
            verify.index("apt-get"),
            "the non-browser suite should run before renderer setup can fail",
        )

    def test_publish_never_ends_on_a_history_repair(self) -> None:
        """Publish has to publish, whatever the history turned out to be."""
        publish = self.job("publish")
        step = publish.index("- name: Reach every referenced commit if the history allows it")
        body = publish[step : step + 400]

        self.assertIn("continue-on-error: true", body)

    def test_publish_uses_an_honest_fallback_when_history_repair_fails(self) -> None:
        publish = self.job("publish")
        history = publish.index(
            "- name: Reach every referenced commit if the history allows it"
        )
        build = publish.index("- name: Build and assemble current status")
        body = publish[build:]

        self.assertIn("id: history", publish[history:build])
        self.assertIn("PUBLISH_HISTORY: ${{ steps.history.outcome }}", body)
        self.assertIn('if [[ "$PUBLISH_HISTORY" == "success" ]]; then', body)
        self.assertIn("cargo run --quiet --bin sitegen -- build", body)
        self.assertIn("python3 scripts/history_failure_site.py", body)
        self.assertLess(body.index("history_failure_site.py"), body.index("sitegen -- assemble"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
