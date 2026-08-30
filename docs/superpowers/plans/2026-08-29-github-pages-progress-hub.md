# GitHub Pages Progress Hub Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish an always-current GitHub Pages progress hub with a last-known-good playable Bevy WASM build, concept-art comparisons, screenshots, plans, ASCII diagrams, challenges, test results, and commit history.

**Architecture:** A Rust `sitegen` binary validates one canonical `docs/progress.json` file and renders a static no-npm site. A three-job Pages workflow always publishes current status while replacing the playable game and screenshots only after native, render, WASM, and browser gates pass; a generated `pages-live` branch persists the last green artifacts.

**Tech Stack:** Rust 1.98.0, serde/serde_json, pulldown-cmark, static HTML/CSS/JavaScript, Bevy 0.19.1 WASM, wasm-bindgen CLI, headless Chromium, GitHub Actions, and GitHub Pages.

**Spec:** `docs/superpowers/specs/2026-08-29-github-pages-progress-hub-design.md`

## Global Constraints

- GitHub Pages URL: `https://ridermw.github.io/midcreek-cs-1/`.
- Pages uses workflow deployment and deploys only trusted pushes to `main` plus manual dispatches.
- Every `main` push publishes current status, including failed gate summaries.
- Failed native, render, WASM, or browser gates retain the previous playable game and screenshots.
- A first-run failure publishes a status-only site that states no verified game exists.
- The generated `pages-live` branch is the persistence layer for last-known-good artifacts.
- `docs/progress.json` is the only editable source for Done, Working Now, Future, and Challenges.
- Generated HTML and README status fragments are never hand-edited.
- The site publishes the reviewed implementation plan and preserves ASCII diagrams in `<pre>` blocks.
- The site publishes the approved cel-shift key art and character sheet with source paths and SHA-256 values.
- The site includes a playable WASM build with Arrow, Q/E, and Space controls.
- The site uses no npm, Blender, CMS, database, server application, or human approval gate.
- Every implementation increment updates progress data, passes its available gates, commits, and pushes immediately.
- Every push must be clean: formatting, Clippy, relevant tests, asset freshness, progress validation, and the largest available build/smoke gate pass first.
- This unattended implementation works directly on `main`; it does not wait for pull requests or human review.
- Before each commit and push, `git branch --show-current` must return `main`.
- Every commit includes `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`.
- Workflow-generated public content must exclude secrets, environment variables, absolute local paths, and raw unfiltered logs.

## File Structure

- Modify: `Cargo.toml` — add site generation, Markdown, HTML-test, and WASM support dependencies.
- Modify: `rust-toolchain.toml` — add `wasm32-unknown-unknown`.
- Create: `docs/progress.json` — canonical project and challenge state.
- Create: `docs/implementation-plan.md` — repository copy of the reviewed overall plan.
- Create: `docs/reference/cel-shift-key-art.png` — approved key art.
- Create: `docs/reference/cel-shift-character-sheet.png` — approved character sheet.
- Create: `docs/reference/manifest.json` — provenance and hashes.
- Create: `src/sitegen.rs` — validation, rendering, history, and last-green merge logic.
- Create: `src/bin/sitegen.rs` — `validate`, `build`, and `assemble` CLI.
- Create: `src/web.rs` — WASM-only browser readiness bridge.
- Modify: `src/lib.rs` — install `WebReadyPlugin` only on `wasm32`.
- Create: `site/templates/index.html` — page shell.
- Create: `site/templates/play.html` — playable-game shell.
- Create: `site/static/site.css` — cel-shift progress-hub styling.
- Create: `site/static/site.js` — comparison slider and navigation behavior.
- Create: `site/static/play.js` — WASM bootstrap, key handling, and error capture.
- Create: `scripts/build-web.sh` — pinned WASM package build.
- Create: `scripts/web-smoke.sh` — loopback server and headless Chromium gate.
- Modify: `scripts/check.sh` — include progress and site checks.
- Create: `tests/sitegen_contract.rs` — source validation and output tests.
- Create: `tests/pages_assembly_contract.rs` — last-green merge tests.
- Create: `tests/support/mod.rs` — shared fixture, file, HTML, hash, and repository-fact helpers.
- Create: `tests/fixtures/sitegen/` — green, failed, and invalid source fixtures.
- Create: `.github/workflows/pages.yml` — Verify, Build web, and Publish jobs.
- Modify: `README.md` — Pages link, generated status block, and local commands.

## Shared Interfaces

Define these public types in `src/sitegen.rs`:

