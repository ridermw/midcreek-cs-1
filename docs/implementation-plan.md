# Cell Shift Data Center POC - Reviewed Autonomous Implementation Plan

> **Execution rule:** Use `subagent-driven-development` or `executing-plans`, implement one task at a time, commit and push directly to `main` after every independently green increment, and never pause for human visual approval. Every gate below is executable and deterministic.

## Goal

Build a polished Bevy 0.19.1 proof of concept for the larger Midcreek game:

- A cel-shift data-center technician walks through one fixed-layout data hall.
- Arrow keys move relative to the current camera view.
- Q/E smoothly orbit the orthographic camera through four 90-degree diamond views.
- A seeded recurring fault scheduler creates up to three simultaneous prioritized tickets.
- The player walks to a faulted rack and presses Space to repair it.
- Repair locks movement, plays a repair animation, updates badges/HUD, resolves the ticket, and returns the rack to the recurring fault pool after cooldown.
- All room, equipment, technician, rig, and animation assets are generated autonomously from repository-owned declarative source. Blender is never used.
- Pure state contracts, real input integration, generated-asset checks, a scripted real-app journey, and rendered-image analysis decide whether each hill-climb step passes.
- GitHub Pages continuously publishes current progress, the playable WASM game, concept-art comparisons, screenshots, plans, ASCII diagrams, challenges, tests, and commit history.

## Source of Truth

- `/Users/mattheww/git/midcreek-concept/ART-BIBLE.md`
- `/Users/mattheww/git/midcreek-concept/themes/_shared/foundation.md`
- `/Users/mattheww/git/midcreek-concept/themes/cel-shift/theme.yaml`
- `/Users/mattheww/git/midcreek-concept/themes/cel-shift/prompts/key-art-diamond.mock.md`
- `/Users/mattheww/git/midcreek-concept/themes/cel-shift/prompts/animation-sheet.mock.md`
- `docs/superpowers/specs/2026-08-29-github-pages-progress-hub-design.md`
- `docs/superpowers/plans/2026-08-29-github-pages-progress-hub.md`
- Key-art SHA-256: `a30e12b63a36743015b1c73eeca6248a8b8ee974cf007f23666dc101f06c0e75`
- Character-sheet SHA-256: `8a5a31e7bceb8ad16b3481d2bae89e7a32bb4edd0ef711b7d07a26f177cf6b25`

## What Already Exists

- `main` contains the initial repository commit plus the approved Pages design and implementation plan through commit `504efdb`.
- Rust 1.98.0 and Cargo 1.98.0 are installed.
- The concept repository already provides the approved camera, palette, equipment identity, UI language, worker design, and animation poses. This plan vendors only the two approved reference images and reuses those decisions.
- Bevy 0.19.1 supplies orthographic cameras, glTF loading, skeletal animation, UI layout, and `Screenshot::primary_window().observe(save_to_disk(...))`; the plan uses those APIs rather than creating substitutes.
- No prior implementation or review-driven refactor exists.

## Locked Review Decisions

- Preserve the expanded POC scope: orbit, autonomous authored assets, and recurring multi-ticket repair gameplay are mandatory.
- Use as many cohesive Bevy plugins as the POC needs; do not create trivial one-system plugins.
- Keep visual and collider specifications separate, joined by stable `PropId`, with mandatory validation.
- Verification injects the real `ButtonInput<KeyCode>` path.
- Use exact semantic-report hashes plus mandatory bounded image metrics; do not require cross-GPU PNG byte equality.
- Use typed `PaletteRole`/`Srgba` constants, not runtime hex parsing.
- Keep ordered visual/collider lists and validate duplicate, missing, and orphan IDs.
- Keep explicit `PlayerParts` handles and reset rig nodes deliberately on idle/repair transitions.
- Drive verification through an explicit `VerificationStage` state machine.
- Cache unit meshes and palette materials once.
- Generate combined static rack/equipment meshes rather than one entity per server slot or LED.
- Compute every image's metrics in one pass and cache reference metrics once.
- Extract colliders once and linearly scan the cached vector; no spatial index for one room.
- Q/E perform eased 90-degree orbit transitions.
- Generate assets with Rust from declarative RON source; no Blender, manual export, or third-party art pipeline.
- Use a seeded recurring queue with multiple simultaneous tickets and deterministic priority ordering.
- Work directly on `main`; no pull request or human review is required during this unattended hill climb.
- Run the largest available clean gate before every push, then push immediately so progress stays visible.
- Publish a status-only Pages hub after project foundation and before visual game work.
- Publish current status on every `main` push, but replace the playable WASM game and screenshots only after all associated gates pass.
- Keep Done, Working Now, Future, and Challenges in one validated `docs/progress.json` source.

## Architecture

```text
                         CellShiftPlugin
                               |
          +--------------------+--------------------+
          |                    |                    |
       AssetPlugin          HallPlugin        TechnicianPlugin
          |                    |                    |
 RON source -> assetgen     SceneBlueprint      ButtonInput
          |               visuals + colliders       |
 generated GLBs                 |             movement/collision
          |                     |                    |
          +-----------> loaded scene handles <-------+
                               |
                +--------------+--------------+
                |                             |
           CameraPlugin                 OperationsPlugin
       follow + Q/E orbit          seeded faults + tickets
                |                   repair interaction/state
                +--------------+--------------+
                               |
                            HudPlugin
                  queue + meters + badges + controls
                               |
                    VerificationPlugin (test mode)
                   stages -> frames + semantic report
                               |
             asset/state/input/render contracts -> pass/fail
```

