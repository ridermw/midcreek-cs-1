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

## Foundation gates

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo run --bin assetgen -- --write
cargo run --bin assetgen -- --check
cargo test
```