```rust
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressDocument {
    pub schema_version: u32,
    pub project: String,
    pub tasks: Vec<ProgressTask>,
    pub challenges: Vec<Challenge>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStatus {
    Future,
    InProgress,
    Done,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressTask {
    pub id: String,
    pub title: String,
    pub status: ProgressStatus,
    pub depends_on: Vec<String>,
    pub summary: String,
    pub completed_commit: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeStatus {
    Open,
    Resolved,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub id: String,
    pub title: String,
    pub status: ChallengeStatus,
    pub impact: String,
    pub approach: String,
    pub resolution: Option<String>,
    pub resolved_commit: Option<String>,
}

pub struct RepoFacts {
    pub head_sha: String,
    pub known_commits: BTreeSet<String>,
    pub commits: Vec<CommitSummary>,
}

pub struct CommitSummary {
    pub sha: String,
    pub subject: String,
    pub committed_at: String,
    pub task_id: Option<String>,
}

pub struct ReferenceAsset {
    pub name: String,
    pub source_path: String,
    pub public_path: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

pub struct ReferenceManifest {
    pub assets: Vec<ReferenceAsset>,
}

pub enum GateStatus {
    Passed,
    Failed,
    SkippedDependency,
}

pub struct GateSummary {
    pub name: String,
    pub status: GateStatus,
    pub passed: u32,
    pub failed: u32,
    pub duration_ms: u64,
    pub artifact_url: Option<String>,
}

pub struct WorkflowSummary {
    pub source_commit: String,
    pub run_url: String,
    pub native: GateStatus,
    pub web: GateStatus,
    pub gates: Vec<GateSummary>,
}

pub struct VerificationSummary {
    pub semantic_visual_hash: String,
    pub frames: BTreeMap<String, String>,
    pub gates: Vec<GateSummary>,
    pub metrics: BTreeMap<String, f64>,
}

pub struct SiteInputs {
    pub progress: ProgressDocument,
    pub plan_markdown: String,
    pub reference_manifest: ReferenceManifest,
    pub verification: Option<VerificationSummary>,
    pub workflow: WorkflowSummary,
    pub repo: RepoFacts,
}

pub enum BuildDisposition {
    GreenReplacement,
    FailedRetainLastGreen,
    FirstRunStatusOnly,
}

pub struct SiteManifest {
    pub source_commit: String,
    pub playable_commit: Option<String>,
    pub current_task: Option<String>,
    pub generated_files: Vec<PathBuf>,
    pub semantic_visual_hash: Option<String>,
}

pub struct GalleryEntry {
    pub semantic_visual_hash: String,
    pub source_commit: String,
    pub committed_at: String,
    pub current_task: String,
    pub frames: BTreeMap<String, String>,
    pub metric_deltas: BTreeMap<String, f64>,
}

pub struct GalleryManifest {
    pub entries: Vec<GalleryEntry>,
}

pub struct LastGreenManifest {
    pub source_commit: String,
    pub semantic_visual_hash: String,
    pub game_files: Vec<PathBuf>,
    pub screenshot_files: Vec<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProgressError {
    UnsupportedSchemaVersion { actual: u32 },
    DuplicateTaskId { task_id: String },
    UnknownPlanTask { task_id: String },
    MultipleCurrentTasks,
    MissingCurrentTask,
    DependencyNotDone { task_id: String, dependency_id: String },
    MissingCompletionCommit { task_id: String },
    UnknownCompletionCommit { task_id: String, commit: String },
    UnexpectedCompletionCommit { task_id: String },
    MissingChallengeContext { challenge_id: String, field: String },
    MissingChallengeResolution { challenge_id: String },
}

#[derive(Debug)]
pub enum SitegenError {
    Io { path: PathBuf, message: String },
    Json { path: PathBuf, message: String },
    Progress(Vec<ProgressError>),
    Markdown { path: PathBuf, message: String },
    MissingInput { path: PathBuf },
    UnsafeOutputPath { path: PathBuf },
    BrokenLocalLink { source: PathBuf, target: PathBuf },
    MissingAltText { source: PathBuf },
    InvalidHtml { path: PathBuf, message: String },
    MissingPreviousArtifact { path: PathBuf },
}

pub fn validate_progress(
    document: &ProgressDocument,
    plan_task_ids: &BTreeSet<String>,
    repo: &RepoFacts,
) -> Result<(), Vec<ProgressError>>;

pub fn build_site(inputs: &SiteInputs, output: &Path) -> Result<SiteManifest, SitegenError>;

pub fn assemble_site(
    previous: Option<&Path>,
    current: &Path,
    result: &WorkflowSummary,
    output: &Path,
) -> Result<BuildDisposition, SitegenError>;

pub fn update_gallery(
    gallery: &GalleryManifest,
    report: &VerificationSummary,
    commit: &CommitSummary,
) -> GalleryManifest;
```

Create `tests/support/mod.rs` with these exact helpers:

