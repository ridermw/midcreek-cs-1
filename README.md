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

The fixed 40 m square walkable hall contains 30 authored visuals and 13
colliders. Only the visual apron lies outside the room; everything else is
inside it:

| Category | Count | Placement |
|---|---|---|
| Rendered-coverage apron | 1 | 72 m x 72 m building shell, 0.05 m below the floor, visual only |
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
- Collision is resolved at the destination, so the integration step is the whole
  anti-tunneling invariant and movement owns it rather than borrowing it from
  the engine. `movement_delta_secs` clamps every frame delta to
  `PLAYER_MAX_MOVE_DELTA` (0.25 s, matching `Time<Virtual>`'s default maximum
  delta), and a test proves `PLAYER_SPEED * PLAYER_MAX_MOVE_DELTA` is strictly
  shorter than every radius-inflated authored obstacle on both world axes -- the
  narrowest being the 1.10 m inflated hose drop. A hitch frame therefore
  shortens the step instead of stepping over a hose.
- Facing and the animation state come from the accepted displacement, never the
  requested direction. Walking into a rack diagonally therefore turns the
  technician along the slide.
- `PlayerParts` holds explicit handles for the skinned mesh node and all eleven
  bones, together with the rest transform captured before any clip played.
  Missing, duplicated, and stale nodes are typed `PlayerRigError` values that
  move the app into `PlayerRigState::Failed` and stop movement; nothing is ever
  silently skipped.
- Bevy despawns and respawns a glTF world instance when a sub-asset event
  arrives, so the binder rescans every frame the bound handles stop resolving.
  No path leaves the report claiming health: an unresolvable instance root
  publishes `TechnicianInstanceUnavailable` and an instance with no named nodes
  publishes `TechnicianRigNodesUnavailable`, both of which make
  `player_rig_is_ready` false in that same frame and leave the rig `Pending` so
  a complete instance recovers on its own. `StalePart` stays the verdict while
  the rest of the rig survives the loss; when every bound handle died the
  instance itself was replaced, so an incomplete replacement reports its own
  `MissingPart` and `DuplicatePart` findings instead of a wall of stale handles.
- The generated `Idle`, `Walk`, and `Repair` clips are driven through one
  `AnimationGraph`. Every transition stops the clips it is leaving, explicitly
  restores every `PlayerParts` rest transform, and only then plays the
  destination clip, so no stale mid-stride or mid-repair pose survives a change
  of clip. That restore runs for *every* destination, including `Walk`:
  `Repair` poses `bone-head`, `bone-arm-lower-right`, and `bone-tool`, none of
  which `Walk` animates, so skipping it would leave those bones stuck in the
  repair pose. A clip that is already playing takes an early return and is
  never re-posed.

Movement gates:

```bash
cargo test player
cargo test --test app_contract keyboard_movement
cargo test --test app_contract technician_rig
cargo test --test app_contract aisle_waypoint_journey
```

The keyboard matrix drives real key presses across all four headings for the
empty press, both opposing pairs, all cardinals, and the diagonals. The waypoint
journey walks the technician end to end through all three aisles using only
accepted arrow input, and each aisle's authored hose drop -- which closes the
centre line to a 0.50 m centre-space run -- is crossed off centre rather than
teleported past. The rig tests despawn and rebuild the whole instance in the
running app to prove movement stops while it is unavailable, recovers on its
own when a complete rig returns, and names the specific missing or duplicated
node when the replacement is incomplete.

## Clamped four-way camera orbit

`src/camera.rs` owns the one game camera and is the sole runtime updater of
`ViewBasis`. Movement still never reads a camera entity.

- One orthographic `Camera3d` renders a fixed 26 m by 14.625 m rectangle at 57
  degrees of elevation with zero roll, so zoom and pitch never change with the
  heading or the window. The authored hall is unlit, so the camera alone makes
  it visible and the app spawns no light at all.
- `CameraHeading` names the compass quadrant the camera itself occupies, on a
  map whose north is `+Z` and whose east is `+X`. `NorthEast` is the reviewed
  initial 45-degree view, and the declared order `NE -> SE -> SW -> NW` is the
  clockwise one `E` walks.
- Real `Q`/`E` `just_pressed` frames retarget the desired heading immediately;
  holding a key never spins the camera, and both keys on one frame cancel
  exactly, leaving the running turn untouched rather than restarting it.
- The yaw eases with smoothstep at a constant 90 degrees per 0.30 seconds. A
  settled quarter turn therefore takes exactly 0.30 s, and a turn retargeted
  mid-tween starts at the interpolated yaw and takes only what the shortest
  remaining angle costs: 0.15 s to reverse from the midpoint, 0.45 s to queue a
  second quarter turn from it.
