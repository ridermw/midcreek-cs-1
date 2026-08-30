//! The clamped four-way isometric camera: heading, eased orbit, and the follow
//! clamp that keeps the ground footprint inside the rendered coverage apron.
//!
//! ```text
//! current interpolated yaw + fixed ortho rectangle
//!                      |
//!                      v
//! cast viewport corners onto Y=0
//!                      |
//!                      v
//! ground quadrilateral -> X/Z extents
//!                      |
//!                      v
//! RENDER_COVERAGE_SIZE minus extents = legal target rectangle
//!         (which contains the whole 40 m walkable room)
//!                      |
//!                      v
//! clamp followed player -> derive camera transform
//! ```
//!
//! The walkable room and the rendered coverage are two different squares. The
//! technician may only ever stand inside the 40 m room the perimeter walls
//! enclose; the camera may overhang that room freely, because the 72 m visual
//! apron beneath it is what actually gets rendered out there. The clamp is
//! therefore against [`RENDER_COVERAGE_SIZE`], not
//! [`ROOM_SIZE`](crate::design::ROOM_SIZE), and because
//! `72 / 2 - hypot(13, 8.71916) = 20.3468` m exceeds the room's 20 m half
//! extent, every legal player position is followed exactly, at every yaw. The
//! clamp only ever engages for a position the technician cannot reach.
//!
//! ```text
//! ButtonInput<KeyCode>
//!    |
//!    +-- Q just_pressed -----> counter-clockwise
//!    +-- E just_pressed -----> clockwise
//!    +-- Q and E on one frame -> cancel, tween untouched
//!                 |
//!                 v
//!        retarget desired heading now
//!                 |
//!                 v
//!  from = current interpolated yaw, to = from + shortest delta,
//!  duration = |delta| / (90 degrees per 0.30 s)
//!                 |
//!                 v
//!        smoothstep(elapsed / duration)
//!                 |
//!                 +--> ViewBasis (before MovePlayer)
//!                 |
//!                 +--> clamped follow -> camera Transform (FollowCamera)
//! ```
//!
//! Headings are named for the compass quadrant the camera itself occupies, on a
//! map whose north is `+Z` and whose east is `+X`, so the reviewed initial view
//! at yaw 45 degrees is [`CameraHeading::NorthEast`] and the declared order
//! `NE -> SE -> SW -> NW` is the clockwise one that `E` walks.
//!
//! This module is the sole runtime updater of
//! [`ViewBasis`](crate::player::ViewBasis). Movement still never reads the
//! camera entity: it reads the basis this module publishes every frame, before
//! [`CellShiftSet::MovePlayer`].

use bevy::{camera::ScalingMode, prelude::*};

use crate::{
    CellShiftSet,
    design::{
        CAMERA_ELEVATION_DEGREES, CAMERA_ORBIT_DURATION_SECONDS, INITIAL_CAMERA_YAW_DEGREES,
        ORTHOGRAPHIC_HEIGHT, ORTHOGRAPHIC_WIDTH, RENDER_COVERAGE_SIZE,
    },
    player::{Technician, ViewBasis},
};

/// Degrees between two adjacent headings.
pub const CAMERA_HEADING_STEP_DEGREES: f32 = 90.0;

/// Angular rate every orbit runs at, in degrees per second.
///
/// A settled quarter turn therefore takes exactly
/// [`CAMERA_ORBIT_DURATION_SECONDS`], and a tween retargeted mid-flight keeps
/// this same average rate, so its duration scales with the shortest remaining
/// angular distance instead of restarting a fixed 0.30-second clock.
pub const CAMERA_ORBIT_SPEED_DEGREES_PER_SECOND: f32 =
    CAMERA_HEADING_STEP_DEGREES / CAMERA_ORBIT_DURATION_SECONDS;

/// Distance from the followed ground point to the camera, in metres.
///
/// The projection is orthographic, so this changes nothing about framing or
/// scale; it only has to hold the whole room between the near and far planes.
pub const CAMERA_DISTANCE: f32 = 60.0;

/// Near clipping plane, in metres.
pub const CAMERA_NEAR: f32 = 0.1;

/// Far clipping plane, in metres.
pub const CAMERA_FAR: f32 = 200.0;

// ---------------------------------------------------------------------------
// Heading
// ---------------------------------------------------------------------------

/// One of the four reviewed isometric headings.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CameraHeading {
    /// The reviewed initial view, yaw 45 degrees.
    #[default]
    NorthEast,
    /// Yaw 135 degrees.
    SouthEast,
    /// Yaw 225 degrees.
    SouthWest,
    /// Yaw 315 degrees.
    NorthWest,
}

impl CameraHeading {
    /// Every heading in clockwise order, starting at the reviewed initial view.
    pub const ALL: [Self; 4] = [
        Self::NorthEast,
        Self::SouthEast,
        Self::SouthWest,
        Self::NorthWest,
    ];

    /// Clockwise index of this heading, starting at zero for `NorthEast`.
    pub const fn index(self) -> usize {
        match self {
            Self::NorthEast => 0,
            Self::SouthEast => 1,
            Self::SouthWest => 2,
            Self::NorthWest => 3,
        }
    }

    /// Settled camera yaw for this heading, in degrees.
    pub fn yaw_degrees(self) -> f32 {
        INITIAL_CAMERA_YAW_DEGREES + CAMERA_HEADING_STEP_DEGREES * self.index() as f32
    }

    /// Settled camera yaw for this heading, in radians.
    pub fn yaw_radians(self) -> f32 {
        self.yaw_degrees().to_radians()
    }

    /// The heading a settled yaw names, or `None` for a yaw between headings.
    pub fn from_yaw_degrees(degrees: f32) -> Option<Self> {
        let steps = (degrees - INITIAL_CAMERA_YAW_DEGREES) / CAMERA_HEADING_STEP_DEGREES;
        (steps.fract().abs() < 1.0e-4)
            .then(|| Self::ALL[(steps.round() as i64).rem_euclid(4) as usize])
    }

    /// The next heading clockwise, which is what `E` requests.
    pub const fn clockwise(self) -> Self {
        Self::ALL[(self.index() + 1) % 4]
    }