```rust
pub fn read(path: impl AsRef<Path>) -> String;
pub fn sha256(path: impl AsRef<Path>) -> String;
pub fn fixture(name: &str) -> ProgressDocument;
pub fn validate_fixture(name: &str) -> Vec<ProgressError>;
pub fn plan_ids() -> BTreeSet<String>;
pub fn repo_facts() -> RepoFacts;
pub fn build_fixture_site(name: &str) -> Result<GeneratedSite, SitegenError>;
pub fn fixture_site(name: &str) -> tempfile::TempDir;
pub fn workflow_fixture(name: &str) -> WorkflowSummary;
pub fn existing_gallery() -> GalleryManifest;
pub fn report_with_hash(hash: &str) -> VerificationSummary;
pub fn commit_summary(sha: &str) -> CommitSummary;
pub fn read_last_green(path: impl AsRef<Path>) -> LastGreenManifest;
pub fn assert_has_element_id(html: &str, id: &str);
pub fn assert_text(html: &str, selector: &str, expected: &str);
```

`GeneratedSite` owns its temporary directory and exposes
`fn root(&self) -> &Path` and `fn index_html(&self) -> String`.

---

### Task 1: Establish canonical progress and publication inputs

**Files:**
- Modify: `Cargo.toml`
- Modify: `rust-toolchain.toml`
- Create: `docs/progress.json`
- Create: `docs/implementation-plan.md`
- Create: `docs/reference/cel-shift-key-art.png`
- Create: `docs/reference/cel-shift-character-sheet.png`
- Create: `docs/reference/manifest.json`
- Create: `src/sitegen.rs`
- Create: `src/bin/sitegen.rs`
- Create: `tests/support/mod.rs`
- Test: `tests/sitegen_contract.rs`

**Interfaces:**
- Consumes: reviewed overall plan and the two approved files from `../midcreek-concept/themes/cel-shift/masters/`.
- Produces: `ProgressDocument`, `RepoFacts`, `validate_progress`, and `sitegen validate`.

- [ ] **Step 1: Mark the Pages foundation as current**

Run `git rev-parse HEAD` after the game-foundation task is committed. Insert
that exact 40-character output as the `completed_commit` for
`foundation-contracts`.

Create `docs/progress.json` with `schema_version: 1`, project
`Cell Shift Data Center POC`, an empty challenge list, and exactly this ordered
task graph:

| ID | Initial status | Dependencies | Initial summary |
|---|---|---|---|
| `foundation-contracts` | `done` | none | Pinned the Bevy project and established reviewed contracts. |
| `pages-foundation` | `in_progress` | `foundation-contracts` | Building the canonical progress model and status-only Pages site. |
| `autonomous-assets` | `future` | `pages-foundation` | Generate autonomous rigged and modular game assets after the status hub is live. |
| `data-hall` | `future` | `autonomous-assets` | Build the authored cel-shift data hall. |
| `technician-movement` | `future` | `data-hall` | Add rigged camera-relative technician movement. |
| `camera-orbit` | `future` | `technician-movement` | Add clamped Q/E four-way camera orbit. |
| `operations-loop` | `future` | `data-hall`, `technician-movement` | Add recurring prioritized faults, tickets, and repair. |
| `operations-hud` | `future` | `camera-orbit`, `operations-loop` | Add ticket HUD, controls, and rack badges. |
| `pages-playable` | `future` | `operations-hud` | Publish the playable WASM game. |
| `autonomous-verification` | `future` | `operations-hud` | Build deterministic gameplay and render verification. |
| `pages-verification` | `future` | `pages-playable`, `autonomous-verification` | Publish comparisons, screenshots, challenges, and test evidence. |
| `pages-status-always` | `future` | `pages-verification` | Retain the last green game while publishing current status. |
| `ci-baseline` | `future` | `pages-status-always` | Publish and verify the final POC baseline. |

Serialize each row as a `ProgressTask`. Set `completed_commit` to `null` for
every task except `foundation-contracts`.

- [ ] **Step 2: Write failing progress-validation tests**

```rust
#[test]
fn accepts_one_dependency_ready_current_task() {
    let document = fixture("green-progress.json");
    assert!(validate_progress(&document, &plan_ids(), &repo_facts()).is_ok());
}

#[test]
fn rejects_two_current_tasks() {
    let errors = validate_fixture("two-current.json");
    assert!(errors.contains(&ProgressError::MultipleCurrentTasks));
}

#[test]
fn rejects_done_task_without_commit() {
    let errors = validate_fixture("done-without-commit.json");
    assert!(errors.iter().any(|error| matches!(
        error,
        ProgressError::MissingCompletionCommit { task_id }
            if task_id == "pages-foundation"
    )));
}

#[test]
fn resolves_head_to_the_workflow_commit() {
    let resolved = resolve_commit_ref("HEAD", &repo_facts()).unwrap();
    assert_eq!(resolved, repo_facts().head_sha);
}
```

- [ ] **Step 3: Run the focused tests and confirm failure**

Run: `cargo test --test sitegen_contract progress_`
Expected: FAIL because `sitegen` types and validation do not exist.

- [ ] **Step 4: Add dependencies and the validation model**