### Progress publication

```text
green increment -> update progress/challenges -> commit -> push main
                                                        |
                                                        v
                                                   Pages workflow
                             +--------------------------+------------------+
                             |                                             |
                     always publish current                       replace only if green
                     status/plan/tests                            WASM game/screenshots
                             |                                             |
                             +--------------------------+------------------+
                                                        |
                                                        v
                                                   GitHub Pages
```

The detailed Pages design and seven-task implementation plan live in the two
repository documents listed under Source of Truth. They are authoritative for
site generation, WASM packaging, last-green retention, and browser testing.

### Runtime data flow

```text
ButtonInput<KeyCode>
   |
   +-- Arrow keys ------------------------------+
   |                                            v
   |                                  camera-relative direction
   |                                            |
   |                               cached linear collider scan
   |                                            |
   |                                     player transform
   |                                            |
   +-- Q/E -> desired heading -> eased orbit ---+--> clamped camera
   |
   +-- Space -> in-range ticket? -- no --> unchanged
                                  |
                                 yes
                                  v
             TicketOpen -> Repairing -> Resolved -> Cooldown -> Healthy
                              |             |
                       movement locked   ticket removed
                       repair clip       rack returns later
```

### Asset pipeline

```text
assets/source/*.ron
        |
        v
schema + invariant validation
        |
        v
primitive tessellation + rigid bone weights + animation tracks
        |
        v
merge static geometry by asset/material/bone
        |
        v
deterministic GLB writer (stable ordering, no timestamps/absolute paths)
        |
        +--> --write -> assets/generated/*.glb
        |
        +--> --check -> temp generation -> byte compare -> pass/fail
```

### Plugin scheduling

Use explicit system sets in `src/lib.rs`:

```text
AssetReady
   -> SpawnWorld
   -> ReadInput
   -> UpdateOrbitIntent
   -> UpdateOperations
   -> MovePlayer
   -> UpdateAnimation
   -> FollowCamera
   -> UpdateHudAndBadges
   -> VerificationProbe
```

Each plugin registers systems into these shared sets. Startup and update ordering must not depend on plugin insertion order.

## Fixed Product Contract

- **Window:** 1280x720 default; 960x540 verification resize.
- **Room:** 40m x 40m, polished light concrete, low perimeter walls.
- **Layout:** four parallel rack rows, three traversable aisles, cooling units, overhead trays, black hose drops, yellow floor markings, red cart, yellow stool.
- **Projection:** orthographic 26m x 14.625m, 57-degree elevation, four headings separated by 90 degrees, initial yaw 45 degrees.
- **Camera offset direction:** normalized `(1, 2.1776979, 1)` because `sqrt(2) * tan(57 degrees) = 2.1776979`.
- **Camera orbit:** Q counter-clockwise, E clockwise, 0.30-second smoothstep quarter turn; opposite keys on one frame cancel.
- **Camera follow:** fixed zoom/elevation, clamped from the current yaw's ground-plane frustum footprint.
- **Movement:** arrow keys, screen-relative to the live interpolated camera basis, normalized diagonals, no wall/equipment penetration.
- **Interaction:** Space starts repair only for the highest-priority in-range ticket within 1.5m.
- **Fault queue:** deterministic seed, maximum three active tickets, no duplicate active ticket per rack.
- **Priority:** Critical before Warning, then creation tick, then stable rack ID.
- **Timing:** new fault opportunity every 4 seconds while under capacity; repair duration 3 seconds; resolved display 2 seconds; rack cooldown 8 seconds.
- **Repair behavior:** movement disabled during repair; camera remains active; repair animation plays; completion is automatic.
- **Technician:** adult proportions; blue hard hat, hi-vis vest, slate shirt, dark trousers, brown boots, tool belt; Idle, Walk, and Repair clips.
- **Style:** typed cel-shift palette, unlit base/shadow materials, explicit hard shadow geometry, dark outlines, no gradients, grunge, bloom, depth of field, or texture noise.
- **UI:** top-left prioritized ticket queue/status stack, floating red fault and blue wrench badges, bottom-right Arrow/Q/E/Space controls; no minimap, inventory, or build toolbar.

### Typed palette

Define `PaletteRole` and const `Srgba` values for:

- Rack white `#FBFCFD`
- Rack shadow `#C6D5E0`
- Floor light `#DEE6EB`
- Floor shadow `#B2C0CB`
- Signature yellow `#FFC93C`
- Teal accent `#2FB8A8`
- Hose charcoal `#2E353B`
- Ink `#1F2A33`
- Sky-bounce blue `#9FD0F0`
- Healthy green `#4ADE80`
- Fault red `#FF4B4B`
- Worker hi-vis `#C8D94A`
- Worker slate `#55707F`
- Worker trousers `#2F3A42`
- Worker boots `#7A5233`
- Worker hard hat `#2C6FB8`
- Worker skin `#C98F6A`

