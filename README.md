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
  corrupt, or mislabelled asset records the offending path in `AssetLoadReport`
  and stops there. There is no procedural fallback.
- Readiness requires more than a successful read: every loaded glTF document
  must also expose the module scene name the pipeline declares.
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
walkable component with the player spawn. The same report measures the
narrowest walkable width across those checkpoints, so a hairline gap between
two colliders cannot pass as a usable aisle.

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