Add serde, serde_json, pulldown-cmark, and the chosen HTML parser as direct dependencies or dev-dependencies. Add the WASM target to `rust-toolchain.toml`.

Implement `#[serde(deny_unknown_fields)]` on every public source type. Return all validation errors in stable task order instead of stopping at the first error.

- [ ] **Step 5: Copy and verify publication sources**

Copy the reviewed overall plan to `docs/implementation-plan.md`. Copy the two approved cel-shift images to `docs/reference/`. Write `manifest.json` with original repository paths, destination paths, dimensions, and the two approved SHA-256 values.

Add tests that hash the destination files and fail on any mismatch.

- [ ] **Step 6: Implement `sitegen validate`**

```rust
enum Command {
    Validate {
        progress: PathBuf,
        plan: PathBuf,
        repository: PathBuf,
    },
    Build {
        inputs: PathBuf,
        output: PathBuf,
    },
    Assemble {
        previous: Option<PathBuf>,
        current: PathBuf,
        result: PathBuf,
        output: PathBuf,
    },
}
```

Invalid arguments exit with code 2. Invalid content exits with code 1 and prints one line per structured error. Successful validation prints the current task ID and exits 0.

- [ ] **Step 7: Run the task gate**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --test sitegen_contract progress_
cargo run --bin sitegen -- validate \
  --progress docs/progress.json \
  --plan docs/implementation-plan.md \
  --repository .
```

Expected: all validation and reference-provenance checks pass.

- [ ] **Step 8: Commit and push**

Mark `pages-foundation` still `in_progress` with a precise summary of this increment.

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml \
  docs/progress.json docs/implementation-plan.md \
  docs/reference/cel-shift-key-art.png \
  docs/reference/cel-shift-character-sheet.png \
  docs/reference/manifest.json \
  src/sitegen.rs src/bin/sitegen.rs \
  tests/support/mod.rs tests/sitegen_contract.rs
git commit -m "feat: establish published project progress model"
git push origin main
```

---

### Task 2: Render the status, plan, challenge, and comparison hub

**Files:**
- Modify: `src/sitegen.rs`
- Modify: `src/bin/sitegen.rs`
- Create: `site/templates/index.html`
- Create: `site/static/site.css`
- Create: `site/static/site.js`
- Create: `tests/fixtures/sitegen/`
- Modify: `tests/sitegen_contract.rs`
- Modify: `docs/progress.json`

**Interfaces:**
- Consumes: `SiteInputs` and `validate_progress`.
- Produces: `build_site`, `SiteManifest`, and a complete status-only site.

- [ ] **Step 1: Write failing generated-site tests**

```rust
#[test]
fn renders_every_required_section() {
    let output = build_fixture_site("green").unwrap();
    let html = read(output.root().join("index.html"));

    for id in [
        "build-status",
        "play",
        "comparison",
        "progress",
        "screenshots",
        "plan",
        "challenges",
        "tests",
        "commits",
    ] {
        assert_has_element_id(&html, id);
    }
}

#[test]
fn preserves_ascii_diagrams_as_preformatted_text() {
    let html = build_fixture_site("green").unwrap().index_html();
    assert!(html.contains("<pre><code class=\"language-text\">"));
    assert!(html.contains("main push"));
}

#[test]
fn escapes_progress_and_commit_content() {
    let html = build_fixture_site("hostile-content").unwrap().index_html();
    assert!(!html.contains("<script>alert("));
    assert!(html.contains("&lt;script&gt;alert("));
}
```

- [ ] **Step 2: Run the focused tests and confirm failure**

Run: `cargo test --test sitegen_contract`
Expected: FAIL because site rendering does not exist.

- [ ] **Step 3: Implement the static site renderer**

Render semantic HTML from the template and generated data using the shared
`SiteManifest`, `GalleryEntry`, and `GalleryManifest` types. Tests compare
`ProgressError` and `SitegenError` variants rather than message substrings.

Convert plan Markdown with `pulldown-cmark`. Escape JSON, Git, test, and challenge strings before rendering. Copy only declared static assets and reference images.

- [ ] **Step 4: Implement the cel-shift visual layout**

Use the approved palette. Build:

- a fixed status strip;
- responsive 16:9 Play panel;
- key-art/current comparison slider;
- character-sheet/current-worker comparison;
- Done, Working Now, and Future columns;
- screenshot timeline;
- plan/diagram viewer;
- challenge cards;
- test matrix;
- commit timeline.

The status-only first version shows “No verified playable build yet” in the Play panel.

- [ ] **Step 5: Implement comparison and navigation JavaScript**

Use vanilla JavaScript. Clamp the comparison slider to `[0, 1]`, support pointer and keyboard input, and preserve alt text outside the clipped visual layers. Do not load CDN scripts.

- [ ] **Step 6: Add output validation**

After rendering, parse every HTML file and assert:

- one `<main>`;
- unique IDs;
- no missing local link or image target;
- alt text on every image;
- no absolute local path;
- no unescaped `<script>` from source data;
- every progress task links to a plan heading.