Only one material handle per `PaletteRole` may be created at runtime.

## Planned Repository Shape

- `.gitignore`
- `rust-toolchain.toml`
- `Cargo.toml`
- `Cargo.lock`
- `README.md`
- `src/main.rs`
- `src/lib.rs`
- `src/design.rs` — typed palette, IDs, visual/collider specs, scene blueprint, validators.
- `src/assetgen.rs` — RON schemas, mesh/skin/animation generation, deterministic GLB writer.
- `src/bin/assetgen.rs` — thin `--write`/`--check` CLI.
- `src/assets.rs` — generated-asset handles, load-state validation, shared `RenderAssets`.
- `src/world.rs` — hall spawning from validated visual specs.
- `src/player.rs` — real keyboard movement, collision, rig discovery, animation state.
- `src/camera.rs` — four headings, tweening, frustum clamp, follow.
- `src/operations.rs` — seeded fault scheduler, tickets, repair state machine.
- `src/hud.rs` — ticket stack, badges, controls, status visualization.
- `src/verification.rs` — explicit verification stages, screenshots, semantic report, watchdog.
- `src/sitegen.rs` — canonical progress validation, static-site rendering, gallery history, and last-green assembly.
- `src/bin/sitegen.rs` — `validate`, `build`, and `assemble` CLI.
- `src/web.rs` — WASM-only browser readiness bridge.
- `assets/source/technician.ron`
- `assets/source/rack.ron`
- `assets/source/cooling-unit.ron`
- `assets/source/utility-props.ron`
- `assets/source/infrastructure.ron`
- `assets/generated/technician.glb`
- `assets/generated/rack.glb`
- `assets/generated/cooling-unit.glb`
- `assets/generated/utility-props.glb`
- `assets/generated/infrastructure.glb`
- `tests/asset_contract.rs`
- `tests/app_contract.rs`
- `tests/render_contract.rs`
- `tests/sitegen_contract.rs`
- `tests/pages_assembly_contract.rs`
- `tests/support/mod.rs`
- `scripts/check.sh`
- `scripts/build-web.sh`
- `scripts/web-smoke.sh`
- `.github/workflows/ci.yml`
- `.github/workflows/pages.yml`
- `docs/progress.json`
- `docs/implementation-plan.md`
- `docs/reference/cel-shift-key-art.png`
- `docs/reference/cel-shift-character-sheet.png`
- `docs/reference/manifest.json`
- `docs/verification/v0-center.png`
- `docs/verification/v0-report.json`
- `site/templates/index.html`
- `site/templates/play.html`
- `site/static/site.css`
- `site/static/site.js`
- `site/static/play.js`

This is intentionally larger than the walking-only draft because the user explicitly moved orbit, the asset pipeline, and recurring repair gameplay into the POC. Files remain cohesive; no class/plugin count cap may reduce visual quality.

## Continuous Publication Protocol

After Task 1 establishes the Rust project, execute Pages-plan Tasks 1-3 before
starting autonomous asset work. This publishes the status-only hub, reviewed
plans, diagrams, and cel-shift references early.

For every later increment:

1. Confirm `git branch --show-current` returns `main`.
2. Update `docs/progress.json` so exactly one dependency-ready task is
   `in_progress`.
3. Update challenge records, the implementation plan, and nearby ASCII diagrams
   when the increment changes them.
4. Run the targeted tests, asset/progress freshness checks, and largest
   available clean gate.
5. Commit with the required Copilot co-author trailer.
6. Push directly with `git push origin main`.
7. Let Pages publish current status. Promote new game/screenshots only when
   native, render, WASM, and browser gates are green.

Cross-plan ordering:

```text
Game Task 1
   |
   v
Pages Tasks 1-3: status-only hub live
   |
   v
Game Tasks 2-7
   |
   +--------------------+
   |                    |
   v                    v
Pages Task 4       Game Task 8
playable WASM      verification
   |                    |
   +----------+---------+
              v
       Pages Tasks 5-6
       evidence + status-always
              |
              v
          Game Task 9
              |
              v
         Pages Task 7
         final published baseline
```

## Core Types and Invariants

### Scene specification

```rust
struct PropId(String);

struct VisualSpec {
    id: PropId,
    asset: AssetKind,
    transform: TransformSpec,
    collision_required: bool,
}

struct ColliderSpec {
    id: PropId,
    center: Vec2,
    half_extents: Vec2,
}

struct SceneBlueprint {
    room: RoomSpec,
    visuals: Vec<VisualSpec>,
    colliders: Vec<ColliderSpec>,
    player_spawn: Vec2,
}
```

`SceneBlueprint::validate()` returns all errors at once:

- duplicate visual IDs;
- duplicate collider IDs;
- required collider missing;
- orphan collider;
- collider outside room;
- player spawn outside room or inside collider;
- fewer/more than four rack rows or three aisles;
- blocked aisle topology;
- camera target interval empty for any settled or mid-orbit heading.

### Operations state machine

Keep this diagram in `src/operations.rs` and update it with any state change:

```text
Healthy
   |
   | seeded scheduler; capacity available
   v
Faulted + TicketOpen
   |
   | player in range + Space just_pressed
   v
Repairing (3 s, movement locked, blue wrench badge)
   |
   | timer complete
   v
Resolved (2 s, healthy indicator)
   |
   | display timer complete
   v
Cooldown (8 s, no active ticket)
   |
   | cooldown complete
   v
Healthy and eligible again
```

### Camera clamp

Keep this diagram in `src/camera.rs`:

```text
current interpolated yaw + fixed ortho rectangle
                     |
                     v
cast viewport corners onto Y=0
                     |
                     v
ground quadrilateral -> X/Z extents
                     |
                     v
room bounds minus extents = legal target rectangle
                     |
                     v
clamp followed player -> derive camera transform
```

### Verification state machine

Keep the complete transition table and this summary in `src/verification.rs`:

```text
Boot
 -> WaitForAssets
 -> ValidateBlueprint
 -> HealthyCapture
 -> SeedThreeFaults
 -> FaultQueueCapture
 -> KeyboardJourney
 -> WalkCapture
 -> BeginRepair
 -> RepairCapture
 -> CompleteRepair
 -> ResolvedCapture
 -> OrbitNE/SE/SW/NW Captures
 -> MidOrbitCapture
 -> CornerProbes
 -> LowResolutionCapture
 -> AnalyzeReady
 -> WriteReport
 -> Success

Any invalid transition, missing entity, asset failure, capture failure, or
watchdog expiry -> Failure -> AppExit::error()
```

## Deterministic Test Diagram

```text
asset source
├── parse error ----------------------------> explicit assetgen failure
├── invariant error ------------------------> all schema errors reported
└── valid -> generate twice -> byte equal? -+-> inspect GLB contract
                                             |
real ButtonInput<KeyCode>                    |
├── Arrow combinations x 4 camera headings  |
├── Q/E/or opposing orbit keys               |
└── Space in/out of repair range             |
        |                                    |
        v                                    v
movement -> collision -> rig animation -> operations state
   |           |             |                 |
   |           |             |                 +-> queue capacity/priority/recurrence
   |           |             +-> idle/walk/repair/reset
   |           +-> wall/rack/slide/reachability
   +-> cardinal/diagonal/current-camera basis
        |
        v
camera follow + orbit -> center/corners/mid-tween x resolutions
        |
        v
14 real screenshots + canonical semantic report
        |
        +-> dimensions / sentinel / palette / luminance
        +-> diagonal edges in all four views
        +-> worker identity and animation deltas
        +-> red fault + blue repair badges
        +-> queue/HUD layout and state colors
        +-> reference histogram relationship
        |
        v
all mandatory thresholds pass -> continue
any failure -> metric + expected bound + artifact path -> iterate
```

No LLM or prompt file is modified, so no model eval suite applies.

## Objective Gates

No gate may require a person to play, resize, inspect, score, or approve anything.

| Requirement | Objective proof |
|---|---|
| Generated assets are current | `assetgen --check` regenerates in a temp directory and byte-compares every GLB. |
| Asset identity | GLB parser asserts node/material/mesh/joint/animation names and counts. |
| Rig works | Bevy loads technician, discovers required named bones, and plays Idle/Walk/Repair. |
| Hall identity | Exact scene specs and rendered color/edge/category metrics. |
| Separate visuals/colliders stay aligned | Stable `PropId` join validator with duplicate/missing/orphan tests. |
| Every aisle is usable | Grid flood-fill plus real-arrow-key waypoint traversal through all three aisles. |
| Controls remain screen-relative | Real `ButtonInput<KeyCode>` table across all four headings and mid-orbit samples. |
| Orbit is correct | Heading/tween math, duration, cancellation, current-basis movement, and all-view screenshots. |
| Camera never leaks outside | Frustum math plus magenta sentinel ratio at corners and mid-orbit. |
| Ticket queue is deterministic | Fixed seed produces exact rack/severity/tick sequence. |
| Multi-ticket behavior works | Capacity, no duplicates, priority order, resolve/cooldown/recurrence tests. |
| Repair interaction works | Out-of-range Space is rejected; in-range Space locks movement, plays Repair, resolves automatically. |
| HUD/badges match state | ECS layout/state contracts plus pixel-presence checks for red/blue/green states. |
| Visual gate is meaningful | Generated bad-image fixtures must fail the specific analyzer metric they target. |
| Verification cannot false-pass | Every legal/illegal stage transition and timeout/callback/report error is fault-injected. |
| Current semantic state is reproducible | Canonical report excludes time/absolute paths, sorts maps, rounds floats to 1e-6, and hashes identically. |
| Progress status is accurate | `sitegen validate` enforces one current task, completed dependencies, valid commit references, plan IDs, and complete challenge context. |
| Published plans and diagrams are current | `sitegen check` parses `docs/implementation-plan.md`, validates progress links, and rejects generated-source drift. |
| Browser game is playable | Pinned WASM build plus headless Chromium requires `data-game-state="ready"`, a nonblank canvas, working control-key suppression, and no captured browser errors. |
| Failed pushes preserve a working game | Assembly fixtures prove failed native/render/WASM runs update status while retaining prior game and screenshot hashes. |
| Concept comparisons are authentic | Published reference files must match approved source SHA-256 values and expose their provenance. |
| Pages deployment succeeds | GitHub's Pages deployment API must report success for the pushed `main` commit. |