    /// The next heading counter-clockwise, which is what `Q` requests.
    pub const fn counter_clockwise(self) -> Self {
        Self::ALL[(self.index() + 3) % 4]
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// The orbit request one frame of real keyboard state carries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OrbitInput {
    /// Neither key was newly pressed, or both were and they cancelled.
    #[default]
    None,
    /// `E`: one quarter turn clockwise.
    Clockwise,
    /// `Q`: one quarter turn counter-clockwise.
    CounterClockwise,
}

/// Reads the real `Q`/`E` state. Only a new press requests a turn, so holding a
/// key never spins the camera, and opposite keys on one frame cancel exactly.
pub fn orbit_input(keys: &ButtonInput<KeyCode>) -> OrbitInput {
    match (
        keys.just_pressed(KeyCode::KeyQ),
        keys.just_pressed(KeyCode::KeyE),
    ) {
        (true, false) => OrbitInput::CounterClockwise,
        (false, true) => OrbitInput::Clockwise,
        _ => OrbitInput::None,
    }
}

// ---------------------------------------------------------------------------
// Orbit state
// ---------------------------------------------------------------------------

/// Smoothstep easing over a normalized `0..=1` parameter.
pub fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Wraps a yaw into `0..TAU`.
pub fn normalize_yaw(radians: f32) -> f32 {
    radians.rem_euclid(std::f32::consts::TAU)
}

/// Shortest signed angular delta from `from` to `to`, in radians. A half turn
/// resolves clockwise so the tie is never left to floating-point sign noise.
pub fn shortest_yaw_delta(from: f32, to: f32) -> f32 {
    let raw = (to - from).rem_euclid(std::f32::consts::TAU);
    if raw > std::f32::consts::PI {
        raw - std::f32::consts::TAU
    } else {
        raw
    }
}

/// The desired heading and the eased yaw currently between headings.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct CameraOrbit {
    heading: CameraHeading,
    from_yaw: f32,
    to_yaw: f32,
    elapsed: f32,
    duration: f32,
}

impl Default for CameraOrbit {
    fn default() -> Self {
        Self::settled(CameraHeading::default())
    }
}

impl CameraOrbit {
    /// A settled orbit at `heading`.
    pub fn settled(heading: CameraHeading) -> Self {
        let yaw = heading.yaw_radians();
        Self {
            heading,
            from_yaw: yaw,
            to_yaw: yaw,
            elapsed: 0.0,
            duration: 0.0,
        }
    }

    /// The heading the orbit is turning towards, retargeted the moment a key is
    /// pressed rather than when the current turn finishes.
    pub fn heading(self) -> CameraHeading {
        self.heading
    }

    /// Eased progress through the current turn.
    pub fn progress(self) -> f32 {
        if self.duration <= 0.0 {
            return 1.0;
        }
        smoothstep(self.elapsed / self.duration)
    }

    /// Current interpolated yaw, in radians, wrapped into `0..TAU`.
    pub fn yaw_radians(self) -> f32 {
        normalize_yaw(self.from_yaw + (self.to_yaw - self.from_yaw) * self.progress())
    }

    /// Current interpolated yaw, in degrees, wrapped into `0..360`.
    pub fn yaw_degrees(self) -> f32 {
        self.yaw_radians().to_degrees()
    }

    /// Whether the current turn has finished.
    pub fn is_settled(self) -> bool {
        self.elapsed >= self.duration
    }

    /// Total duration of the current turn, in seconds.
    pub fn duration_seconds(self) -> f32 {
        self.duration
    }

    /// Seconds of the current turn still to run.
    pub fn remaining_seconds(self) -> f32 {
        (self.duration - self.elapsed).max(0.0)
    }

    /// The basis movement reads, derived from the current interpolated yaw.
    pub fn basis(self) -> ViewBasis {
        ViewBasis::from_yaw_radians(self.yaw_radians())
    }

    /// Retargets to `heading` from the current interpolated yaw, keeping the
    /// reviewed angular rate, so the new turn is shorter than a quarter turn
    /// exactly when less than a quarter turn remains.
    pub fn retarget(&mut self, heading: CameraHeading) {
        let from = self.yaw_radians();
        let delta = shortest_yaw_delta(from, heading.yaw_radians());
        self.heading = heading;
        self.from_yaw = from;
        self.to_yaw = from + delta;
        self.elapsed = 0.0;
        self.duration = delta.abs().to_degrees() / CAMERA_ORBIT_SPEED_DEGREES_PER_SECOND;
    }

    /// Applies one frame of real orbit input.
    pub fn apply(&mut self, input: OrbitInput) {
        match input {
            OrbitInput::None => {}
            OrbitInput::Clockwise => self.retarget(self.heading.clockwise()),
            OrbitInput::CounterClockwise => self.retarget(self.heading.counter_clockwise()),
        }
    }

    /// Advances the current turn. A non-finite or negative delta stalls the
    /// orbit instead of poisoning the yaw.
    pub fn advance(&mut self, delta_secs: f32) {
        if !delta_secs.is_finite() || delta_secs <= 0.0 {
            return;
        }
        self.elapsed = (self.elapsed + delta_secs).min(self.duration);
    }
}

// ---------------------------------------------------------------------------
// Ground footprint and clamp
// ---------------------------------------------------------------------------

/// Half the width of the orthographic rectangle on the ground, in metres. The
/// camera has zero roll, so screen horizontal is already horizontal in world.
pub fn ground_half_width() -> f32 {
    ORTHOGRAPHIC_WIDTH * 0.5
}

/// Half the depth of the orthographic rectangle once cast onto `Y = 0`. The
/// fixed elevation foreshortens screen vertical by `sin(elevation)`.
pub fn ground_half_depth() -> f32 {
    ORTHOGRAPHIC_HEIGHT * 0.5 / CAMERA_ELEVATION_DEGREES.to_radians().sin()
}

/// The four viewport corners cast onto `Y = 0`, around `target`.
pub fn ground_quadrilateral(yaw_radians: f32, target: Vec2) -> [Vec2; 4] {
    let basis = ViewBasis::from_yaw_radians(yaw_radians);
    let right = basis.right() * ground_half_width();
    let forward = basis.forward() * ground_half_depth();
    [
        target - right - forward,
        target + right - forward,
        target + right + forward,
        target - right + forward,
    ]
}