- [ ] **Step 7: Run the task gate**

Run:

```bash
cargo test --test sitegen_contract
cargo run --bin sitegen -- build \
  --inputs tests/fixtures/sitegen/green/inputs.json \
  --output target/site-preview
```

Expected: all tests pass and `target/site-preview/index.html` contains every required section.

- [ ] **Step 8: Commit and push**

Update the `pages-foundation` summary with the rendered sections.

```bash
git add docs/progress.json \
  site/templates/index.html site/static/site.css site/static/site.js \
  src/sitegen.rs src/bin/sitegen.rs \
  tests/support/mod.rs tests/sitegen_contract.rs tests/fixtures/sitegen
git commit -m "feat: render the cell shift progress hub"
git push origin main
```

---

### Task 3: Publish the status-only Pages site immediately

**Files:**
- Create: `.github/workflows/pages.yml`
- Modify: `src/sitegen.rs`
- Create: `tests/pages_assembly_contract.rs`
- Modify: `docs/progress.json`

**Interfaces:**
- Consumes: a generated current site and optional previous `pages-live` directory.
- Produces: `assemble_site` and the first automatic GitHub Pages deployment.

- [ ] **Step 1: Write failing assembly tests**

```rust
#[test]
fn first_run_without_game_publishes_status_only() {
    let current = fixture_site("status-only");
    let output = tempfile::tempdir().unwrap();
    let disposition = assemble_site(
        None,
        current.path(),
        &workflow_fixture("green-no-web"),
        output.path(),
    )
    .unwrap();
    assert_eq!(disposition, BuildDisposition::FirstRunStatusOnly);
    assert!(output.path().join("index.html").exists());
    assert!(!output.path().join("play/game_bg.wasm").exists());
}

#[test]
fn failed_run_retains_previous_game() {
    let previous = fixture_site("previous-green");
    let current = fixture_site("current-failed");
    let output = tempfile::tempdir().unwrap();
    let old_hash = sha256(previous.path().join("play/game_bg.wasm"));
    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_fixture("failed-native"),
        output.path(),
    )
    .unwrap();
    assert_eq!(disposition, BuildDisposition::FailedRetainLastGreen);
    assert_eq!(sha256(output.path().join("play/game_bg.wasm")), old_hash);
    assert!(read(output.path().join("index.html")).contains("CURRENT SOURCE: FAILED"));
}
```

- [ ] **Step 2: Run the focused tests and confirm failure**

Run: `cargo test --test pages_assembly_contract`
Expected: FAIL because `assemble_site` does not exist.

- [ ] **Step 3: Implement safe last-green assembly**

Create output from scratch. Copy previous `play/`, `screenshots/`, and `last-green.json` only when present. Always replace current HTML, plan, status, challenge, test, and commit data. Never recursively delete a caller-supplied directory; the CLI may clean only its own newly created temporary output.

- [ ] **Step 4: Add the initial Pages workflow**

Create a workflow triggered by `push` to `main` and `workflow_dispatch`. Use one non-canceling concurrency group.

```yaml
name: Pages

on:
  push:
    branches: [main]
  workflow_dispatch:

concurrency:
  group: pages
  cancel-in-progress: false
```

The initial Verify job runs progress/site tests. Publish runs with `if: always()`, initializes `pages-live` with `.nojekyll` when absent, assembles a status-only site, pushes `pages-live`, uploads a Pages artifact, and deploys it through the official Pages actions.

Grant `contents: write`, `pages: write`, `id-token: write`, and `actions: read` only to Publish. Verify receives `contents: read`.

- [ ] **Step 5: Add workflow-source assertions**

Test the checked-in YAML as text for:

- `main`-only push trigger;
- `cancel-in-progress: false`;
- `if: always()` on Publish;
- official `upload-pages-artifact` and `deploy-pages`;
- no `pull_request_target`;
- no `${{ secrets.* }}` reference;
- Publish-only write permissions.

- [ ] **Step 6: Complete the first Pages milestone**

Set `pages-foundation` to `done` with `"completed_commit": "HEAD"`. Set the next dependency-ready game task to `in_progress`.

- [ ] **Step 7: Run the task gate**

Run:

```bash
cargo test --test sitegen_contract
cargo test --test pages_assembly_contract
cargo run --bin sitegen -- validate \
  --progress docs/progress.json \
  --plan docs/implementation-plan.md \
  --repository .
git diff --check
```

Expected: the status-only site and workflow contracts pass.

- [ ] **Step 8: Commit and push**

```bash
git add .github/workflows/pages.yml docs/progress.json src/sitegen.rs tests/pages_assembly_contract.rs
git commit -m "ci: publish the live project progress hub"
git push origin main
```

After the direct push to `main`, wait for the Pages workflow and verify its
API-reported deployment status is successful. This is an objective deployment
check, not visual approval.

