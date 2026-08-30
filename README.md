# Cell Shift Data Center POC

A Bevy 0.19.1 proof of concept for a cel-shift data-center operations game.
The project uses repository-owned, declarative assets and objective automated
verification; Blender and human visual approval gates are intentionally out of
scope.

## Reviewed foundation

- Rust 1.98.0 with rustfmt and Clippy
- Bevy 0.19.1 with 3D, UI, glTF, and PNG support
- Typed palette, scene, collider, camera, and timing contracts
- Approved cel-shift key art and character sheet in `docs/reference/`

## Autonomous asset pipeline

Every mesh is generated from repository-owned declarative RON. There is no
external content tool, no third-party art, and no manual export step.

- `assets/source/*.ron` — primitives, repeat lattices, palette roles, bone
  bindings, and animation keyframes
- `src/assetgen.rs` — validation, deterministic tessellation, merge by palette
  role, rigid skinning, animation sampling, and the glTF writer
- `assets/generated/*.glb` — committed outputs, regenerated and byte compared by
  `assetgen --check`

Generated assets:

| Asset | Scenes | Highlights |
|---|---|---|
| `technician.glb` | `technician` | adult silhouette, 11-bone rigid skin, Idle/Walk/Repair clips, blue hard hat, hi-vis vest, slate shirt, dark trousers, brown boots, tool belt and wrench |
| `rack.glb` | `rack-row` | eight merged cabinets, readable top plus lit and shadow faces, 96 server slots, 96 status lights, ink outline hull, contact shadow |
| `cooling-unit.glb` | `cooling-unit` | grille fins, teal fan, coolant risers, status panel |
| `utility-props.glb` | `utility-cart`, `step-stool` | red service cart with toolbox and casters, yellow stool with treads |
| `infrastructure.glb` | `overhead-tray`, `hose-drop` | cable tray with hangers and bundles, black hose trunk with collars |

Determinism rules: declaration or `PaletteRole::ALL` ordering everywhere, every
written float quantized to a 1e-6 grid with negative zero normalized away, a
constant generator string, and no timestamp, host, user, or path data in any
output.

Rig convention: skinned `POSITION` values stay in model space, and each inverse
bind matrix is the standard `T(-global_bind_origin)`, so a conformant renderer
computing `global joint transform * inverse bind matrix * POSITION` reproduces
the authored rest pose exactly. Budgets are enforced during generation, not only
in tests: at most 256 bones per rig, 24 000 triangles per asset, and clip names
unique across every rigged module because glTF animation names are document
scoped.

## Authored data hall

`src/assets.rs` loads the committed GLBs and publishes the shared render
handles; `src/world.rs` spawns the hall from the validated blueprint in
`src/design.rs`.

- `AssetLoadState` is explicitly `Loading`, `Ready`, or `Failed`. A missing,
  corrupt, mislabelled, or misbound asset records the offending path in
  `AssetLoadReport` and stops there. There is no procedural fallback.
- Readiness requires more than a successful read: every loaded glTF document
  must expose the module scene name the pipeline declares, and that named scene
  must be the very same sub-asset handle the hall spawns by scene index. A
  multi-scene file whose scene order and name binding are swapped therefore
  fails readiness instead of silently spawning the wrong module.
- `RenderAssets` creates exactly one unit mesh per primitive shape and one
  unlit material per `PaletteRole`. Spawning the hall creates no new mesh or
  material assets.
- Generated static detail stays merged inside its module, so no server slot,
  status light, or tray rung becomes a runtime entity.

The fixed 40 m square hall contains 29 authored visuals and 13 colliders:

| Category | Count | Placement |
|---|---|---|
| Floor | 1 | 40 m x 40 m polished light concrete |
| Low perimeter walls | 4 | flush outside the play area, 1.2 m high |
| Rack rows | 4 | x = -9, -3, 3, 9 |
| Aisles | 3 | x = -6, 0, 6, z = -12 to 12 |
| Cooling units | 4 | x = +/-13, z = +/-6 |
| Overhead trays | 3 | one per aisle at y = 4 |
| Hose drops | 3 | hanging from each tray at z = 7 |
| Utility cart, step stool | 1 each | (-13, -10) and (13, 10) |
| Yellow floor markings | 8 | six aisle edges plus two cross-hall walkways |

