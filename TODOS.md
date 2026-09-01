# TODOS

Deferred work that is genuinely valuable but deliberately out of the current
plan's scope. Each entry records enough context to be picked up cold.

Items here were considered and deferred during the plan-exit review of
`docs/plans/2026-08-31-1507-feat-phase-2-hill-climb-plan.md` on 2026-08-31.

---

## Pin focal placement in the Phase 1 baseline

**What.** Add U3's fourth composition component, focal placement, to
`docs/reference/phase-1-baseline.json` and to `BASELINE_COMPONENTS` in
`tests/render_contract.rs`.

**Why.** U3 names rack mass, floor mass, diagonal angle, and focal placement as
the components M1 must be shown to have moved toward the reference. The
committed baseline pins the first three, using the diagonal band shares for
diagonal angle. Focal placement is absent, so an M1 gate built on this vector
could declare victory with one camera-sensitive component unmeasured.

**Context.** Focal placement is the projected worker rectangle, which the
analyzers read from `FrameMetrics::regions` keyed by `WORKER_REGION`. Those
rectangles are produced by the run and recorded in `report.json`, not derivable
from a committed PNG alone, so pinning it means committing the baseline run's
report beside the frames and cross-checking the two. That was out of scope for
U0, which stores the vector; U3 owns the gate that consumes it.

**Depends on.** Nothing. Best done as the first step of U3, before the camera
model changes, so the pre-change value is captured.

---

## Discover installed Windows browsers in the clean gate

**What.** Teach `scripts/check.sh` and `scripts/web-smoke.sh` to locate standard
Windows Chrome and Edge installations when no browser command is supplied.

**Why.** The clean gate now runs under Git Bash on Windows, but its browser
lookup only checks Unix and macOS locations. A Windows machine can therefore
have Chrome or Edge installed while the gate reports that no browser exists and
skips browser verification.

**Context.** Found during the U0 portability review after the Windows shell path
was enabled. This is outside U0, which is limited to lint cleanup, render
contract diagnosis, and binding the Phase 1 baseline. The future change must
preserve the existing rule that a missing renderer or display cannot look like
a passing render gate.

**Depends on / blocked by.** Coordinate this with U2's Windows gate script so
browser discovery has one owner and one documented override.

---

## Scope `OutlineHull` to silhouette-defining meshes

**What.** Emit reversed-winding outline hulls only for the meshes that carry the
Cel Shift silhouette — racks, the technician, the cart, the stool — and give the
remaining props (kerbs, hoses, quick-disconnects, ladder and mesh trays,
manifolds) `InkDetail` geometry only.

**Why.** Phase 2 KTD5 generates an expanded, front-face-culled hull for *every*
generated mesh. That is roughly a doubling of the triangle count, and by
construction it is pure overdraw: the hull exists to be drawn behind and around
the thing it outlines. Every gate this project runs — WARP on Windows, llvmpipe
on Linux, SwiftShader in the browser — is a **CPU rasteriser**, where frame cost
scales with triangles and overdraw rather than with GPU fill rate. At the same
time U4 and U6 substantially densify the hall (back-to-back rack-row pairs plus
eight new prop families) and U2 requires the continuous gate to get *faster*
(2-3 minutes, hard ceiling 5). Those three pressures point in opposite
directions. R24 forbids a game frame-rate target, which is correct for the game
but removes the signal that would otherwise expose a software-raster blow-up.

**Context.** The review chose option **9A** — add a geometry and overdraw budget
to the fidelity contract (triangle count, outline-hull ratio, per-frame
software-raster time), assert it headlessly in `tests/asset_contract.rs`, and
report it from every gate — in preference to option **9B**, which is this entry.
The reasoning was to measure before narrowing, so the budget is the tripwire and
this entry is the prepared response.

Trigger this work when the 9A budget is breached, i.e. when the continuous gate
approaches its 5-minute ceiling or the milestone gate approaches the 11A budget
and per-stage elapsed reporting attributes the cost to rasterisation rather than
to compilation or the mandated real-time simulation stages.

Starting points:

- `src/assetgen.rs` (1,965 lines) is where the `OutlineHull` render class is
  generated; the RON sources under `assets/source/*.ron` declare which modules
  exist.
- The palette/render-role coverage assertion lives in `AssetReadyProof`
  (see `src/assets.rs`), which already refuses meshes that resolve to no known
  role — so narrowing the hull set must update that coverage expectation rather
  than simply omitting geometry.
- The silhouette analyzers in the frozen fidelity contract
  (`docs/reference/fidelity.json`) decide which objects actually need an outline
  to pass; that contract is the authority for what "silhouette-defining" means,
  not intuition.