---

### Task 4: Package the Bevy game for WebAssembly

**Files:**
- Create: `src/web.rs`
- Modify: `src/lib.rs`
- Create: `site/templates/play.html`
- Create: `site/static/play.js`
- Create: `scripts/build-web.sh`
- Create: `scripts/web-smoke.sh`
- Modify: `Cargo.toml`
- Modify: `tests/sitegen_contract.rs`
- Modify: `docs/progress.json`

**Interfaces:**
- Consumes: the working `CellShiftPlugin`, generated GLBs, and site output.
- Produces: `WebReadyPlugin`, packaged WASM files, and a deterministic browser smoke result.

- [ ] **Step 1: Mark playable web work current and push status**

Set `pages-playable` to `in_progress` only after its game dependencies are `done`.

Run:

```bash
cargo run --bin sitegen -- validate \
  --progress docs/progress.json \
  --plan docs/implementation-plan.md \
  --repository .
git add docs/progress.json
git commit -m "docs: start playable web milestone"
git push origin main
```

- [ ] **Step 2: Write failing readiness and bootstrap tests**

```rust
#[test]
fn play_template_has_explicit_browser_states() {
    let html = read("site/templates/play.html");
    assert!(html.contains("data-game-state=\"loading\""));
    assert!(html.contains("id=\"browser-errors\""));
}

#[test]
fn bootstrap_handles_error_and_rejection_events() {
    let js = read("site/static/play.js");
    assert!(js.contains("window.addEventListener(\"error\""));
    assert!(js.contains("window.addEventListener(\"unhandledrejection\""));
}
```

- [ ] **Step 3: Run tests and confirm failure**

Run: `cargo test --test sitegen_contract`
Expected: FAIL because the play template and bootstrap do not exist.

- [ ] **Step 4: Implement `WebReadyPlugin`**

Compile the plugin only for `wasm32`. After assets and gameplay reach Ready,
count two `PostUpdate` frames, then call a narrow wasm-bindgen function that
sets `document.body.dataset.gameState = "ready"`.

On initialization failure, set `data-game-state="error"` and append a sanitized
message to `#browser-errors`.

- [ ] **Step 5: Implement the browser shell**

Load the generated wasm-bindgen module through relative URLs that work below
`/midcreek-cs-1/`. Focus the canvas on pointer interaction. While focused,
prevent default browser behavior for Arrow keys, Q, E, and Space.

- [ ] **Step 6: Implement the pinned web build**

`scripts/build-web.sh` must:

1. read the locked wasm-bindgen crate version;
2. require the CLI to match exactly;
3. build `--release --target wasm32-unknown-unknown`;
4. run `wasm-bindgen --target web --no-typescript`;
5. copy generated GLBs and browser templates;
6. fail on a missing asset or absolute path.

- [ ] **Step 7: Implement the headless browser gate**

`scripts/web-smoke.sh` starts a loopback server on an available port, launches
Chrome/Chromium headlessly, dumps the DOM, and captures a screenshot. It fails
unless:

- the site and every local asset return HTTP 200;
- `data-game-state="ready"` appears within 30 seconds;
- `#browser-errors` is empty;
- the canvas is visible and 16:9 within one pixel;
- a focused control-key sequence does not move the page scroll position;
- the canvas screenshot region contains at least three approved palette classes
  and nonzero variance.

Always terminate the exact server/browser PIDs and retain diagnostics on failure.

- [ ] **Step 8: Run the task gate**

Run:

```bash
cargo test --test sitegen_contract
./scripts/build-web.sh target/web-preview
./scripts/web-smoke.sh target/web-preview
```

Expected: WASM packages successfully and the browser reaches verified Ready.

- [ ] **Step 9: Commit and push**

Update the current task summary and challenge list with any browser constraints.

```bash
git add Cargo.toml Cargo.lock docs/progress.json src/lib.rs src/web.rs \
  site/templates/play.html site/static/play.js \
  scripts/build-web.sh scripts/web-smoke.sh tests/sitegen_contract.rs
git commit -m "feat: publish a verified playable web build"
git push origin main
```

---

### Task 5: Publish verification results and screenshot history

**Files:**
- Modify: `src/sitegen.rs`
- Modify: `src/bin/sitegen.rs`
- Modify: `site/templates/index.html`
- Modify: `site/static/site.css`
- Modify: `site/static/site.js`
- Modify: `tests/sitegen_contract.rs`
- Modify: `tests/pages_assembly_contract.rs`
- Modify: `docs/progress.json`

**Interfaces:**
- Consumes: native `report.json`, browser-smoke report, reference manifest, and verification PNGs.
- Produces: `VerificationSummary`, `GalleryManifest`, comparison pages, and test matrices.

- [ ] **Step 1: Mark verification publication current and push**

Set `pages-verification` to `in_progress` after autonomous game verification is
`done`. Validate, commit, and push this status before implementation.

