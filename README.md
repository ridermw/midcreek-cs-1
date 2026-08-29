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

## Foundation gates

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test design
```
