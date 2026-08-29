# GitHub Pages Progress Hub Design

**Date:** August 29, 2026  
**Status:** Approved  
**Pages URL:** `https://ridermw.github.io/midcreek-cs-1/`

## Purpose

Publish a continuously updated record of the Cell Shift POC and a playable
WebAssembly build. The site must show what is complete, what is in progress,
what comes next, what has been difficult, which tests pass, and how the current
game compares with the approved cel-shift concept art.

The site updates after every push to `main`. Failed builds still publish current
status and diagnostics, but they must not replace the last known-good game or
screenshots.

## Goals

- Give the user a live, public view of development progress.
- Keep the playable browser build at the last verified green commit.
- Publish current screenshots beside the approved concept art.
- Show the reviewed plan and ASCII architecture diagrams.
- Keep completed, current, and future work accurate from one canonical source.
- Show open and resolved challenges with enough context to understand them.
- Publish native, render, asset, site, and WebAssembly gate results.
- Avoid npm, Blender, manual deployment, and subjective release gates.
- Commit and push every independently green increment.

## Non-goals

- Replace GitHub Actions as the full log archive.
- Publish a broken or unverified game build.
- Add a CMS, database, server-side application, or authentication.
- Duplicate task status across README files, HTML, and workflow configuration.
- Store secrets, internal paths, or unfiltered command output on the public site.

## Chosen Approach

Use a repository-owned Rust site generator and a generated `pages-live` branch.

```text
main push
   |
   v
+--------------------- GitHub Actions ----------------------+
|                                                          |
|  native gates       rendered gates       WASM gates      |
|       |                   |                  |            |
|       +-------------------+------------------+            |
|                           |                               |
|                    result manifest                        |
+---------------------------+-------------------------------+
                            |
                            v
                  checkout pages-live
                            |
              +-------------+-------------+
              |                           |
       always regenerate             replace only if
       current status/docs           every gate is green
       plans/challenges/tests        game/screenshots
              |                           |
              +-------------+-------------+
                            |
                            v
                  official Pages deploy
```

The workflow deploys through the official Pages actions. The `pages-live`
branch persists the last green game and screenshot set between workflow runs.
The workflow only triggers for pushes to `main` and manual dispatches, so pushes
to `pages-live` cannot recurse.

## Source-of-truth Model

### Canonical progress data

`docs/progress.json` is the sole structured source for planned work and
challenges.

```json
{
  "schema_version": 1,
  "project": "Cell Shift Data Center POC",
  "tasks": [
    {
      "id": "foundation-contracts",
      "title": "Establish reviewed contracts",
      "status": "done",
      "depends_on": [],
      "summary": "Pinned the project and defined the visual and gameplay contracts.",
      "completed_commit": "HEAD"
    },
    {
      "id": "autonomous-assets",
      "title": "Generate autonomous game assets",
      "status": "in_progress",
      "depends_on": ["foundation-contracts"],
      "summary": "Building the RON-to-GLB generator.",
      "completed_commit": null
    }
  ],
  "challenges": [
    {
      "id": "cross-gpu-rendering",
      "title": "Cross-GPU render variance",
      "status": "open",
      "impact": "Raw PNG hashes differ across Metal and Vulkan.",
      "approach": "Gate semantic state and bounded image metrics instead.",
      "resolution": null
    }
  ]
}
```

Allowed task states are `future`, `in_progress`, and `done`.

The validator enforces these rules:

- Exactly one task is `in_progress`, unless every task is `done`.
- A task may start only after every dependency is `done`.
- Every `done` task has an existing full commit SHA or the exact sentinel
  `HEAD`. CI resolves `HEAD` to the workflow commit before rendering the site.
- `future` and `in_progress` tasks have no completion SHA.
- Task IDs are unique and match the reviewed implementation plan.
- Every challenge includes impact and approach.
- Resolved challenges include a resolution and an existing commit SHA or
  `HEAD`.
- Unknown fields fail validation instead of being ignored.

### Other authoritative inputs

- `docs/implementation-plan.md` contains the reviewed plan and ASCII diagrams.
- `docs/reference/` contains the two approved cel-shift images and provenance.
- Verification JSON contains gameplay, asset, camera, UI, and render metrics.
- Git history supplies the commit timeline.
- Workflow job results supply current gate status.

The HTML site, README status block, task columns, test tables, and challenge
cards are generated views. They are never edited directly.

## Site Generator

Add a Rust binary named `sitegen`.

```text
progress.json -----------+
implementation-plan.md --+
reference manifest ------+--> validate --> render --> site output
verification report -----+
workflow result JSON ----+
git log JSON ------------+
```