- The interpolated basis is published in `UpdateOrbitIntent`, before
  `MovePlayer` reads it, so the technician walks along the camera it can see on
  that frame rather than the one from the frame before.
- The walkable room and the rendered coverage are two different squares. The
  technician may only ever stand inside the 40 m room the perimeter walls
  enclose; the camera may overhang that room freely, because a 72 m visual
  apron of building shell is authored beneath and outside it. The apron uses
  the cel-shift floor-shadow role, sits 0.05 m below the floor so the coplanar
  40 m square never z-fights, carries no collider, and is never walkable.
- Every frame the ground quadrilateral is cast from the current yaw, its
  axis-aligned extents are subtracted from the *active blueprint's*
  `room.coverage` -- the same field the validator checks and the apron is
  authored from, falling back to `RENDER_COVERAGE_SIZE` exactly as the hall
  spawner falls back to `SceneBlueprint::v0` -- and the followed technician is
  clamped into what is left before the transform is
  derived. Because `72 / 2 - hypot(13, 8.71916) = 20.3468` m exceeds the room's
  20 m half extent, every legal player position is followed exactly, at every
  yaw; the clamp only ever engages for a position the technician cannot reach.
  The footprint is widest between headings, not at one, so the tightest legal
  rectangle of a whole orbit -- 20.3468 m, at yaw 33.851 degrees, with 0.3468 m
  of slack -- is a state only a mid-tween frame reaches. The blueprint
  validator checks those two extremal yaws as well as the eight sampled ones,
  and rejects any coverage that cannot follow the whole walkable room.

Camera gates:

```bash
cargo test camera
cargo test --test app_contract camera_orbit
```

The orbit contracts drive real `KeyboardInput` messages through the same
headless app the hall and movement contracts use, then measure the spawned
camera entity: its heading quadrant, fixed distance, elevation, and roll, its
recovered ground target, and its projected framing through Bevy's real
`Camera::world_to_viewport`. Framing is asserted at the room centre, the
authored player spawn, all three aisle centre lines end to end, and all four
room corners, at four settled headings and three tween samples each.

**Tween samples are exact.** A sample of `n` frames is exactly `n / 18` of the
turn: the key tap itself runs the first frame -- the press is read in
`ReadInput` and the tween advances in `UpdateOrbitIntent` of that same frame --
so the pump is one frame shorter and there is no trailing update. The requested
midpoint really is `elapsed / duration = 0.5`, and because `smoothstep(0.5)` is
`0.5` the eased yaw there is the plain arithmetic midpoint: 90 degrees
clockwise off `NorthEast`, 90 degrees counter-clockwise off `SouthEast`, and
0 degrees for both wraparounds through zero.
`camera_orbit_tween_samples_land_on_the_exact_requested_fraction` asserts those
numbers on the resource and on the real camera entity, and pins one frame later
at `10 / 18` so an off-by-one frame cannot hide. Each batch of samples then runs
with the virtual clock stopped, and asserts the whole `CameraOrbit` resource is
unchanged and the yaw bit-identical for every sample, so a requested mid-tween
state cannot settle part way through a loop.

**Framing is the whole technician, not a ground point.** The gate projects the
eight corners of the box that contains the technician's full spatial envelope,
measured from the generated source: the rest pose and every sampled pose of the
generated Idle, Walk, and Repair clips, rigidly skinned through the real rig,
with the cel outline hulls included. That is a 0.7998 m radius and a 1.9704 m
crown against a 0.3110 m radius and a 1.9440 m crown at rest, and every one of
those corners has to stay at least 32 logical pixels inside the viewport.
`camera_framing_margin_is_calibrated_against_the_real_pixel_to_world_scale`
anchors that number independently: 1280 px over 26 m is 49.23077 px/m, so a
point 12.35 m across the ground, or 7.9441 m along it, must project at exactly
32 px from the edge, and one metre-pixel further out must fail the gate.

**Room corners.** `camera_orbit_frames_every_room_corner_with_the_reviewed_margin`
drives the running app to all four corners of the walkable room at all four
settled headings and two tween samples each, asserts each tween sample really
lies between headings, and requires the whole envelope around both the
reachable corner and the wall corner itself to keep at least 32 logical pixels
of margin. The followed corner
is centred to within half a pixel and keeps the full 360 px half-viewport,
because the apron lets the camera go there.
`camera_orbit_holds_the_rendered_apron_instead_of_leaking_past_it` then proves
the follow target is never clamped at a corner, that the whole ground
quadrilateral stays inside the 72 m coverage, and that it genuinely does
overhang the 40 m room -- so the apron is load bearing rather than decorative.
`camera_follow_clamps_against_the_active_blueprint_coverage_not_the_constant`
overrides the running hall with two other valid coverages and proves the
runtime clamp boundary moves with the blueprint rather than with the constant.
An earlier revision of the plan asked for corner framing *and* containment
inside the 40 m room, which is impossible for any room size; the published
challenge records how the two squares were separated.