/// Axis-aligned half extents of the ground quadrilateral.
pub fn ground_footprint_extents(yaw_radians: f32) -> Vec2 {
    ground_quadrilateral(yaw_radians, Vec2::ZERO)
        .into_iter()
        .fold(Vec2::ZERO, |extents, corner| extents.max(corner.abs()))
}

/// The legal follow-target rectangle: rendered coverage minus the ground
/// footprint. `None` when the footprint is wider than the coverage on either
/// axis.
///
/// `coverage` is the square the camera is allowed to render — the visual apron,
/// not the walkable room. Clamping against the room instead would push the
/// camera off any technician standing near a wall.
pub fn camera_target_bounds(coverage: Vec2, yaw_radians: f32) -> Option<(Vec2, Vec2)> {
    let remaining = coverage * 0.5 - ground_footprint_extents(yaw_radians);
    (remaining.min_element() >= 0.0).then_some((-remaining, remaining))
}

/// Whether every legal position in a walkable `room` is also a legal follow
/// target under `coverage` at `yaw_radians`, so the camera never has to stop
/// tracking the technician.
pub fn coverage_holds_room(room: Vec2, coverage: Vec2, yaw_radians: f32) -> bool {
    camera_target_bounds(coverage, yaw_radians)
        .is_some_and(|(_, max)| max.x >= room.x * 0.5 && max.y >= room.y * 0.5)
}

/// The two yaws at which the ground footprint is widest, one per world axis.
///
/// Each axis extent is `half_width * |cos yaw| + half_depth * |sin yaw|` with
/// the roles swapped, so each peaks at `hypot(half_width, half_depth)` between
/// two settled headings rather than at one of them. Sampling only the four
/// reviewed views would therefore miss the tightest legal rectangle of a whole
/// orbit, which is exactly the state a mid-tween frame is in.
pub fn widest_footprint_yaws() -> [f32; 2] {
    let peak = ground_half_depth().atan2(ground_half_width());
    [peak, std::f32::consts::FRAC_PI_2 - peak]
}

/// Clamps the followed player into the legal rectangle. Coverage too small for
/// the footprint has no legal target at all, so the camera holds the centre
/// rather than tracking a player it cannot follow without leaking past the
/// rendered apron.
pub fn clamp_follow_target(player: Vec2, coverage: Vec2, yaw_radians: f32) -> Vec2 {
    match camera_target_bounds(coverage, yaw_radians) {
        Some((min, max)) => player.clamp(min, max),
        None => Vec2::ZERO,
    }
}

/// Unit direction from the followed ground point towards the camera.
pub fn camera_offset_direction(yaw_radians: f32) -> Vec3 {
    let elevation = CAMERA_ELEVATION_DEGREES.to_radians();
    let offset = ViewBasis::from_yaw_radians(yaw_radians).camera_offset();
    Vec3::new(
        elevation.cos() * offset.x,
        elevation.sin(),
        elevation.cos() * offset.y,
    )
}

/// The camera transform for one yaw and one already-clamped ground target.
/// Zoom, elevation, and roll are fixed; only yaw and translation move.
pub fn camera_transform(yaw_radians: f32, target: Vec2) -> Transform {
    let focus = Vec3::new(target.x, 0.0, target.y);
    Transform::from_translation(focus + camera_offset_direction(yaw_radians) * CAMERA_DISTANCE)
        .looking_at(focus, Vec3::Y)
}

/// The reviewed orthographic projection: a fixed 26 m by 14.625 m rectangle
/// that never changes with window size.
pub fn orthographic_projection() -> Projection {
    Projection::Orthographic(OrthographicProjection {
        scaling_mode: ScalingMode::Fixed {
            width: ORTHOGRAPHIC_WIDTH,
            height: ORTHOGRAPHIC_HEIGHT,
        },
        near: CAMERA_NEAR,
        far: CAMERA_FAR,
        ..OrthographicProjection::default_3d()
    })
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// The one game camera.
#[derive(Component, Clone, Copy, Debug)]
pub struct CellShiftCamera;

/// Spawns the orthographic camera, orbits it from real `Q`/`E` input, and
/// publishes [`ViewBasis`] before movement reads it.
pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewBasis>()
            .init_resource::<CameraOrbit>()
            .add_systems(Startup, spawn_camera)
            .add_systems(Update, read_orbit_input.in_set(CellShiftSet::ReadInput))
            .add_systems(
                Update,
                advance_orbit.in_set(CellShiftSet::UpdateOrbitIntent),
            )
            .add_systems(Update, follow_player.in_set(CellShiftSet::FollowCamera));
    }
}

fn spawn_camera(mut commands: Commands, orbit: Res<CameraOrbit>) {
    commands.spawn((
        CellShiftCamera,
        Name::new("cell-shift-camera"),
        Camera3d::default(),
        orthographic_projection(),
        camera_transform(orbit.yaw_radians(), Vec2::ZERO),
    ));
}

fn read_orbit_input(keys: Res<ButtonInput<KeyCode>>, mut orbit: ResMut<CameraOrbit>) {
    let input = orbit_input(&keys);
    if input != OrbitInput::None {
        orbit.apply(input);
    }
}

fn advance_orbit(time: Res<Time>, mut orbit: ResMut<CameraOrbit>, mut basis: ResMut<ViewBasis>) {
    if !orbit.is_settled() {
        orbit.advance(time.delta_secs());
    }
    let updated = orbit.basis();
    if *basis != updated {
        *basis = updated;
    }
}