**Depends on / blocked by.** Requires 9A's geometry budget to exist and to have
produced at least one milestone's worth of measurements. Must not be done before
U5 lands, and any change to the hull set is a fidelity-policy change, so it
requires the G0 recalibration path described in KTD7.

---

## Audit `verification.rs` unwrap/expect calls

**What.** Review the 44 `unwrap()` / `expect()` call sites in
`src/verification.rs` and convert those that can be reached by a real gate
failure into named, diagnostic-preserving errors.

**Why.** `src/verification.rs` carries by far the highest panic density in the
crate — 44 calls, against 0 in `design.rs`, 3 in `camera.rs`, 1 in `metrics.rs`
and 5 in `hud.rs`. It is also the file that decides whether a gate passed and
what evidence survives a failure. A panic there produces a stack trace instead
of the named frame/stage/artifact diagnostics the rest of the module is
carefully built around (see the `CAPTURE_TIMEOUT` doc comment, which explicitly
argues that a named timeout is "strictly better evidence than a watchdog expiry
could ever be"). Phase 2's U8 adds injected-failure paths to this same file,
which multiplies the number of reachable error states.

**Context.** The review chose option **8A** — extract the stage machine and the
journey profiles into a new `src/journey.rs`, leaving analysis and reporting in
`verification.rs` — over option **8B**, which paired sequential landing with an
unwrap audit. The split addresses the merge-contention half of the problem (six
of ten plan units modify this file) but not the error-handling half, so the
audit is deferred here.

Do this **after** the 8A split, so the audit runs over two smaller files and the
results are not invalidated by the move. Expect many of the calls to be
legitimately infallible (constant table lookups, `PaletteRole` slot indexing,
e.g. `.expect("every role is in the palette")` at `src/verification.rs:4553`);
the goal is to separate those from the ones a broken run can actually hit, not
to eliminate the count.

**Depends on / blocked by.** Blocked by U2's `src/journey.rs` extraction (8A).
Best sequenced immediately after U8 lands its injected-failure paths, so the
audit covers the final set of reachable error states rather than an intermediate
one.

---

## Provide a macOS/Linux continuous-gate path

**What.** Give developers on macOS and Linux a continuous visual-iteration gate
equivalent to the Windows `scripts/check-windows.ps1 -Gate Continuous` loop.

**Why.** As planned, the entire Phase 2 inner loop is Windows-only: PowerShell 7,
`WGPU_BACKEND=dx12`, `WGPU_FORCE_FALLBACK_ADAPTER=1`, and an adapter preflight
that rejects anything whose name does not contain `Microsoft Basic Render
Driver` (KTD3, R25-R26). A developer on macOS — which is where this review was
run — cannot execute the primary gate at all, and so cannot verify any visual
change locally. Linux has `scripts/check.sh`, but that is the *milestone* gate:
it runs the full fourteen-frame journey, the release build, actionlint, sitegen
validation and the browser gate, and the equivalent job in CI currently takes
43+ minutes. There is no fast Linux or macOS path.

The practical consequence is a single point of failure: if the Windows devbox is
unavailable, Phase 2 visual work stops entirely.

**Context.** Raised during the architecture review as an unaddressed failure
scenario. No option was chosen, because the plan's WARP-authority decision
(KTD3) is deliberate and correct for *acceptance* — one reproducible adapter
avoids per-machine tolerance drift. The gap is in *iteration*, not acceptance.

The likely shape of the work: reuse the capture-profile abstraction that U2 adds
(and that 8A moves into `src/journey.rs`) to run the same five-frame continuous
journey under llvmpipe on Linux and under the macOS Metal or llvmpipe path,
reporting results as **advisory** rather than as an acceptance signal. U8's
adapter-calibration artifact (`docs/reference/adapter-calibration.json`) already
measures per-metric deltas between WARP, llvmpipe and SwiftShader, so the data
needed to state how far an advisory local result may drift from the WARP
authority will already exist.

Note the existing constraint recorded in `scripts/check.sh`: a missing display
or renderer must be a **hard failure, never a skip**, because a skipped render
gate is indistinguishable from a passing one. Any new local path must preserve
that property.

**Depends on / blocked by.** Depends on U2's capture-profile abstraction and on
8A's `src/journey.rs` extraction. Best done after U8 produces the adapter
calibration data, so advisory tolerances are measured rather than guessed. Also
depends on `tests/render_contract.rs` being stabilised by U0, since a local
advisory gate built on a flaky suite would be worse than none.