`sitegen build` writes the site to a requested output directory.
`sitegen check` builds into a temporary directory and validates all source
links, image references, task relationships, alt text, headings, and generated
data. The generator uses stable ordering and emits no host paths.

The generator must escape all Markdown, JSON, Git, and test-derived content
before inserting it into HTML. The Pages workflow processes only trusted
`main` content and never deploys pull-request code.

## Site Structure

### Header

Show:

- latest source commit;
- latest workflow result;
- last green playable-game commit;
- current task;
- latest update time from the Git commit, not a manually entered timestamp.

### Play

Embed the latest green Bevy WASM build in a responsive 16:9 canvas. Show Arrow,
Q/E, and Space controls next to the canvas. Prevent browser scrolling for those
keys while the canvas has focus.

If the current source commit failed verification, show a clear banner:

```text
CURRENT SOURCE: FAILED AT <short SHA>
PLAYABLE BUILD: LAST GREEN AT <short SHA>
```

The banner links to the corresponding GitHub Actions run.

### Visual comparison

Publish:

- approved cel-shift key art beside the current game frame;
- a draggable comparison slider;
- the character sheet beside the current worker crop;
- exact source paths and SHA-256 values;
- current deterministic visual metrics.

The site copies concept art from:

- `midcreek-concept/themes/cel-shift/masters/key-art/04-diamond-bright.png`
- `midcreek-concept/themes/cel-shift/masters/animation/01-model-sheet.png`

The files become repository-owned references before the workflow uses them.
The Pages build never depends on a sibling checkout.

### Progress

Render three columns from `progress.json`:

```text
+----------------+----------------+----------------+
| DONE           | WORKING NOW    | FUTURE         |
|                |                |                |
| completed task | exactly one    | dependency-    |
| + commit link  | active task    | ordered tasks  |
+----------------+----------------+----------------+
```

Each task links to its plan section. Completed tasks link to their commit.

### Screenshot history

Add a gallery entry only when the successful render report's semantic visual
hash differs from the latest gallery entry. This prevents documentation-only
pushes from duplicating screenshots.

Each entry shows:

- source commit and date;
- current task;
- center frame;
- ticket/repair frame when available;
- visual metric deltas from the previous entry;
- links to the full verification report.

### Plans and diagrams

Render `docs/implementation-plan.md` as HTML. Preserve fenced ASCII diagrams in
`<pre>` elements with horizontal scrolling. Link each progress task to its
matching heading.

### Challenges

Render open challenges before resolved challenges. Each card shows impact,
current approach, resolution when present, and relevant commits.

### Tests

Show the latest gate matrix:

- formatting;
- Clippy;
- autonomous asset regeneration;
- Rust unit tests;
- app contracts;
- rendered-image contracts;
- WASM compilation;
- browser-ready smoke test;
- release build.

Display counts and durations from generated reports. Link full logs to GitHub
Actions rather than publishing unfiltered logs.

### Commit timeline

Render recent `main` commits with short SHA, subject, date, and task association.
Do not duplicate commit messages in `progress.json`.

## WebAssembly Build

The game remains one codebase. Target-specific configuration disables native
verification process control in WASM but retains the production game plugins.

```text
cargo build --release --target wasm32-unknown-unknown
        |
        v
pinned wasm-bindgen --target web
        |
        v
game JS + WASM + generated GLBs
        |
        v
site/play/index.html
```

The WASM-only `WebReadyPlugin` waits until assets and the gameplay state are
ready, then waits through two `PostUpdate` frames before asking the browser
bootstrap to set:

- `data-game-state="loading"` before instantiation;
- `data-game-state="ready"` after the two-frame readiness handshake;
- `data-game-state="error"` with a concise message on failure.

The build pins the Rust target and `wasm-bindgen-cli` version. The local and CI
gates compare the CLI and crate versions before packaging.

The bootstrap installs `window.onerror` and `unhandledrejection` handlers that
append sanitized messages to a hidden `#browser-errors` element. The browser
gate checks that element and separately analyzes the canvas region in its
screenshot. Readiness and visible rendering therefore have independent proofs.

## Browser Gate

Serve the generated site from a loopback HTTP server and launch headless
Chromium without npm.

```text
site output -> loopback server -> headless Chromium
                                  |
                                  +-> dump DOM
                                  +-> screenshot
                                  +-> console/error capture
```

The gate fails unless:

- the main page returns HTTP 200;
- all local links and required assets return HTTP 200;
- the playable page reaches `data-game-state="ready"` within 30 seconds;
- the canvas has nonzero dimensions and 16:9 aspect within one pixel;
- `#browser-errors` is empty, proving no captured error or unhandled rejection;
- Arrow, Q/E, and Space do not scroll the focused page;
- the current and last-green commit labels are present;
- concept and current screenshots decode;
- Done, Working Now, Future, Challenges, Tests, Plans, and Commits appear;
- the browser screenshot is nonblank, and the canvas region contains at least
  three palette classes and sufficient pixel variance.