fn follow_player(
    orbit: Res<CameraOrbit>,
    players: Query<&Transform, (With<Technician>, Without<CellShiftCamera>)>,
    mut cameras: Query<&mut Transform, With<CellShiftCamera>>,
) {
    let followed = players
        .iter()
        .next()
        .map(|transform| Vec2::new(transform.translation.x, transform.translation.z))
        .unwrap_or(Vec2::ZERO);
    let yaw = orbit.yaw_radians();
    let target = clamp_follow_target(followed, RENDER_COVERAGE_SIZE, yaw);
    for mut transform in &mut cameras {
        *transform = camera_transform(yaw, target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::{CAMERA_OFFSET_DIRECTION, PLAYER_RADIUS, ROOM_SIZE};

    const YAW_TOLERANCE_DEGREES: f32 = 1.0e-3;

    fn keys(pressed: &[KeyCode]) -> ButtonInput<KeyCode> {
        let mut input = ButtonInput::default();
        for key in pressed {
            input.press(*key);
        }
        input
    }

    fn held(pressed: &[KeyCode]) -> ButtonInput<KeyCode> {
        let mut input = keys(pressed);
        input.clear();
        input
    }

    fn assert_yaw(actual_radians: f32, expected_degrees: f32, context: &str) {
        let actual = actual_radians.to_degrees();
        let delta = shortest_yaw_delta(actual_radians, expected_degrees.to_radians())
            .to_degrees()
            .abs();
        assert!(
            delta < YAW_TOLERANCE_DEGREES,
            "{context}: expected yaw {expected_degrees} degrees, got {actual} degrees"
        );
    }

    fn press(orbit: &mut CameraOrbit, pressed: &[KeyCode]) {
        orbit.apply(orbit_input(&keys(pressed)));
    }

    // -----------------------------------------------------------------------
    // Heading
    // -----------------------------------------------------------------------

    #[test]
    fn camera_headings_map_to_the_four_reviewed_yaws() {
        assert_eq!(
            CameraHeading::ALL,
            [
                CameraHeading::NorthEast,
                CameraHeading::SouthEast,
                CameraHeading::SouthWest,
                CameraHeading::NorthWest,
            ]
        );
        assert_eq!(CameraHeading::default(), CameraHeading::NorthEast);
        assert_eq!(
            CameraHeading::NorthEast.yaw_degrees(),
            INITIAL_CAMERA_YAW_DEGREES
        );
        for (heading, degrees) in CameraHeading::ALL
            .into_iter()
            .zip([45.0_f32, 135.0, 225.0, 315.0])
        {
            assert_eq!(heading.yaw_degrees(), degrees, "{heading:?}");
            assert_eq!(heading.yaw_radians(), degrees.to_radians(), "{heading:?}");
            assert_eq!(CameraHeading::from_yaw_degrees(degrees), Some(heading));
            assert_eq!(
                CameraHeading::from_yaw_degrees(degrees + 360.0),
                Some(heading),
                "{heading:?} must survive a full wrap"
            );
            assert_eq!(
                CameraHeading::from_yaw_degrees(degrees - 360.0),
                Some(heading),
                "{heading:?} must survive a negative wrap"
            );
        }
        assert_eq!(CameraHeading::from_yaw_degrees(0.0), None);
        assert_eq!(CameraHeading::from_yaw_degrees(90.0), None);
        assert_eq!(CameraHeading::from_yaw_degrees(46.0), None);
    }

    #[test]
    fn camera_heading_steps_walk_every_quadrant_in_both_directions() {
        let mut clockwise = CameraHeading::NorthEast;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(clockwise);
            clockwise = clockwise.clockwise();
        }
        assert_eq!(seen, CameraHeading::ALL.to_vec());
        assert_eq!(clockwise, CameraHeading::NorthEast, "clockwise must wrap");

        let mut counter = CameraHeading::NorthEast;
        let mut back = Vec::new();
        for _ in 0..4 {
            back.push(counter);
            counter = counter.counter_clockwise();
        }
        assert_eq!(
            back,
            vec![
                CameraHeading::NorthEast,
                CameraHeading::NorthWest,
                CameraHeading::SouthWest,
                CameraHeading::SouthEast,
            ]
        );
        assert_eq!(counter, CameraHeading::NorthEast, "counter must wrap");

        for heading in CameraHeading::ALL {
            assert_eq!(heading.clockwise().counter_clockwise(), heading);
            assert_yaw(
                heading.clockwise().yaw_radians(),
                heading.yaw_degrees() + 90.0,
                "clockwise step",
            );
            assert_yaw(
                heading.counter_clockwise().yaw_radians(),
                heading.yaw_degrees() - 90.0,
                "counter-clockwise step",
            );
        }
    }

    #[test]
    fn camera_heading_names_the_quadrant_the_camera_occupies() {
        // North is +Z and east is +X, so each heading must place the camera in
        // the quadrant it is named for.
        for (heading, quadrant) in CameraHeading::ALL.into_iter().zip([
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, -1.0),
            Vec2::new(-1.0, -1.0),
            Vec2::new(-1.0, 1.0),
        ]) {
            let offset = camera_offset_direction(heading.yaw_radians());
            assert_eq!(
                Vec2::new(offset.x.signum(), offset.z.signum()),
                quadrant,
                "{heading:?} camera offset {offset:?}"
            );
            assert!(offset.y > 0.0, "{heading:?} must look down, not up");
        }
    }

    // -----------------------------------------------------------------------
    // Input
    // -----------------------------------------------------------------------

    #[test]
    fn camera_orbit_input_reads_real_new_presses_and_cancels_opposing_keys() {
        assert_eq!(orbit_input(&keys(&[])), OrbitInput::None);
        assert_eq!(
            orbit_input(&keys(&[KeyCode::KeyQ])),
            OrbitInput::CounterClockwise
        );
        assert_eq!(orbit_input(&keys(&[KeyCode::KeyE])), OrbitInput::Clockwise);
        assert_eq!(
            orbit_input(&keys(&[KeyCode::KeyQ, KeyCode::KeyE])),
            OrbitInput::None,
            "opposite keys on one frame must cancel"
        );
        assert_eq!(
            orbit_input(&held(&[KeyCode::KeyE])),
            OrbitInput::None,
            "a held key must not keep spinning the camera"
        );
        assert_eq!(orbit_input(&held(&[KeyCode::KeyQ])), OrbitInput::None);
        assert_eq!(
            orbit_input(&keys(&[KeyCode::KeyE, KeyCode::ArrowUp])),
            OrbitInput::Clockwise,
            "an unrelated key must not disturb the orbit request"
        );
    }

    // -----------------------------------------------------------------------
    // Tween
    // -----------------------------------------------------------------------

    #[test]
    fn camera_orbit_starts_settled_at_the_reviewed_initial_view() {
        let orbit = CameraOrbit::default();

        assert_eq!(orbit.heading(), CameraHeading::NorthEast);
        assert_yaw(orbit.yaw_radians(), INITIAL_CAMERA_YAW_DEGREES, "initial");
        assert!(orbit.is_settled());
        assert_eq!(orbit.duration_seconds(), 0.0);
        assert_eq!(orbit.remaining_seconds(), 0.0);
        assert_eq!(orbit.basis(), ViewBasis::default());
    }

    #[test]
    fn camera_orbit_settled_quarter_turn_takes_exactly_the_reviewed_duration() {
        let mut orbit = CameraOrbit::default();
        press(&mut orbit, &[KeyCode::KeyE]);

        assert_eq!(orbit.heading(), CameraHeading::SouthEast);
        assert_eq!(orbit.duration_seconds(), CAMERA_ORBIT_DURATION_SECONDS);
        assert!(!orbit.is_settled());
        assert_yaw(orbit.yaw_radians(), 45.0, "tween start");

        orbit.advance(CAMERA_ORBIT_DURATION_SECONDS - 0.001);
        assert!(
            !orbit.is_settled(),
            "the turn must still be running one millisecond early"
        );
        assert!(
            (orbit.yaw_degrees() - 135.0).abs() > 1.0e-5,
            "the turn must not have arrived one millisecond early"
        );

        orbit.advance(0.001);
        assert!(orbit.is_settled(), "the turn must end at exactly 0.30 s");
        assert_yaw(orbit.yaw_radians(), 135.0, "settled south-east");
        assert_eq!(orbit.remaining_seconds(), 0.0);
    }

    #[test]
    fn camera_orbit_eases_with_smoothstep_and_passes_through_the_exact_midpoint() {
        assert_eq!(smoothstep(0.0), 0.0);
        assert_eq!(smoothstep(0.5), 0.5);
        assert_eq!(smoothstep(1.0), 1.0);
        assert_eq!(smoothstep(-1.0), 0.0);
        assert_eq!(smoothstep(2.0), 1.0);
        assert_eq!(smoothstep(0.25), 0.156_25);

        let mut orbit = CameraOrbit::default();
        press(&mut orbit, &[KeyCode::KeyE]);
        orbit.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.5);
        assert_yaw(orbit.yaw_radians(), 90.0, "smoothstep midpoint");

        let mut quarter = CameraOrbit::default();
        press(&mut quarter, &[KeyCode::KeyE]);
        quarter.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.25);
        assert_yaw(
            quarter.yaw_radians(),
            45.0 + 90.0 * 0.156_25,
            "smoothstep quarter",
        );
        assert!(
            (quarter.yaw_degrees() - (45.0 + 90.0 * 0.25)).abs() > 1.0,
            "the eased quarter must not be the linear quarter, got {}",
            quarter.yaw_degrees()
        );
    }

    #[test]
    fn camera_orbit_walks_every_heading_in_both_directions_including_wraparound() {
        let mut orbit = CameraOrbit::default();
        for expected in [
            CameraHeading::SouthEast,
            CameraHeading::SouthWest,
            CameraHeading::NorthWest,
            CameraHeading::NorthEast,
        ] {
            press(&mut orbit, &[KeyCode::KeyE]);
            orbit.advance(CAMERA_ORBIT_DURATION_SECONDS);
            assert_eq!(orbit.heading(), expected);
            assert_yaw(
                orbit.yaw_radians(),
                expected.yaw_degrees(),
                "clockwise walk",
            );
        }

        for expected in [
            CameraHeading::NorthWest,
            CameraHeading::SouthWest,
            CameraHeading::SouthEast,
            CameraHeading::NorthEast,
        ] {
            press(&mut orbit, &[KeyCode::KeyQ]);
            orbit.advance(CAMERA_ORBIT_DURATION_SECONDS);
            assert_eq!(orbit.heading(), expected);
            assert_yaw(
                orbit.yaw_radians(),
                expected.yaw_degrees(),
                "counter-clockwise walk",
            );
        }
    }

    #[test]
    fn camera_orbit_wraps_across_zero_without_taking_the_long_way_round() {
        let mut orbit = CameraOrbit::settled(CameraHeading::NorthWest);
        press(&mut orbit, &[KeyCode::KeyE]);
        assert_eq!(orbit.duration_seconds(), CAMERA_ORBIT_DURATION_SECONDS);
        orbit.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.5);
        assert_yaw(orbit.yaw_radians(), 0.0, "wrap through zero clockwise");
        orbit.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.5);
        assert_yaw(orbit.yaw_radians(), 45.0, "wrapped to north-east");

        press(&mut orbit, &[KeyCode::KeyQ]);
        orbit.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.5);
        assert_yaw(orbit.yaw_radians(), 0.0, "wrap through zero counter");
        orbit.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.5);
        assert_yaw(orbit.yaw_radians(), 315.0, "wrapped to north-west");
    }

    #[test]
    fn camera_orbit_opposing_keys_on_one_frame_leave_the_tween_untouched() {
        let mut settled = CameraOrbit::default();
        press(&mut settled, &[KeyCode::KeyQ, KeyCode::KeyE]);
        assert_eq!(settled, CameraOrbit::default());

        let mut running = CameraOrbit::default();
        press(&mut running, &[KeyCode::KeyE]);
        running.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.5);
        let before = running;
        press(&mut running, &[KeyCode::KeyQ, KeyCode::KeyE]);
        assert_eq!(
            running, before,
            "a cancelled frame must not retarget, restart, or re-time the turn"
        );
        running.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.5);
        assert_eq!(running.heading(), CameraHeading::SouthEast);
        assert_yaw(
            running.yaw_radians(),
            135.0,
            "cancelled frame still arrives",
        );
    }

    #[test]
    fn camera_orbit_retarget_mid_tween_starts_at_the_interpolated_yaw() {
        let mut reversed = CameraOrbit::default();
        press(&mut reversed, &[KeyCode::KeyE]);
        reversed.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.5);
        assert_yaw(reversed.yaw_radians(), 90.0, "midpoint before reversing");

        press(&mut reversed, &[KeyCode::KeyQ]);
        assert_eq!(reversed.heading(), CameraHeading::NorthEast);
        assert_yaw(
            reversed.yaw_radians(),
            90.0,
            "reversal must start where the tween had reached",
        );
        assert!(
            (reversed.duration_seconds() - CAMERA_ORBIT_DURATION_SECONDS * 0.5).abs() < 1.0e-6,
            "45 degrees at 90 degrees per 0.30 s must take 0.15 s, got {}",
            reversed.duration_seconds()
        );
        reversed.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.5);
        assert!(reversed.is_settled());
        assert_yaw(reversed.yaw_radians(), 45.0, "reversed back to north-east");
    }

    #[test]
    fn camera_orbit_retarget_mid_tween_keeps_a_constant_angular_speed() {
        let mut queued = CameraOrbit::default();
        press(&mut queued, &[KeyCode::KeyE]);
        queued.advance(CAMERA_ORBIT_DURATION_SECONDS * 0.5);
        press(&mut queued, &[KeyCode::KeyE]);

        assert_eq!(queued.heading(), CameraHeading::SouthWest);
        assert_yaw(queued.yaw_radians(), 90.0, "queued turn starts at 90");
        assert!(
            (queued.duration_seconds() - CAMERA_ORBIT_DURATION_SECONDS * 1.5).abs() < 1.0e-6,
            "135 degrees at 90 degrees per 0.30 s must take 0.45 s, got {}",
            queued.duration_seconds()
        );

        for (elapsed, expected) in [
            (0.0_f32, 90.0_f32),
            (CAMERA_ORBIT_DURATION_SECONDS * 0.75, 157.5),
            (CAMERA_ORBIT_DURATION_SECONDS * 1.5, 225.0),
        ] {
            let mut sample = queued;
            sample.advance(elapsed);
            assert_yaw(sample.yaw_radians(), expected, "queued turn sample");
        }

        // Every retarget, at any point of any tween, keeps the same rate.
        for fraction in [0.0_f32, 0.1, 0.37, 0.5, 0.9, 1.0] {
            let mut orbit = CameraOrbit::default();
            press(&mut orbit, &[KeyCode::KeyE]);
            orbit.advance(CAMERA_ORBIT_DURATION_SECONDS * fraction);
            let before = orbit.yaw_radians();
            press(&mut orbit, &[KeyCode::KeyQ]);
            let travelled = shortest_yaw_delta(before, orbit.heading().yaw_radians())
                .abs()
                .to_degrees();
            let expected = travelled / CAMERA_ORBIT_SPEED_DEGREES_PER_SECOND;
            assert!(
                (orbit.duration_seconds() - expected).abs() < 1.0e-6,
                "retarget at {fraction} must take {expected} s for {travelled} degrees, got {}",
                orbit.duration_seconds()
            );
        }
    }

    #[test]
    fn camera_orbit_retarget_to_the_settled_heading_is_an_instant_no_op() {
        let mut orbit = CameraOrbit::default();
        orbit.retarget(CameraHeading::NorthEast);

        assert!(orbit.is_settled());
        assert_eq!(orbit.duration_seconds(), 0.0);
        assert_yaw(orbit.yaw_radians(), 45.0, "no-op retarget");
    }

    #[test]
    fn camera_orbit_advance_ignores_a_poisoned_or_reversed_delta() {
        let mut orbit = CameraOrbit::default();
        press(&mut orbit, &[KeyCode::KeyE]);
        orbit.advance(f32::NAN);
        orbit.advance(-1.0);
        assert_yaw(orbit.yaw_radians(), 45.0, "poisoned delta must stall");
        assert!(orbit.yaw_radians().is_finite());

        orbit.advance(CAMERA_ORBIT_DURATION_SECONDS * 10.0);
        assert!(orbit.is_settled(), "a huge delta must clamp, not overshoot");
        assert_yaw(orbit.yaw_radians(), 135.0, "overshoot clamps to the target");
    }

    #[test]
    fn camera_orbit_publishes_the_interpolated_basis_at_every_step() {
        let mut orbit = CameraOrbit::default();
        press(&mut orbit, &[KeyCode::KeyE]);
        for _ in 0..30 {
            orbit.advance(CAMERA_ORBIT_DURATION_SECONDS / 30.0);
            assert_eq!(
                orbit.basis(),
                ViewBasis::from_yaw_radians(orbit.yaw_radians())
            );
            assert_eq!(
                orbit.basis().forward(),
                -orbit.basis().camera_offset(),
                "the basis must look along the camera"
            );
        }
        assert_eq!(orbit.basis(), ViewBasis::from_yaw_degrees(135.0));
    }

    #[test]
    fn camera_shortest_yaw_delta_never_takes_the_long_way_round() {
        for (from, to, expected) in [
            (45.0_f32, 135.0_f32, 90.0_f32),
            (315.0, 45.0, 90.0),
            (45.0, 315.0, -90.0),
            (0.0, 359.0, -1.0),
            (359.0, 0.0, 1.0),
            (45.0, 45.0, 0.0),
            (45.0, 225.0, 180.0),
        ] {
            let delta = shortest_yaw_delta(from.to_radians(), to.to_radians()).to_degrees();
            assert!(
                (delta - expected).abs() < YAW_TOLERANCE_DEGREES,
                "shortest delta {from} -> {to} should be {expected}, got {delta}"
            );
        }
        assert_eq!(
            normalize_yaw(-std::f32::consts::FRAC_PI_2),
            std::f32::consts::TAU - std::f32::consts::FRAC_PI_2
        );
    }

    // -----------------------------------------------------------------------
    // Footprint and clamp
    // -----------------------------------------------------------------------

    #[test]
    fn camera_ground_rectangle_is_the_projected_orthographic_viewport() {
        assert_eq!(ground_half_width(), ORTHOGRAPHIC_WIDTH * 0.5);
        let expected_depth =
            ORTHOGRAPHIC_HEIGHT * 0.5 / CAMERA_ELEVATION_DEGREES.to_radians().sin();
        assert!((ground_half_depth() - expected_depth).abs() < 1.0e-6);
        assert!(
            (ground_half_depth() - 8.719_16).abs() < 1.0e-4,
            "half ground depth should be 8.71916 m, got {}",
            ground_half_depth()
        );

        for heading in CameraHeading::ALL {
            let yaw = heading.yaw_radians();
            let quad = ground_quadrilateral(yaw, Vec2::new(2.0, -3.0));
            let basis = ViewBasis::from_yaw_radians(yaw);
            for corner in quad {
                let offset = corner - Vec2::new(2.0, -3.0);
                assert!(
                    (offset.dot(basis.right()).abs() - ground_half_width()).abs() < 1.0e-4,
                    "{heading:?} corner {corner:?} is not on the viewport edge"
                );
                assert!(
                    (offset.dot(basis.forward()).abs() - ground_half_depth()).abs() < 1.0e-4,
                    "{heading:?} corner {corner:?} is not on the viewport edge"
                );
            }
        }
    }

    #[test]
    fn camera_ground_footprint_extents_match_the_rotated_rectangle() {
        for (degrees, expected) in [
            (0.0_f32, Vec2::new(13.0, 8.719_43)),
            (90.0, Vec2::new(8.719_43, 13.0)),
            (180.0, Vec2::new(13.0, 8.719_43)),
            (
                45.0,
                Vec2::splat((13.0 + 8.719_16) * 0.5 * std::f32::consts::SQRT_2),
            ),
        ] {
            let extents = ground_footprint_extents(degrees.to_radians());
            assert!(
                (extents - expected).abs().max_element() < 1.0e-3,
                "yaw {degrees} should have extents {expected:?}, got {extents:?}"
            );
        }

        // The footprint is widest between the headings, not at one, so the
        // smallest legal rectangle of a whole orbit is never sampled by the
        // four settled views alone.
        let widest = ground_half_width().hypot(ground_half_depth());
        assert!(
            (widest - 15.653_2).abs() < 1.0e-3,
            "the widest footprint extent should be 15.6532 m, got {widest}"
        );
        for yaw in widest_footprint_yaws() {
            let extents = ground_footprint_extents(yaw);
            assert!(
                (extents.max_element() - widest).abs() < 1.0e-3,
                "yaw {} should attain the widest extent {widest}, got {extents:?}",
                yaw.to_degrees()
            );
        }
        assert!(
            (widest_footprint_yaws()[0].to_degrees() - 33.851).abs() < 1.0e-2,
            "the widest yaw should be 33.851 degrees, got {}",
            widest_footprint_yaws()[0].to_degrees()
        );

        let diagonal = ground_footprint_extents(CameraHeading::NorthEast.yaw_radians());
        assert!(
            diagonal.max_element() < widest,
            "a settled heading {diagonal:?} must not be the worst case {widest}"
        );
        for step in 0..3600 {
            let extents = ground_footprint_extents((step as f32 * 0.1).to_radians());
            assert!(
                extents.max_element() <= widest + 1.0e-3,
                "yaw {} extents {extents:?} exceed the closed-form worst case {widest}",
                step as f32 * 0.1
            );
        }
    }

    #[test]
    fn camera_target_bounds_hold_the_whole_walkable_room_at_every_yaw() {
        let half_room = ROOM_SIZE * 0.5;
        for step in 0..720 {
            let degrees = step as f32 * 0.5;
            let yaw = degrees.to_radians();
            let bounds = camera_target_bounds(RENDER_COVERAGE_SIZE, yaw);
            let (min, max) = bounds
                .unwrap_or_else(|| panic!("yaw {degrees} must have a legal target rectangle"));
            assert_eq!(min, -max);
            assert!(
                max.x >= half_room.x && max.y >= half_room.y,
                "yaw {degrees} legal rectangle {max:?} does not hold the walkable room {half_room:?}"
            );
            assert!(
                coverage_holds_room(ROOM_SIZE, RENDER_COVERAGE_SIZE, yaw),
                "yaw {degrees} cannot follow the technician everywhere in the room"
            );
        }

        let diagonal =
            camera_target_bounds(RENDER_COVERAGE_SIZE, CameraHeading::NorthEast.yaw_radians())
                .expect("the reviewed initial view must have a legal rectangle");
        assert!(
            (diagonal.1 - Vec2::splat(20.641_6)).abs().max_element() < 1.0e-3,
            "the diagonal legal rectangle should reach 20.6416 m, got {:?}",
            diagonal.1
        );

        // The tightest legal rectangle of a whole orbit still contains the
        // 20 m room half extent, with 0.3468 m of authored slack to spare.
        let tightest = RENDER_COVERAGE_SIZE * 0.5
            - Vec2::splat(ground_half_width().hypot(ground_half_depth()));
        assert!(
            (tightest - Vec2::splat(20.346_8)).abs().max_element() < 1.0e-3,
            "the tightest legal rectangle of a whole orbit should reach 20.3468 m, got {tightest:?}"
        );
        assert!(
            ((tightest - half_room) - Vec2::splat(0.346_8))
                .abs()
                .max_element()
                < 1.0e-3,
            "the worst-case coverage slack should be 0.3468 m, got {:?}",
            tightest - half_room
        );

        // Coverage narrower than the footprint still has no legal target.
        assert_eq!(
            camera_target_bounds(Vec2::splat(30.0), CameraHeading::NorthEast.yaw_radians()),
            None
        );
        assert_eq!(camera_target_bounds(Vec2::new(25.0, 72.0), 0.0), None);

        // Clamping against the 40 m room instead of the apron is exactly the
        // rejected design: it cannot follow the technician to a wall.
        assert!(
            !coverage_holds_room(ROOM_SIZE, ROOM_SIZE, CameraHeading::NorthEast.yaw_radians()),
            "the walkable room is far too small to be its own rendered coverage"
        );
    }

    #[test]
    fn camera_follows_every_legal_player_position_without_clamping() {
        let reachable = ROOM_SIZE * 0.5 - Vec2::splat(PLAYER_RADIUS);
        let wall = ROOM_SIZE * 0.5;
        for step in 0..360 {
            let yaw = (step as f32).to_radians();
            for player in [
                Vec2::ZERO,
                reachable,
                -reachable,
                Vec2::new(reachable.x, -reachable.y),
                Vec2::new(-reachable.x, reachable.y),
                wall,
                -wall,
                Vec2::new(wall.x, -wall.y),
                Vec2::new(-wall.x, wall.y),
                Vec2::new(0.0, wall.y),
                Vec2::new(wall.x, 0.0),
            ] {
                assert_eq!(
                    clamp_follow_target(player, RENDER_COVERAGE_SIZE, yaw),
                    player,
                    "yaw {} must follow {player:?} exactly, not clamp it",
                    yaw.to_degrees()
                );
            }
        }
    }

    #[test]
    fn camera_clamp_is_the_identity_inside_the_legal_rectangle() {
        for heading in CameraHeading::ALL {
            let yaw = heading.yaw_radians();
            let (min, max) =
                camera_target_bounds(RENDER_COVERAGE_SIZE, yaw).expect("legal rectangle");
            for player in [
                Vec2::ZERO,
                min,
                max,
                Vec2::new(min.x, max.y),
                Vec2::new(max.x, min.y),
                max * 0.5,
            ] {
                assert_eq!(
                    clamp_follow_target(player, RENDER_COVERAGE_SIZE, yaw),
                    player,
                    "{heading:?} must follow {player:?} exactly"
                );
            }
        }
    }

    #[test]
    fn camera_clamp_keeps_the_ground_quadrilateral_inside_the_rendered_coverage_everywhere() {
        let half = RENDER_COVERAGE_SIZE * 0.5;
        let reachable = ROOM_SIZE * 0.5 - Vec2::splat(PLAYER_RADIUS);
        for step in 0..72 {
            let yaw = (step as f32 * 5.0).to_radians();
            for player in [
                Vec2::ZERO,
                reachable,
                -reachable,
                Vec2::new(reachable.x, -reachable.y),
                Vec2::new(-reachable.x, reachable.y),
                Vec2::new(0.0, reachable.y),
                Vec2::new(reachable.x, 0.0),
                Vec2::new(6.0, -11.0),
            ] {
                let target = clamp_follow_target(player, RENDER_COVERAGE_SIZE, yaw);
                for corner in ground_quadrilateral(yaw, target) {
                    assert!(
                        corner.x.abs() <= half.x + 1.0e-3 && corner.y.abs() <= half.y + 1.0e-3,
                        "yaw {} following {player:?} put a ground corner at {corner:?}, outside the {half:?} apron",
                        yaw.to_degrees()
                    );
                }
            }
        }
    }

    #[test]
    fn camera_clamp_without_a_legal_rectangle_holds_the_coverage_centre() {
        let yaw = CameraHeading::NorthEast.yaw_radians();
        assert_eq!(
            clamp_follow_target(Vec2::new(9.0, 9.0), Vec2::splat(30.0), yaw),
            Vec2::ZERO
        );
    }

    // -----------------------------------------------------------------------
    // Transform
    // -----------------------------------------------------------------------

    #[test]
    fn camera_offset_matches_the_reviewed_direction_at_the_initial_yaw() {
        let offset = camera_offset_direction(INITIAL_CAMERA_YAW_DEGREES.to_radians());

        assert!(
            (offset - CAMERA_OFFSET_DIRECTION.normalize())
                .abs()
                .max_element()
                < 1.0e-6,
            "initial offset {offset:?} should match the reviewed {:?}",
            CAMERA_OFFSET_DIRECTION.normalize()
        );
        assert!((offset.length() - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn camera_transform_keeps_the_fixed_elevation_zoom_and_zero_roll_at_every_yaw() {
        for step in 0..72 {
            let degrees = step as f32 * 5.0;
            let yaw = degrees.to_radians();
            let target = Vec2::new(3.0, -4.0);
            let transform = camera_transform(yaw, target);
            let focus = Vec3::new(target.x, 0.0, target.y);

            assert!(
                (transform.translation.distance(focus) - CAMERA_DISTANCE).abs() < 1.0e-3,
                "yaw {degrees} moved the camera off its fixed distance"
            );
            let forward = *transform.forward();
            let elevation = (-forward.y).asin().to_degrees();
            assert!(
                (elevation - CAMERA_ELEVATION_DEGREES).abs() < 1.0e-3,
                "yaw {degrees} elevation should be {CAMERA_ELEVATION_DEGREES}, got {elevation}"
            );
            assert!(
                transform.right().y.abs() < 1.0e-6,
                "yaw {degrees} introduced roll: right {:?}",
                transform.right()
            );

            let basis = ViewBasis::from_yaw_radians(yaw);
            assert!(
                (Vec2::new(forward.x, forward.z).normalize() - basis.forward())
                    .abs()
                    .max_element()
                    < 1.0e-4,
                "yaw {degrees} camera forward disagrees with the movement basis"
            );
            assert!(
                (transform.right().xz().normalize() - basis.right())
                    .abs()
                    .max_element()
                    < 1.0e-4,
                "yaw {degrees} camera right disagrees with the movement basis"
            );
        }
    }

    #[test]
    fn camera_clip_planes_hold_the_whole_room_at_every_heading() {
        let half = ROOM_SIZE * 0.5;
        for heading in CameraHeading::ALL {
            let yaw = heading.yaw_radians();
            let transform = camera_transform(yaw, Vec2::ZERO);
            for corner in [
                Vec3::new(half.x, 0.0, half.y),
                Vec3::new(half.x, 4.0, -half.y),
                Vec3::new(-half.x, 0.0, half.y),
                Vec3::new(-half.x, 4.0, -half.y),
            ] {
                let depth = (corner - transform.translation).dot(*transform.forward());
                assert!(
                    depth > CAMERA_NEAR && depth < CAMERA_FAR,
                    "{heading:?} puts {corner:?} at depth {depth}, outside the clip planes"
                );
            }
        }
    }

    #[test]
    fn camera_projection_is_the_reviewed_fixed_orthographic_rectangle() {
        let Projection::Orthographic(projection) = orthographic_projection() else {
            panic!("the reviewed camera must be orthographic");
        };
        assert!(
            matches!(
                projection.scaling_mode,
                ScalingMode::Fixed { width, height }
                    if width == ORTHOGRAPHIC_WIDTH && height == ORTHOGRAPHIC_HEIGHT
            ),
            "the reviewed camera must use a fixed 26 m by 14.625 m rectangle, got {:?}",
            projection.scaling_mode
        );
        assert_eq!(projection.scale, 1.0);
        assert_eq!(projection.near, CAMERA_NEAR);
        assert_eq!(projection.far, CAMERA_FAR);
        assert_eq!(projection.viewport_origin, Vec2::new(0.5, 0.5));
    }
}