Visual and collider lists stay separate and are joined only by their stable
`PropId`; duplicate, missing, and orphan identifiers are all reported in one
aggregate pass before anything spawns. Colliders are extracted once into
`HallColliders` and scanned linearly, and a room-wide flood fill over a 0.25 m
walkability grid proves that every aisle centreline checkpoint shares one
walkable component with the player spawn. Connectivity alone would accept a
hairline corridor, so `validate()` also measures clearance: every grid
cross-section of every aisle must keep a contiguous run of open nodes at least
0.50 m wide, or the blueprint is rejected with
`SceneValidationError::InsufficientAisleClearance`. The grid is already inflated
by `PLAYER_RADIUS`, so that run is measured in centre space -- a run of `n`
adjacent open nodes spans `(n - 1)` cells -- and the player diameter is never
counted twice. The authored hose drops at z = 7 are the narrowest point in the
hall at exactly 0.50 m, one grid cell above the gate.

## Rigged technician and keyboard movement

`src/player.rs` spawns the generated technician once the assets and the hall are
both ready, binds its rig explicitly, and moves it from the real
`ButtonInput<KeyCode>` arrow state.

- `ViewBasis` is the one public screen-to-world interface. It is initialised to
  the reviewed NorthEast 45-degree diamond view; movement never reads a camera
  entity, so the orbit task can retarget the basis without touching a movement
  rule.
- Arrow keys produce a normalized screen request: opposite keys cancel exactly,
  and a diagonal never outruns a cardinal. The request is mapped through
  `ViewBasis`, so the eight key combinations become the eight compass directions
  on the ground plane at every heading.
- World X and world Z are resolved separately against the cached
  `HallColliders` vector and radius-aware room bounds, which is what lets the
  technician slide along a rack face instead of sticking to it. Every rejection
  names the offending `PropId` in `PlayerMotion`.
- Facing and the animation state come from the accepted displacement, never the
  requested direction. Walking into a rack diagonally therefore turns the
  technician along the slide.
- `PlayerParts` holds explicit handles for the skinned mesh node and all eleven
  bones, together with the rest transform captured before any clip played.
  Missing, duplicated, and stale nodes are typed `PlayerRigError` values that
  move the app into `PlayerRigState::Failed` and stop movement; nothing is ever
  silently skipped. Bevy despawns and respawns a glTF world instance when a
  sub-asset event arrives, so the binder rescans and rebinds on any complete
  instance and keeps the stale handles otherwise.
- The generated `Idle` and `Walk` clips are driven through one
  `AnimationGraph`. The idle transition stops `Walk`, explicitly restores every
  `PlayerParts` rest transform, and only then plays `Idle`, so a stopped
  technician never keeps a mid-stride pose. The `Repair` clip is bound in
  `PlayerAnimations` for the operations task and nothing plays it yet.

Movement gates:

```bash
cargo test player
cargo test --test app_contract keyboard_movement
cargo test --test app_contract aisle_waypoint_journey
```

The keyboard matrix drives real key presses across all four headings for the
empty press, both opposing pairs, all cardinals, and the diagonals. The waypoint
journey walks the technician end to end through all three aisles using only
accepted arrow input, and each aisle's authored hose drop -- which closes the
centre line to a 0.50 m centre-space run -- is crossed off centre rather than
teleported past.

## Foundation gates

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo run --bin assetgen -- --write
cargo run --bin assetgen -- --check
cargo test
```

Hall gates:

```bash
cargo test world
cargo test --test app_contract hall
```

The hall contracts drive a real Bevy app built from `DefaultPlugins` with
`WinitPlugin` disabled and `RenderPlugin` created without a wgpu backend, so the
committed GLBs are loaded by the real glTF loader on a machine with no GPU.