## Workflow

Use one Pages workflow with three jobs.

### 1. Verify

- Validate `progress.json`.
- Run native, asset, app, render, and release gates.
- Generate current screenshots and reports.
- Upload a result manifest from an `if: always()` step. If an earlier command
  prevents normal report generation, synthesize the manifest from step
  outcomes and include the failing step name.

### 2. Build web

- Run only after native verification passes.
- Install the pinned WASM target and `wasm-bindgen-cli`.
- Build and package the browser game.
- Run the headless Chromium gate.
- Upload the green site/game artifact.

### 3. Publish

Run with `if: always()`.

1. Check out `pages-live` into the output directory. If the branch does not
   exist, initialize a clean generated site with `.nojekyll`.
2. Replace status, plan, challenge, test, and commit pages from the current
   `main` commit.
3. If Verify and Build web passed, replace the playable game, current
   screenshots, and last-green metadata.
4. If either failed, retain the previous game and screenshots and publish the
   new failure banner and diagnostics summary. On a first-run failure, publish
   a status-only page whose Play section says that no verified build exists.
5. Push the generated `pages-live` branch.
6. Upload the directory with `actions/upload-pages-artifact`.
7. Deploy it with `actions/deploy-pages`.

Set workflow concurrency to one non-canceling Pages deployment. A newer push
waits for the active publish instead of racing it.

Grant write permissions only to the Publish job. Verify and Build web use
read-only contents permissions.

Publish permissions:

```yaml
permissions:
  contents: write
  pages: write
  id-token: write
  actions: read
```

## Commit and Push Policy

This unattended hill climb works directly on `main`. It does not depend on pull
requests or human review. Before each push, confirm the current branch is
`main`.

The project uses frequent, green checkpoints:

1. Mark a task `in_progress`, validate progress data, commit, and push.
2. Implement one independently testable increment.
3. Run its targeted gate and the largest practical clean gate.
4. Update task summary, challenges, plans, diagrams, and screenshots when
   affected.
5. Commit and push immediately.
6. Mark the task `done` only after its complete gate passes.

Every push must build cleanly. Visual pushes must include a new verification
report; the Pages workflow decides whether the visual hash warrants a gallery
entry.

## Failure Handling

| Failure | Published status | Playable game |
|---|---|---|
| Progress data invalid | Workflow failure with validator message | Previous green build retained |
| Native tests fail | Current commit and failed gate shown | Previous green build retained |
| Render contract fails | Failed metrics and artifact links shown | Previous green build retained |
| WASM compile fails | Web failure shown | Previous green build retained |
| Browser smoke fails | Browser error and screenshot link shown | Previous green build retained |
| Site generation fails | Workflow fails before deployment | Existing Pages deployment remains |
| Pages deployment fails | GitHub records deployment failure | Existing Pages deployment remains |

No failure path replaces a working game with an unverified build.

## Testing

### Unit tests

- Progress schema and dependency validation
- Challenge validation
- Markdown and HTML escaping
- Stable task ordering
- Semantic visual hash comparison
- Last-green/current metadata merge
- WASM version compatibility

### Integration tests

- Site generation from complete fixtures
- Site generation from failing-build fixtures
- Missing previous `pages-live` bootstrap
- Last-green game replacement on success
- Last-green game retention on failure
- Broken links, missing images, and invalid task IDs

### End-to-end tests

- Native/render workflow manifest generation
- WASM build and packaging
- Loopback server and headless Chromium readiness
- Pages artifact contains every required section and asset
- Failure-status deployment retains the previous game

## Security and Privacy

- Deploy only trusted pushes to `main`.
- Escape all generated content.
- Publish summaries, not raw environment logs.
- Never expose absolute local paths, tokens, environment variables, or GitHub
  event payloads.
- Use the default GitHub token with the minimum listed permissions.
- Copy only the two explicitly approved concept images.
- Pin action major versions and tool versions.

## Acceptance Criteria

- Every `main` push triggers the Pages workflow.
- The site reports the current commit even when its gates fail.
- A failed run retains the last verified playable game and screenshots.
- A green run publishes the new WASM game and verification images.
- The comparison section shows both approved cel-shift references.
- Done, Working Now, and Future derive from one validated file.
- Plans, ASCII diagrams, challenges, test results, and commits are visible.
- Headless Chromium proves the game reaches its first rendered frame.
- No npm, Blender, manual deployment, or human approval gate exists.