## Failure Registry

| Failure | Test coverage | Runtime handling | Visibility |
|---|---|---|---|
| Source asset malformed | Parser fixture | `assetgen` returns nonzero with file/path field | Explicit |
| Generated GLB stale | Double generation and `--check` | Build gate stops | Explicit |
| Required GLB/node/animation missing | Asset contract + real-app load | Enter `AssetLoadState::Failed`, verification exits error | Explicit |
| Duplicate/orphan `PropId` | Validator tests | World never spawns | Explicit |
| Aisle accidentally blocked | Flood-fill + waypoint route | Contract fails before acceptance | Explicit |
| Arrow mapping wrong after orbit | 4-heading real-input matrix | Contract names heading/key/actual vector | Explicit |
| Orbit interrupted/opposite keys | Tween branch tests | Defined cancellation/retarget behavior | Explicit |
| Mid-orbit camera exposes background | Sentinel mid-tween frame | Render contract fails | Explicit |
| Duplicate rack ticket | Seeded scheduler tests | Scheduler rejects invariant and reports rack | Explicit |
| Queue exceeds three | Capacity tests | No new fault emitted | Observable in report |
| Repair begins out of range | Interaction test | Input ignored and reason recorded in verification report | Observable |
| Rig part handle stale | Fault-injection test | Animation system reports missing named part; verification fails | Explicit |
| HUD layout not ready | Stage transition test | Capture waits; watchdog identifies stage | Explicit |
| Screenshot callback lost | Fault-injection test | App and parent timeouts fail | Explicit |
| Analyzer accidentally accepts bad frame | Generated negative fixtures | Unit test fails | Explicit |
| Renderer unavailable | End-to-end child test | Capture stderr/nonzero; no skip | Explicit |
| Output path unsafe/unwritable | CLI tests | Write only named files; never recursive-delete; nonzero error | Explicit |
| Progress data contradicts the plan | Sitegen validator fixtures | Pages build stops before publishing false status | Explicit |
| WASM compiles but never renders | Headless Chromium readiness and canvas analysis | Current failure published; last green game retained | Explicit |
| Current build fails after a green deployment | Pages assembly success/failure fixtures | Status updates; previous game/screenshots remain | Explicit |
| First Pages run fails | First-run assembly fixture | Status-only page states no verified game exists | Explicit |
| Pages deployment races a newer push | Workflow contract | Non-canceling single concurrency group serializes deploys | Explicit |

There are no remaining silent failures with neither a test nor error handling.

---

## Task 1: Establish the project and reviewed contracts

**Files:** `.gitignore`, `rust-toolchain.toml`, `Cargo.toml`, `src/main.rs`, `src/lib.rs`, `src/design.rs`, `README.md`, reference PNGs, `tests/app_contract.rs`.

