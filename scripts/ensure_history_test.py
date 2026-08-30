#!/usr/bin/env python3
"""Tests for the bounded history the published timeline needs.

Every test builds a real repository on disk and runs the real script against
it. Nothing here reaches the network: the "remote" is another directory, and
the one test that deepens does it over `file://`.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "ensure-history.sh"

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

    def test_the_workflow_lint_still_runs_before_the_expensive_gates(self) -> None:
        """Preserved from the contract this file's ordering rule sits beside."""
        lint = self.CHECK.index("./scripts/actionlint.sh")
        clippy = self.CHECK.index("cargo clippy")

        self.assertLess(lint, clippy)


class WorkflowGateOrderTest(unittest.TestCase):
    """Nothing that can fail runs before the result root exists."""

    WORKFLOW = (
        SCRIPT.parent.parent / ".github" / "workflows" / "pages.yml"
    ).read_text(encoding="utf-8")

    def job(self, name: str) -> str:
        start = self.WORKFLOW.index(f"\n  {name}:\n")
        rest = self.WORKFLOW[start + 1 :]
        end = rest.find("\n  publish:\n") if name != "publish" else -1
        return rest if end < 0 else rest[:end]

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

    def test_the_recorded_row_counts_include_the_history_gates(self) -> None:
        self.assertIn("expected=14", self.job("verify"))
        self.assertIn("expected=4", self.job("build-web"))

    def test_publish_never_ends_on_a_history_repair(self) -> None:
        """Publish has to publish, whatever the history turned out to be."""
        publish = self.job("publish")
        step = publish.index("- name: Reach every referenced commit if the history allows it")
        body = publish[step : step + 400]

        self.assertIn("continue-on-error: true", body)


if __name__ == "__main__":
    unittest.main(verbosity=2)