## Recurring faults, prioritized tickets, and repair

`src/operations.rs` owns the whole operations slice and carries the reviewed
rack state machine diagram beside the enum it documents.

- Operational state hangs on the four authored `rack-row-NN` `HallProp`
  entities, joined to the blueprint and to their cached collider rectangles by
  stable `PropId`. Rack indices are assigned from sorted identifier order, which
  is also the authored declaration order, so a rack index means the same thing
  in every report.
- One seeded ChaCha8 stream, `0xCE11_5A1F_DA7A_CE01`, produces the whole fault
  sequence. It is an ordered generator, not a sampler of the current world:
  every four simulated seconds an opportunity matures and the timer stops
  accumulating, a full queue pauses it without consuming a single word, and a
  drawn candidate whose rack already holds a ticket -- or is still repairing,
  resolving, or cooling down -- is held and reported rather than rerolled.
  Exactly two words are consumed per candidate, so repairing early or late moves
  only *when* each fault arrives, never *which* fault it is.
- Tickets carry stable monotonic identifiers that are never reused. The queue
  holds at most three, refuses a second ticket for one rack, names the rack it
  refused, and stays sorted Critical before Warning, then by creation tick, then
  by rack index.
- `Space` gathers only open faults, measures the distance from the technician to
  each rack's collider *rectangle* rather than to its centre, and selects by
  severity, then distance, then creation tick, then rack index. An out-of-range
  press changes nothing at all and is recorded as a named rejection with the
  nearest faulted rack and its distance; it is never counted as a start.
- Starting a repair runs in `UpdateOperations`, before `MovePlayer`, so the
  movement lock takes hold on the same frame. The arrow request is dropped
  rather than merely blocked, so the published motion really is standing still,
  the generated `Repair` clip plays, and the blue-wrench state is exposed for
  the HUD task. The camera keeps orbiting and following throughout.
- The tail completes on its own: three seconds of repair, then the lock is
  released and the healthy indicator shows for two seconds, then the ticket
  leaves the queue as the rack begins an eight-second cooldown, and only a fully
  recovered rack is eligible again. Capacity reopens the instant the ticket is
  removed, and a paused opportunity fires on that very frame.
- Every timer is a `std::time::Duration` of whole nanoseconds rather than an
  accumulated `f32`, so the fixed sixtieth-of-a-second contract lands on exact
  tick boundaries: 240 ticks to a fault, 180 to a repair, 120 to the healthy
  indicator, 480 to a recovered rack.

Operations gates:

```bash
cargo test operations
cargo test --test app_contract operations
cargo test --test app_contract recurring_ticket_journey
```

The real-app operations contracts each cover the interaction from one angle,
and none of them stands in for another:

- `operations_out_of_range_space_is_rejected_and_stays_observable` is the
  dedicated out-of-range test. It presses the real `Space` key from the middle
  of the centre aisle, 2.2 m from both faulted inner rack faces, and proves the
  named `OutOfRange` rejection changes no position, lock, queue, rack state, or
  clip, and is never counted as a start. A second app proves an empty queue is
  its own explicit `NoOpenTickets` rejection.
- `operations_in_range_space_starts_the_repair_and_locks_movement_in_one_frame`
  is the dedicated approach-and-lock test: over two hundred frames of real
  arrow input with no transform writes, then a real `Space` press with the
  arrows still held, and the lock, the `Repair` clip, and the zero accepted
  motion all landing on that one frame. It is the only operations test that
  walks the technician *to* a rack;
  `operations_leaving_the_repair_clip_restores_every_rest_transform_first`
  also walks, but afterwards, to prove the released repair really hands over to
  a moving `Walk`.
- `operations_space_counts_one_edge_per_press_not_a_held_key` holds `Space`
  down for hundreds of frames and proves one press is counted exactly once,
  that a held key neither spams a rejection at 60 Hz nor re-enters a running
  repair, and that releasing and pressing again is a second recognised edge.
- `operations_leaving_the_repair_clip_restores_every_rest_transform_first`
  releases the lock with the arrows still held and proves the direct
  `Repair` -> `Walk` transition restores every rest transform before `Walk`
  plays, so the head, right forearm, and tool cannot keep the repair pose.

`recurring_ticket_journey` is the long recurrence contract rather than a second
interaction test. Over roughly two thousand fixed frames of the real app it
fills the queue to three simultaneous tickets on ticks 240, 480, and 720, then
repeatedly *teleports* the technician to the repair spot of the highest-priority
ticket -- deliberately, because the real approach and the out-of-range rejection
are each proved by their own contract above -- presses the real `Space` key, and
rides the whole documented tail out. It requires at least three completed
repairs and watches two separate racks fault again after their cooldowns, one of
them on the exact tick its eight-second cooldown ended, while the opened
sequence still matches the pinned seed prefix.