1. Rename `readme.md` with `git mv readme.md README.md`.
2. Pin Rust 1.98.0 with rustfmt and clippy.
3. Add Bevy 0.19.1 with 3D/UI/glTF/PNG support plus serialization, deterministic RNG, GLB-writing, hashing, image-analysis, and GLB-inspection dependencies; lock exact versions in `Cargo.lock`.
4. Add `.gitignore` for `/target`, temporary verification output, and macOS metadata.
5. Vendor the two approved reference images and assert their SHA-256 values.
6. Implement typed `PaletteRole`, camera/room/timing constants, `PropId`, ordered `VisualSpec`/`ColliderSpec`, and aggregate `SceneBlueprint::validate()`.
7. Add validator tests for every error branch, including duplicate/missing/orphan IDs and invalid camera room geometry.
8. Add `CellShiftPlugin` and shared ordered system sets.

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test design
```

Expected: all palette/reference/blueprint branches pass; malformed fixtures fail with exact variants.

Commit: `chore: establish reviewed cell shift contracts`

## Pages Milestone A: Publish the status-only progress hub

Execute Tasks 1-3 from
`docs/superpowers/plans/2026-08-29-github-pages-progress-hub.md` before starting
game asset work.

This milestone must:

- create and validate `docs/progress.json`;
- copy the reviewed overall plan into the repository;
- publish both approved cel-shift references with hashes;
- render Done, Working Now, Future, Plans, Diagrams, Challenges, Tests, and
  Commits;
- create the `pages-live` persistence branch;
- deploy a status-only Pages site that clearly states no verified game exists
  yet.

Run the Pages-plan gates, commit each green increment, and push each commit
directly to `main`. Do not proceed to Task 2 until GitHub's Pages deployment API
reports success for the current `main` commit.

## Task 2: Build the autonomous no-Blender asset pipeline

**Files:** `src/assetgen.rs`, `src/bin/assetgen.rs`, `assets/source/*.ron`, `assets/generated/*.glb`, `tests/asset_contract.rs`.

1. Write failing schema, determinism, and GLB-structure tests.
2. Define declarative primitives, transforms, palette roles, parent/bone bindings, and animation keyframes in RON.
3. Implement deterministic tessellation and stable ordering. Never embed timestamps, temp paths, host names, or nondeterministic map iteration.
4. Merge static rack/equipment geometry by material and asset so server slots and LEDs do not become individual runtime entities.
5. Generate:
   - technician: adult silhouette, 11-bone rigid-weight skin, Idle/Walk/Repair clips;
   - rack: top and two readable faces, server slots, status lights, outline shell;
   - cooling unit;
   - utility props: cart and stool;
   - infrastructure: trays and hose modules.
6. Implement `assetgen --write` and `assetgen --check`.
7. Parse every output GLB and assert required names, 11 joints, three clips, approved materials, finite bounds, valid indices/weights, and bounded primitive counts.
8. Generate twice in different temp directories and require byte-identical GLBs.

Run:

```bash
cargo run --bin assetgen -- --write
cargo run --bin assetgen -- --check
cargo test --test asset_contract
```

Expected: committed GLBs exactly match autonomous generation; no Blender executable or manual step appears anywhere.

Commit: `feat: generate rigged cell shift assets autonomously`

## Task 3: Load assets and build the data hall

**Files:** `src/assets.rs`, `src/world.rs`, `src/design.rs`, `tests/app_contract.rs`.

1. Add `AssetPlugin` with explicit `Loading`, `Ready`, and `Failed` states; no procedural fallback on load failure.
2. Add `RenderAssets` caching one unit primitive mesh per shape and one material per `PaletteRole`.
3. Define the fixed 40m square hall with four rack rows, three aisles, cooling units, trays, hoses, markings, cart, and stool.
4. Keep visual and collider lists separate by selected design; validate their stable IDs before spawning.
5. Extract collider rectangles once into a cached resource.
6. Spawn generated GLB modules only after all handles are loaded.
7. Add a grid flood-fill proving all aisle checkpoints and the player spawn share one walkable component.
8. Test exact counts, transforms, material roles, load failures, and collider joins.

Run:

```bash
cargo run --bin assetgen -- --check
cargo test world
cargo test --test app_contract hall
```

Expected: asset and hall contracts pass; any missing asset or blocked aisle is an explicit failure.

Commit: `feat: build the authored cel shift data hall`

## Task 4: Add the rigged technician and real keyboard movement

**Files:** `src/player.rs`, `src/lib.rs`, `tests/app_contract.rs`.

1. Spawn the technician GLB and discover named rig nodes into `PlayerParts`; missing or duplicate names are errors.
2. Read actual `ButtonInput<KeyCode>` arrow state, normalize diagonal input, and map it through the live camera basis.
3. Resolve X and Z separately against the cached collider vector and room bounds to permit sliding.
4. Set facing from accepted displacement, not requested input.
5. Drive Idle/Walk animation clips from accepted velocity.
6. On idle transition, stop Walk and explicitly reset every `PlayerParts` node before playing Idle, per selected review decision.
7. Add the full key matrix for none, opposites, cardinals, and diagonals.
8. Add real-input waypoint traversal through all three aisles plus collision and boundary probes.
9. Fault-inject a stale part handle and require an explicit verification failure.

Run:

```bash
cargo test player
cargo test --test app_contract keyboard_movement
cargo test --test app_contract aisle_waypoint_journey
```

Expected: movement, collision, facing, rig animation, reset, and reachability contracts pass.

Commit: `feat: add rigged camera-relative technician movement`

## Task 5: Add clamped four-way camera orbit

**Files:** `src/camera.rs`, `src/lib.rs`, `tests/app_contract.rs`.

1. Implement `CameraHeading::{NorthEast, SouthEast, SouthWest, NorthWest}`.
2. Read actual Q/E `just_pressed`; simultaneous Q+E cancels.
3. Retarget desired heading immediately and interpolate current yaw with smoothstep over 0.30 seconds.
4. Use current interpolated camera basis for movement throughout the tween.
5. Compute ground footprint and legal follow target from current yaw every frame; keep zoom, elevation, and roll fixed.
6. Test four settled headings, both directions, wraparound, simultaneous cancellation, rapid retarget, exact duration, midpoint, and final angle.
7. For center/four corners at all headings and tween midpoints, assert player viewport margin >=32px and nonempty target bounds.

Run:

```bash
cargo test camera
cargo test --test app_contract camera_orbit
```

Expected: all orbit/follow/projection branches pass with values named in failures.

Commit: `feat: add four-way isometric camera orbit`

## Task 6: Add recurring faults, prioritized tickets, and repair

**Files:** `src/operations.rs`, `src/player.rs`, `tests/app_contract.rs`.

1. Implement the documented rack state machine and keep its ASCII diagram beside the enum.
2. Add deterministic seeded scheduler, stable `TicketId`, severities, and maximum active capacity three.
3. Prevent duplicate active tickets for one rack.
4. Sort Critical before Warning, then creation tick, then rack ID.
5. On Space `just_pressed`, find only in-range open tickets and select deterministically by priority then distance then ID.
6. Enter Repairing for three seconds, lock movement, play Repair, and show blue wrench.
7. Resolve automatically, show healthy state for two seconds, remove the active ticket, cool down eight seconds, then permit recurrence.
8. Test exact seeded sequences, capacity, priority ties, duplicate suppression, out-of-range rejection, in-range start, movement lock, timer boundaries, resolution, cooldown, and recurring re-fault.

Run:

```bash
cargo test operations
cargo test --test app_contract recurring_ticket_journey
```

Expected: the exact multi-ticket lifecycle passes for at least two recurrence cycles.

Commit: `feat: add recurring fault and repair gameplay`

## Task 7: Add operations HUD and diegetic badges

**Files:** `src/hud.rs`, `src/operations.rs`, `tests/app_contract.rs`.

1. Build a top-left queue showing up to three tickets in deterministic priority order.
2. Use icon/shape/color first; short real labels are permitted because Bevy renders text reliably.
3. Add red fault badges and blue repair badges with thin leader lines over affected racks.
4. Add compact bottom-right Arrow/Q/E/Space controls.
5. Read operations state directly; do not copy ticket state into a second UI model.
6. Test healthy, one-ticket, three-ticket, repairing, resolved, and post-removal UI states.
7. At 1280x720 and 960x540, query `ComputedNode` and assert all panels are on-screen, ordered, and outside the central 50% play rectangle.

Run:

```bash
cargo test hud
cargo test --test app_contract operations_hud
```

Expected: state-to-UI and layout contracts pass without a screenshot judgment.

Commit: `feat: add ticket hud and rack status badges`

## Pages Milestone B: Publish the playable WASM game

Execute Task 4 from the Pages implementation plan.

The milestone builds the same production plugins for `wasm32-unknown-unknown`,
packages them with the pinned wasm-bindgen CLI, and uses headless Chromium to
prove the game reaches Ready, renders nonblank cel-shift pixels, handles Arrow,
Q/E, and Space without scrolling the page, and reports no captured browser
errors.

Commit and push the green WASM increment directly to `main`. A failed web gate
must update Pages status while retaining the previous status-only or playable
artifact.

## Task 8: Build the autonomous verification and visual hill-climb gate

**Files:** `src/verification.rs`, `src/main.rs`, `tests/render_contract.rs`, `scripts/check.sh`.

1. Add `--verify-output <directory>`; invalid/unsafe/unwritable paths return code 2 with stderr. Write only exact named files and never recursively delete a supplied path.
2. Implement the documented `VerificationStage` enum and test every legal and illegal transition.
3. Inject real `ButtonInput<KeyCode>` presses/releases for movement, Q/E orbit, and Space repair.
4. Run fixed 1/60-second time, fixed fault seed, MSAA off, and magenta sentinel clear color.
5. Capture 14 named frames:
   - healthy center NE;
   - three-fault queue NE;
   - walk pose NE;
   - repairing NE;
   - resolved NE;
   - settled SE, SW, NW;
   - Q/E tween midpoint;
   - one clamped corner for each heading;
   - 960x540 three-ticket layout.
6. Write canonical `report.json`: sorted maps, relative paths, no wall-clock/host fields, floats rounded to 1e-6, exact source/reference hashes, asset hashes, gameplay results, camera states, ticket histories, UI rectangles, and frame paths.
7. Hash canonical JSON and prove equal output for semantically identical runs in different temp directories.
8. Add app watchdog 45 seconds and parent watchdog 50 seconds; retain stdout, stderr, report, and frames on failure.
9. Implement single-pass `FrameMetrics` per image and one cached reference metric record.
10. Enforce mandatory frame contracts:
    - exact dimensions and artifact names;
    - <=0.1% magenta sentinel at settled corners and tween midpoint;
    - mean linear luminance `[0.48, 0.88]`, within 0.18 of key art;
    - >=60% pixels within RGB distance 24 of approved palette;
    - floor >=20%, rack base/shadow >=6%, yellow >=0.5%, ink/hose 3%-35%;
    - each diagonal edge band 30-50 and 130-150 degrees >=8% strong-edge mass in every settled heading;
    - hard-hat and hi-vis pixels in projected worker crop;
    - bounded Idle/Walk/Repair crop differences and <=1% outside-crop change for same-position captures;
    - red badge pixels for open faults, blue badge pixels during repair, healthy green after resolution;
    - computed HUD rectangles match screenshot state and remain on-screen;
    - nearest-palette histogram L1 distance from key art <=0.90;
    - edge density between 0.35x and 2.5x key-art density.
11. Generate in-memory bad fixtures and prove targeted rejection: all-black, gradient-noise, magenta-border, axis-aligned-only, missing worker colors, missing badge colors, and blank HUD.
12. `scripts/check.sh` runs fmt, clippy, assetgen check, all pure/integration tests, rendered contract, and release build. On headless Linux it requires Xvfb; missing renderer/display is a hard failure.

Run:

```bash
./scripts/check.sh
```

Expected: every asset, state, input, simulation, camera, UI, image, timeout, and release-build gate passes.

Commit: `test: add autonomous gameplay and render verification`

## Pages Milestone C: Publish comparisons, evidence, and last-green retention

Execute Tasks 5-6 from the Pages implementation plan.

This milestone must:

- publish current game frames beside the approved key art;
- publish the worker crop beside the character sheet;
- add screenshot history only when the semantic visual hash changes;
- publish sanitized gate counts, durations, and metric deltas;
- show current failures and challenges without exposing raw logs or local paths;
- retain the previous playable game and screenshots whenever native, render, or
  WASM verification fails.

Commit and push every green increment directly to `main`.

## Task 9: Add CI and publish the reproducible POC baseline

**Files:** `.github/workflows/ci.yml`, `README.md`, `docs/verification/v0-center.png`, `docs/verification/v0-report.json`.

1. Use `ubuntu-24.04`, pinned Rust, cache Cargo, install Bevy Linux prerequisites plus Mesa Vulkan and Xvfb.
2. Run pure checks and asset generation normally.
3. Run rendered contract with:

```bash
LIBGL_ALWAYS_SOFTWARE=1 \
WGPU_BACKEND=vulkan \
WGPU_ADAPTER_NAME=llvmpipe \
xvfb-run -a cargo test --test render_contract -- --nocapture
```

4. Upload frames, report, stdout, and stderr on failure.
5. Generate the documented baseline by setting `CELL_SHIFT_VERIFY_OUTPUT=docs/verification`; retain only canonical center PNG and canonical semantic report after all assertions pass.
6. Run baseline generation twice and require identical semantic hashes. PNG bytes are documentation, not a cross-GPU golden gate.
7. Document controls, architecture, asset regeneration, simulation rules, all gates, artifact diagnostics, and exclusions.
8. Run `./scripts/check.sh` after documentation/baseline generation.

Commit: `docs: publish verified cell shift poc baseline`

## Pages Milestone D: Publish the final project baseline

Execute Task 7 from the Pages implementation plan after Task 9.

Run the unified native, asset, progress, site, render, WASM, browser, and
release gate. Update `docs/progress.json`, `docs/implementation-plan.md`,
challenges, screenshots, and README from their canonical sources. Commit and
push directly to `main`, then require a successful Pages deployment associated
with that commit.

## NOT in Scope

- Multiplayer, networking, accounts, cloud services, or persistence: the POC is local and deterministic.
- Procedurally generated room layouts: one authored hall is required for stable art comparison.
- Economy, inventory, staffing, shift scheduling, or spare-parts systems: the operations slice is tickets/faults/repair only.
- Continuous free camera, mouse orbit, or zoom: Q/E four-way orbit is the locked interaction.
- Audio: it does not prove the requested visual/gameplay hill climb.
- Blender or any manual DCC/export step: autonomy is a hard constraint.
- ML/network aesthetic evaluation: local deterministic metrics are the acceptance authority.
- Human playtest, screenshot review, or subjective score: explicitly prohibited as gates.

## TODOS.md Disposition

No `TODOS.md` is created. The user directed every proposed valuable follow-on—four-way orbit, autonomous authored/rigged assets, and recurring ticket/fault/repair gameplay—into the POC itself. Remaining exclusions are intentionally outside the stated product slice rather than deferred implementation debt.

## Completion Summary

- Step 0: Scope Challenge — user chose scope reduction, then explicitly expanded the mandatory POC; structural discipline retained, no requested feature deferred.
- Architecture Review: 4 issues found and decided.
- Code Quality Review: 4 issues found and decided.
- Test Review: diagram produced; 4 critical silent gaps identified and closed in the plan.
- Performance Review: 4 issues found and decided.
- Additional scope review: Q/E orbit, autonomous no-Blender rigged assets, and recurring prioritized tickets added.
- Continuous publication review: playable WASM, status-always Pages deployment, concept comparisons, progress/challenge tracking, screenshot history, plans, diagrams, tests, and commits added.
- NOT in scope: written.
- What already exists: written.
- TODOS.md updates: 0; user moved all proposed valuable items into POC scope.
- Failure modes: 17 registered; 0 remain without both detection and explicit handling.
- Unresolved decisions that may bite later: none.

## Definition of Done

- `cargo run` loads only generated, repository-owned GLBs and presents the cel-shift hall.
- Arrow keys move the rigged worker correctly at every settled heading and during orbit.
- Q/E animate deterministic quarter-turns and the camera remains clamped throughout.
- Up to three seeded prioritized tickets recur; Space repairs only an in-range ticket; repair animation/state/HUD complete correctly.
- All three aisles are reachable and walls/equipment remain impassable.
- The no-Blender asset generator reproduces committed GLBs byte-for-byte.
- Every pure, real-input, state-machine, asset-load, UI-layout, and failure-injection test passes.
- Every one of the 14 real frames passes every mandatory visual metric.
- The canonical semantic report hash reproduces across output directories.
- `./scripts/check.sh`, the software-rendered CI job, and release build pass.
- `docs/progress.json` accurately drives Done, Working Now, Future, and
  Challenges, and `sitegen` rejects drift from the reviewed plan.
- GitHub Pages publishes the current `main` status after every push while
  retaining the last verified playable game and screenshots after failures.
- The Pages comparison view publishes both approved cel-shift references,
  current game/worker captures, visual metric deltas, plans, ASCII diagrams,
  test results, challenges, and commit history.
- The Bevy WASM build passes the headless Chromium readiness, input, error, and
  nonblank-canvas contract.
- GitHub's Pages deployment API reports success for the final `main` commit.
- No human gate exists.
