# Pages Task 3 Report: Immediate Status-Only Publication

**Date:** August 29, 2026  
**Branch:** `main`  
**Starting commit:** `ff8affe`  
**Delivery:** One local commit (`6dfcf58`); no push.

## Outcome

Task 3 is complete. The repository now contains its first automatic GitHub
Pages workflow and a safe `sitegen assemble` implementation.

Every push to `main`, and every manual dispatch, now starts a non-canceling
Pages run. The workflow verifies the progress model and site generator, builds
the current status-only site even after a failed Verify job when the generator
remains buildable, preserves last-green game artifacts on failed runs, pushes
the generated `pages-live` branch, uploads the Pages artifact, and deploys it
through the official Pages actions.

The workflow grants read-only repository access to Verify. Only Publish
receives `contents: write`, `pages: write`, `id-token: write`, and
`actions: read`.

## Safe Assembly

`assemble_site` now supports three dispositions:

- `FirstRunStatusOnly` publishes the current status site without inventing a
  playable game.
- `FailedRetainLastGreen` copies the previous `play/`, `screenshots/`, and
  `last-green.json` artifacts when present, then overlays current status data.
- `GreenReplacement` copies the complete current site for future verified web
  builds.

Assembly accepts only an absent or empty output directory. It never recursively
deletes a caller-supplied path, never follows source symlinks, and rejects a
nonempty output before writing. It validates the current site before creating
the output directory, so invalid input leaves no output side effect.

The `sitegen assemble` CLI reads the workflow result JSON, invokes the library
assembly function, prints the selected disposition, and preserves the existing
argument and content error-code conventions.

## Pages Workflow

`.github/workflows/pages.yml` provides:

- `push` on `main` and `workflow_dispatch` only;
- one `pages` concurrency group with `cancel-in-progress: false`;
- a read-only Verify job that runs both site test suites and validates the
  canonical publication inputs;
- an `if: always()` Publish job;
- graceful first-run checkout when `pages-live` does not exist;
- generated workflow and repository summaries for the current commit;
- status-site build and last-green-aware assembly in runner-owned temporary
  directories;
- `.nojekyll` initialization;
- generated-branch publication with the default GitHub token;
- `actions/configure-pages@v6`, `actions/upload-pages-artifact@v5`, and
  `actions/deploy-pages@v5`.

The workflow has no pull-request trigger, `pull_request_target`, or
`${{ secrets.* }}` reference. Pushes to `pages-live` cannot trigger another
Pages run because the push trigger names only `main`.

## Progress Milestone

`docs/progress.json` now records:

- `pages-foundation` as `done`;
- `pages-foundation.completed_commit` as the exact `HEAD` sentinel;
- `autonomous-assets` as the sole `in_progress` task.

The validator resolves `HEAD` to the workflow commit and confirms that
`autonomous-assets` is dependency-ready.

## TDD Evidence

| Slice | RED evidence | GREEN evidence |
|---|---|---|
| First-run and failed-run assembly | `cargo test --test pages_assembly_contract` failed with E0432 because `assemble_site` did not exist. | First-run status-only publication, failed-run artifact retention, and caller-directory protection pass. |
| Workflow contract | Four focused workflow tests failed because `.github/workflows/pages.yml` did not exist. | Trigger, concurrency, official action, forbidden-reference, and permission assertions pass. |
| Progress transition | The focused progress suite failed because canonical data still named `pages-foundation` as current. | Canonical graph and CLI output now identify `autonomous-assets` as current. |
| Invalid current input | The new regression test failed because assembly created output before rejecting a missing current site. | Input validation now precedes output creation. |
| Failed source label | The renderer contract failed because the status strip showed only the short SHA and failed gate. | Failed workflows now render `CURRENT SOURCE: FAILED AT <short-sha>`. |

## Verification