- [ ] **Step 2: Write failing verification-view tests**

The report-derived gate is named for what the report can prove. It vouches for
the frames the run captured; it never re-runs `evaluate_frame`, so it cannot
claim the reference image analyzers' verdict. That verdict reaches the site as
a workflow gate, from the job step that really ran the render contract.

```rust
#[test]
fn renders_gate_counts_and_durations() {
    let site = build_fixture_site("verified-game").unwrap();
    assert_text(&site.index_html(), "#tests", "Verified frame captures");
    assert_text(&site.index_html(), "#tests", "14 passed");
}

#[test]
fn adds_gallery_entry_only_when_visual_hash_changes() {
    let same = update_gallery(
        &existing_gallery(),
        &report_with_hash("abc"),
        &commit_summary("same-source"),
    );
    assert_eq!(same.entries.len(), 1);

    let changed = update_gallery(
        &existing_gallery(),
        &report_with_hash("def"),
        &commit_summary("new-source"),
    );
    assert_eq!(changed.entries.len(), 2);
}

#[test]
fn comparison_page_includes_reference_provenance() {
    let html = build_fixture_site("verified-game").unwrap().index_html();
    assert!(html.contains("a30e12b63a36743015b1c73eeca6248"));
    assert!(html.contains("8a5a31e7bceb8ad16b3481d2bae89e7"));
}
```

- [ ] **Step 3: Run tests and confirm failure**

Run: `cargo test --test sitegen_contract`
Expected: FAIL because verification and gallery rendering do not exist.

- [ ] **Step 4: Parse sanitized verification summaries**

Define a strict public projection of internal reports. Include gate names,
status, counts, durations, semantic hashes, relative artifact paths, and metric
values. Exclude command lines, environment values, and local paths.

- [ ] **Step 5: Implement screenshot promotion**

On a green report:

- compare the semantic visual hash with the latest gallery entry;
- copy center, fault, repair, and browser frames only when the hash changes;
- write a stable gallery manifest in commit order;
- retain the current frame even when no new history entry is needed.

On failure, publish metric names and artifact links but retain previous public
screenshots.

- [ ] **Step 6: Render the test and comparison sections**

Show current-versus-reference images, worker comparison, metric deltas, latest
gate matrix, and links to GitHub Actions for full logs.

- [ ] **Step 7: Run the task gate**

Run:

```bash
cargo test --test sitegen_contract
cargo test --test pages_assembly_contract
cargo run --bin sitegen -- build \
  --inputs tests/fixtures/sitegen/verified-game/inputs.json \
  --output target/site-preview
```

Expected: gallery deduplication, report sanitization, comparisons, and test
tables pass.

- [ ] **Step 8: Commit and push**

```bash
git add docs/progress.json src/sitegen.rs src/bin/sitegen.rs \
  site/templates/index.html site/static/site.css site/static/site.js \
  tests/support/mod.rs tests/sitegen_contract.rs \
  tests/pages_assembly_contract.rs tests/fixtures/sitegen
git commit -m "feat: publish verification evidence and screenshots"
git push origin main
```

---

### Task 6: Complete status-always workflow orchestration

**Files:**
- Modify: `.github/workflows/pages.yml`
- Modify: `src/sitegen.rs`
- Modify: `tests/pages_assembly_contract.rs`
- Modify: `docs/progress.json`

**Interfaces:**
- Consumes: current source, optional previous `pages-live`, native result artifact, web result artifact, and generated current site.
- Produces: current-status deployment with atomic last-green game promotion.

- [ ] **Step 1: Write failing workflow-disposition tests**

```rust
#[test]
fn green_run_replaces_game_and_updates_last_green_commit() {
    let previous = fixture_site("previous-green");
    let current = fixture_site("current-green");
    let output = tempfile::tempdir().unwrap();
    let result = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_fixture("green"),
        output.path(),
    )
    .unwrap();
    assert_eq!(result, BuildDisposition::GreenReplacement);
    assert_eq!(
        read_last_green(output.path().join("last-green.json")).source_commit,
        "current-green-sha"
    );
}

#[test]
fn wasm_failure_updates_status_but_keeps_previous_game() {
    let previous = fixture_site("previous-green");
    let current = fixture_site("wasm-failed");
    let output = tempfile::tempdir().unwrap();
    let before = sha256(previous.path().join("play/game_bg.wasm"));
    let result = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_fixture("failed-web"),
        output.path(),
    )
    .unwrap();
    assert_eq!(result, BuildDisposition::FailedRetainLastGreen);
    assert_eq!(sha256(output.path().join("play/game_bg.wasm")), before);
    assert!(read(output.path().join("index.html")).contains("WASM BUILD FAILED"));
}
```

- [ ] **Step 2: Run tests and confirm failure**

Run: `cargo test --test pages_assembly_contract`
Expected: FAIL until green promotion and failed retention are complete.

- [ ] **Step 3: Split the workflow into three jobs**