## Operations HUD and diegetic badges

`src/hud.rs` draws the whole HUD and owns no gameplay state. Every frame it
reads `TicketQueue`, `RackOperations`, `RackRoster`, `MovementLock`,
`LastInteraction`, and the real camera, and writes only presentation components
plus one observable `HudReport`. There is no second ticket model to drift.

- The top-left stack renders up to three rows straight out of the queue's own
  global priority order. It never re-sorts, so Critical before Warning, then
  creation tick, then rack index is the queue's ordering and the HUD's ordering
  by construction. Each row is a severity chip, a rack-state chip, a short real
  label such as `T0002 R01 Critical`, and a thin bar that fills across the
  repairing, resolved, and cooldown dwell times.
- Shape carries meaning alongside colour: a critical severity chip is a hard
  square and a warning chip is a circle; the fault badge is sharp cornered, the
  repair badge is rounded, and the resolved badge is a full pill.
- The status line below the rows is derived from live state only: a running
  repair outranks everything, then a real out-of-range rejection while the
  ticket it was about is still open, then the queue itself.
- Badges are fixed-size screen-space UI nodes, never world-space sprites, so
  they never rotate, shear, or resize with the camera. Each is anchored every
  frame from a stable rack world point 2.4 m above the rack's collider centre,
  projected through the real `Camera::world_to_viewport`. The pass reads the
  camera's own `Transform` rather than its propagated `GlobalTransform`, because
  propagation runs in `PostUpdate` and a stale transform would make badges lag
  the camera through every orbit tween.
- A visible anchor always has a fully visible badge: the badge box is clamped
  inside the viewport and its thin leader line is rotated to end exactly on the
  projected anchor, whatever the heading. An anchor that projects off screen
  hides explicitly as `BadgeVisibility::OffScreen`, and a projection the camera
  refuses is recorded as a typed `ProjectionFailed` error rather than silently
  skipped.
- The bottom-right strip names `Arrows`, `Q`, `E`, and `Space`, and says which
  are live: while a repair holds the technician still, the `Space` cap turns
  the technician's hard-hat blue and the `Arrows` cap goes flat.
- Every colour the HUD draws is a typed `PaletteRole`, and every fixed panel is
  pinned to a corner: the queue stack is 216 px wide and the control strip is
  40 px tall, so at 1280x720 and at 960x540 both keep a 16 px margin and stay
  outside the central 50% x 50% play rectangle.

`HudReport` is the presentation contract. It carries the rendered rows, every
rack's badge with the reason it is or is not drawn, the status, the movement
lock, the viewport, and a list of typed `HudError`s. A rack that lost its
`RackOperations`, a queue ticket whose rack is unknown, a missing badge or row
node, and a missing game camera are all reported rather than quietly ignored.

HUD gates:

```bash
cargo test hud
cargo test --test app_contract operations_hud
```

The real-app HUD contracts drive the same headless Bevy app the rest of the
suite uses and query the real laid-out `ComputedNode` and `UiGlobalTransform`:

- `operations_hud_badges_track_the_projected_rack_at_every_heading_and_mid_tween`
  sweeps all four headings and a genuine mid-tween frame at each, and requires
  every badge's recorded anchor to equal the test's own independent projection
  of the same world point, its visibility to agree with whether that projection
  is on screen, its laid-out box to stay exactly 34x22 px and fully inside the
  viewport, and its leader line to end within 1.5 px of the anchor.
- `operations_hud_clamps_an_edge_badge_and_still_points_its_leader_at_the_rack`
  walks the technician until a faulted rack is pushed against a side edge,
  which is the only case where the leader is not vertical, and proves the
  clamped badge stays on screen while the tilted leader still ends on the rack.
- `operations_hud_panels_stay_on_screen_and_clear_of_the_play_rectangle`
  resizes the real window to 1280x720 and 960x540 through a real
  `WindowResized` message and re-checks the margins, the row order, the panel
  containment, and the play rectangle at both sizes.
- `operations_hud_draws_only_typed_palette_colors` walks every HUD node and
  rejects any background, border, or text colour that is not a `PaletteRole`,
  and requires every visible label to have produced real glyphs.
- `operations_hud_reports_a_rack_that_lost_its_operational_state` and
  `operations_hud_reports_a_missing_camera_instead_of_drawing_stale_badges`
  fault-inject both failures into a running app and require the named typed
  error, the hidden badge, and an otherwise intact queue stack.

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