The final gate passed:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --test sitegen_contract              # 37 passed
cargo test --test pages_assembly_contract       # 8 passed
cargo run --bin sitegen -- validate ...         # autonomous-assets
cargo test                                      # 67 integration tests passed
git diff --check
```

The checked-in workflow also parsed as YAML. Its generated-branch Git plumbing
was exercised locally with a bare Git directory and an external work tree.

## Self-Review

The task prohibited subagents and reviewers, so all review passes ran inline.

### Simplification review

- Reuse: 0 changes. No existing repository helper safely copied recursive
  artifact trees without following symlinks.
- Quality: 1 change. Assembly now validates the current site before creating
  output.
- Efficiency: 0 changes. Directory entries are read once and sorted for stable
  copying; retained artifacts are limited to three named paths.
- Skipped: 0 findings.

### Adversarial review

Mode: Rubber Duck. Execution path: single-agent. No independent consensus
ranking was possible.

The premortem considered token exposure, recursive workflow triggers, failed
verification, missing first-run state, symlink traversal, partial deployment,
and last-green data loss.

One accepted finding remained after verification: the real failed workflow
site lacked the explicit failure label used by the assembly fixture. The
renderer now emits `CURRENT SOURCE: FAILED AT <short-sha>` whenever the native
or web gate fails.

No unresolved correctness, security, permission, data-integrity, supply-chain,
or deployment-order findings remain.

## Files

- `.github/workflows/pages.yml`
- `docs/progress.json`
- `src/bin/sitegen.rs`
- `src/sitegen.rs`
- `tests/pages_assembly_contract.rs`
- `tests/sitegen_contract.rs`
- `tests/support/mod.rs`

## Concerns

The workflow has not run because this task was explicitly committed without a
push. The controller must push `6dfcf58` and monitor the first Pages deployment.

The macOS linker still emits the pre-existing `__eh_frame section too large`
warning while linking the game binary. Clippy remains warning-free, and all
tests pass.

## Commit

`6dfcf58 ci: publish the live project progress hub`

## Fix Round 1: Status-Only Last-Green Retention

**Date:** August 29, 2026  
**Starting commit:** `6dfcf58`

The assembly state model now distinguishes a successful native verification
with dependency-skipped web verification from an actual gate failure. When a
previous site exists, that status-only workflow returns `RetainLastGreen`,
retains `play/`, `screenshots/`, and `last-green.json`, and overlays the current
status site. `FailedRetainLastGreen` is selected only when either the native or
web gate is `Failed`. A status-only first run still returns
`FirstRunStatusOnly`.

The CLI now formats dispositions through the exhaustive `Display`
implementation and prints `RetainLastGreen` for this path. The focused Pages
fixture `tests/fixtures/pages/native-passed-web-skipped.json` records the
native-passed/web-skipped workflow state used by the assembly and CLI
regressions.

### Exact Fix Evidence

RED:

```text
cargo test --test pages_assembly_contract
error[E0599]: no variant, associated function, or constant named
`RetainLastGreen` found for enum `BuildDisposition`
```

GREEN:

```text
cargo fmt --check
cargo test --test pages_assembly_contract  # 11 passed; 0 failed
cargo test --test sitegen_contract         # 37 passed; 0 failed
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
```

The regression verifies that native-passed/web-skipped assembly with previous
artifacts returns `RetainLastGreen`, preserves the prior game hash, overlays
the current status HTML, and contains no failure label. Separate focused tests
retain `FailedRetainLastGreen` for native and web failures.

### Fix Round 1 Concern

The pre-existing macOS linker warning about the oversized `__eh_frame` section
still appears while linking test binaries. Strict Clippy completes without
warnings.

## Fix Round 2: Linux Native Build Prerequisites

**Date:** August 29, 2026  
**Starting commit:** `03768e1`  
**Failed workflow:** GitHub Actions run `33279412285`

Both Verify/Test site generation and Publish/Build and assemble current status
failed before site generation because Bevy's enabled `3d` feature includes
`default_platform`, which enables Wayland, X11, and gamepad support. The Ubuntu
runner lacked the native development metadata required by `wayland-sys`;
`pkg-config` could not resolve `wayland-client.pc`.

Verify and Publish now each install the same explicit native build set before
their first Cargo command:

```text
pkg-config
libwayland-dev
libxkbcommon-dev
libxkbcommon-x11-dev
libudev-dev
libx11-dev
libxi-dev
libxrandr-dev
```

Each job runs `apt-get update` followed by
`apt-get install -y --no-install-recommends`. The workflow trigger,
non-canceling concurrency group, Publish `if: always()`, Pages deployment
actions, and Publish-only write permissions are unchanged.

### Exact Fix Evidence

RED:

```text
cargo test --test pages_assembly_contract \
  workflow_contract::installs_bevy_linux_prerequisites_before_cargo_in_both_jobs \
  -- --exact

test workflow_contract::installs_bevy_linux_prerequisites_before_cargo_in_both_jobs ... FAILED
Verify should install native build prerequisites
```

GREEN:

```text
cargo test --test pages_assembly_contract --test sitegen_contract
# pages_assembly_contract: 12 passed; 0 failed
# sitegen_contract: 37 passed; 0 failed

cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
# all completed successfully
```

The regression test independently inspects Verify and Publish, requires the
complete package set in each job, and proves the install command occurs before
that job's first Cargo command.

### Fix Round 2 Concern

The fix is locally verified but has not run on GitHub-hosted Ubuntu because
this round must not be pushed. The pre-existing macOS linker warning about the
oversized `__eh_frame` section still appears while linking test binaries;
strict Clippy remains warning-free.

## Fix Round 3: Public Plan Path Sanitization

**Date:** August 29, 2026
**Starting commit:** `472ab15`
**Failed workflow:** GitHub Actions run `33279744877`

Verify passed, but Publish/sitegen correctly rejected the generated
`index.html` because the published implementation plan contained five
machine-local `/Users/mattheww/git/midcreek-concept/...` source references.
The public HTML absolute-path rejection remains unchanged.

The canonical master plan now uses repository-relative
`../midcreek-concept/...` references for all five sources. The tracked
`docs/implementation-plan.md` is byte-identical to that external master plan,
with SHA-256
`8768ec95bf6596bfd91cf4b36d53a2df849e3cc70765f75dc60e3dc9c0185e1d`.

`tests/sitegen_contract.rs` now has a focused publication-input regression that
rejects `/Users/`, `file://`, and Windows drive-root paths in the published
plan. The existing approved-plan hash contract and generated HTML
absolute-local-path rejection remain active.

### Exact Fix Evidence

RED:

```text
cargo test --test sitegen_contract \
  progress_contract::publication_inputs::published_plan_contains_no_absolute_local_paths \
  -- --exact

test progress_contract::publication_inputs::published_plan_contains_no_absolute_local_paths ... FAILED
plan contains a macOS user path
```

GREEN:

```text
cargo test --test sitegen_contract          # 38 passed; 0 failed
cargo test --test pages_assembly_contract   # 12 passed; 0 failed
cargo run --bin sitegen -- validate \
  --progress docs/progress.json \
  --plan docs/implementation-plan.md \
  --repository .                            # autonomous-assets
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cmp master-plan docs/implementation-plan.md
git diff --check
# all completed successfully
```

### Fix Round 3 Concern

The fix has not run in GitHub Actions because this round must not be pushed.
The pre-existing macOS linker warning about the oversized `__eh_frame` section
still appears while linking test binaries; strict Clippy remains warning-free.