1. **Verify:** progress validation, fmt, Clippy, asset freshness, Rust tests,
   rendered contract, release build, screenshots, and always-uploaded result
   manifest.
2. **Build web:** runs only after Verify succeeds; packages WASM, executes
   browser smoke, and uploads web result/site artifacts.
3. **Publish:** runs with `if: always()`; assembles against `pages-live`, pushes
   the generated branch, and deploys through Pages.

If an earlier step prevents normal manifest generation, an `if: always()` step
must synthesize a failed result with the failing step name.

- [ ] **Step 4: Add branch persistence and race protection**

Create `pages-live` when absent. Include `.nojekyll` and a generated-file
warning. Use `concurrency.group: pages` with `cancel-in-progress: false`. Push
with the workflow token. Trigger only from `main` and manual dispatch.

- [ ] **Step 5: Add workflow contract assertions**

Assert:

- Publish has both upstream jobs in `needs`;
- Publish uses `if: always()`;
- Build web requires Verify success;
- failed upstream results select retention;
- write permissions exist only in Publish;
- failure artifacts use `if: failure()`;
- generated branch pushes cannot trigger the workflow.

- [ ] **Step 6: Run the task gate**

Run:

```bash
cargo test --test pages_assembly_contract
cargo test --test sitegen_contract
git diff --check
```

Expected: all green/failure/first-run dispositions pass.

- [ ] **Step 7: Commit and push**

Mark Pages orchestration `done` with `"HEAD"` and update the next current task.

```bash
git add .github/workflows/pages.yml docs/progress.json src/sitegen.rs tests/pages_assembly_contract.rs
git commit -m "ci: retain the last green game on failed builds"
git push origin main
```

---

### Task 7: Integrate project-wide checks and documentation

**Files:**
- Modify: `scripts/check.sh`
- Modify: `README.md`
- Modify: `docs/progress.json`
- Modify: `docs/implementation-plan.md`
- Test: all Pages and project gates.

**Interfaces:**
- Consumes: all site, game, asset, verification, and web commands.
- Produces: one clean-push command and complete user-facing documentation.

- [ ] **Step 1: Add Pages gates to the canonical check script**

Run in this order:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo run --bin assetgen -- --check
cargo run --bin sitegen -- validate \
  --progress docs/progress.json \
  --plan docs/implementation-plan.md \
  --repository .
cargo test --all-targets --all-features
cargo test --test render_contract -- --nocapture
./scripts/build-web.sh target/web-check
./scripts/web-smoke.sh target/web-check
cargo build --release
```

Do not skip a gate when a dependency is missing. Print installation guidance
and fail.

- [ ] **Step 2: Document the progress protocol**

Update README with:

- Pages URL;
- current/last-green distinction;
- controls;
- local site preview;
- progress data schema;
- task-start and task-completion updates;
- challenge logging;
- asset and screenshot publication;
- full clean-push command.

Keep a generated status block delimited by:

```markdown
<!-- sitegen:status:start -->
<!-- sitegen:status:end -->
```

`sitegen check` must fail when this block differs from `progress.json`.

- [ ] **Step 3: Synchronize the reviewed overall plan**

Copy the current approved session plan to `docs/implementation-plan.md`. Add the
Pages milestones and push/deployment dependencies. Run plan-ID validation.

- [ ] **Step 4: Complete progress data**

Set the Pages tasks accurately:

- `done` for completed milestones with commit references;
- exactly one dependency-ready `in_progress` game or release task;
- remaining tasks `future`;
- open/resolved challenges with concrete impact and approach.

- [ ] **Step 5: Run the complete gate**

Run: `./scripts/check.sh`
Expected: native, asset, progress, site, render, WASM, browser, and release
checks pass without human inspection.

- [ ] **Step 6: Commit and push**

```bash
git add README.md docs/progress.json docs/implementation-plan.md scripts/check.sh
git commit -m "docs: publish the complete autonomous progress workflow"
git push origin main
```

- [ ] **Step 7: Verify the deployed state**

After the direct push to `main`, query GitHub for the Pages deployment
associated with the commit. Assert:

- deployment status is successful;
- published source commit matches `main`;
- current status is visible;
- playable commit equals the latest green build;
- concept reference assets return HTTP 200;
- browser-smoke report is green.

Record this result in `docs/progress.json` only if the task state changes; do
not add a manual approval step.

---

## Execution Order Relative to the Game Plan

```text
game foundation
      |
      v
Pages Tasks 1-3: status-only hub live
      |
      +--------------------+
      |                    |
game implementation   Pages Task 4 waits
      |                    |
playable game ready -------+
      |
      v
Pages Task 4: WASM game
      |
game verification ready
      |
      v
Pages Task 5: screenshots/tests
      |
      v
Pages Tasks 6-7: status-always release workflow
```

The status-only site ships early. Later Pages tasks resume when their named game
dependencies become green.
