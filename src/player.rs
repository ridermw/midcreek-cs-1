//! The rigged technician: real keyboard movement, sliding collision, explicit
//! rig discovery, and the Idle/Walk animation state.
//!
//! ```text
//! HallState::Ready + PlayerSpawnPoint
//!        |
//!        v
//! spawn technician WorldAssetRoot -----------------+
//!        |                                          |
//!        v                                          |
//! discover named rig nodes                          |
//!   |            |               |                  |
//! missing     duplicate       complete               |
//!   |            |               |                  |
//!   +------------+       PlayerParts + PlayerAnimations
//!   |                            |                  |
//!   v                            v                  |
//! PlayerRigState::Failed   PlayerRigState::Ready     |
//!   (movement stops)              |                  |
//!        ^                        v                  |
//!        |                verify parts every frame --+
//!        +---- stale handle ------+
//!                                 |
//!                                 +-- root or nodes unavailable -->
//!                                     PlayerRigState::Pending, typed
//!                                     rebinding condition, movement stops,
//!                                     rescan recovers a complete instance
//!
//! ButtonInput<KeyCode> arrows
//!        |
//!        v
//! normalized screen request -> ViewBasis -> world direction
//!        |
//!        v
//! frame delta clamped to PLAYER_MAX_MOVE_DELTA (no destination-only tunnel)
//!        |
//!        v
//! resolve X, then Z, against HallColliders + radius-aware room bounds
//!        |
//!        +--> accepted displacement --> facing --> Idle/Walk clip
//! ```
//!
//! Movement never reads the camera entity. It reads [`ViewBasis`], the one
//! public screen-to-world interface, so the orbit task can retarget the basis
//! without touching a single movement rule.

use bevy::{prelude::*, world_serialization::WorldInstance};

use crate::{
    CellShiftSet,
    assetgen::{TECHNICIAN_BONES, TECHNICIAN_CLIPS},
    assets::GeneratedAssets,
    design::{INITIAL_CAMERA_YAW_DEGREES, PLAYER_RADIUS, PropId, ROOM_SIZE},
    operations::MovementLock,
    world::{HallColliders, HallState, PlayerSpawnPoint},
};

/// Ground speed of the technician, in metres per second.
pub const PLAYER_SPEED: f32 = 3.0;

/// Accepted displacement below this length is treated as standing still.
pub const PLAYER_MOVE_EPSILON: f32 = 1.0e-5;

/// Longest frame delta movement will ever integrate, in seconds.
///
/// Collision is resolved at the destination only, so it cannot see an obstacle
/// a single step jumped over. The integration step is therefore the whole
/// anti-tunneling invariant, and it is owned here rather than borrowed from
/// Bevy's virtual clock: this value matches
/// [`Time<Virtual>`](bevy::prelude::Virtual)'s default maximum delta and holds
/// even if that default is raised or movement is ever driven from another
/// clock. `player_maximum_step_never_spans_a_radius_inflated_obstacle` proves
/// `PLAYER_SPEED * PLAYER_MAX_MOVE_DELTA` is strictly shorter than every
/// radius-inflated authored obstacle on both world axes.
pub const PLAYER_MAX_MOVE_DELTA: f32 = 0.25;

/// Clamps a raw frame delta to [`PLAYER_MAX_MOVE_DELTA`], so a hitch, a
/// breakpoint, or a non-finite delta shortens the step instead of teleporting
/// the technician through the narrowest hose drop.
pub fn movement_delta_secs(raw_secs: f32) -> f32 {
    if raw_secs.is_nan() {
        // A poisoned clock must stall the technician, not teleport it.
        return 0.0;
    }
    raw_secs.clamp(0.0, PLAYER_MAX_MOVE_DELTA)
}

/// Asset file stem holding the technician module.
pub const TECHNICIAN_ASSET: &str = "technician";

/// Declared module (and glTF scene) name of the technician.
pub const TECHNICIAN_MODULE: &str = "technician";

/// Name of the technician's skinned mesh node inside the generated scene.
pub const TECHNICIAN_SKIN_NODE: &str = "technician-skin";

/// The authored model faces its own local `+Z`, so facing rotations map `+Z`
/// onto the accepted displacement rather than Bevy's `-Z` camera forward.
pub const TECHNICIAN_MODEL_FORWARD: Vec3 = Vec3::Z;

/// Every rig node the technician must expose, in discovery order: the skinned
/// mesh, then the eleven skin joints.
///
/// The generated module root shares its name with the glTF scene, so it is
/// deliberately not a rig part; the spawned technician entity is the stable
/// handle for the whole instance.
pub fn required_player_parts() -> Vec<&'static str> {
    let mut required = Vec::with_capacity(TECHNICIAN_BONES.len() + 1);
    required.push(TECHNICIAN_SKIN_NODE);
    required.extend(TECHNICIAN_BONES);
    required
}

// ---------------------------------------------------------------------------
// View basis
// ---------------------------------------------------------------------------

/// The stable screen-to-world interface movement reads.
///
/// It is initialised to the reviewed NorthEast diamond view and is the only
/// thing movement knows about the camera. The orbit task becomes its runtime
/// updater; every movement rule below stays unchanged.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct ViewBasis {
    yaw_radians: f32,
}

impl Default for ViewBasis {
    fn default() -> Self {
        Self::from_yaw_degrees(INITIAL_CAMERA_YAW_DEGREES)
    }
}

impl ViewBasis {
    /// Basis for a camera yaw, in degrees.
    pub fn from_yaw_degrees(degrees: f32) -> Self {
        Self::from_yaw_radians(degrees.to_radians())
    }

    /// Basis for a camera yaw, in radians.
    pub fn from_yaw_radians(radians: f32) -> Self {
        Self {
            yaw_radians: radians,
        }
    }

    /// Current camera yaw, in radians.
    pub fn yaw_radians(&self) -> f32 {
        self.yaw_radians
    }

    /// Current camera yaw, in degrees.
    pub fn yaw_degrees(&self) -> f32 {
        self.yaw_radians.to_degrees()
    }

    /// Retargets the basis, in radians.
    pub fn set_yaw_radians(&mut self, radians: f32) {
        self.yaw_radians = radians;
    }

    /// Retargets the basis, in degrees.
    pub fn set_yaw_degrees(&mut self, degrees: f32) {
        self.set_yaw_radians(degrees.to_radians());
    }

    /// Ground-plane direction from the followed point towards the camera.
    pub fn camera_offset(&self) -> Vec2 {
        Vec2::new(self.yaw_radians.sin(), self.yaw_radians.cos())
    }

    /// Ground-plane direction of screen up, which is the direction the camera
    /// looks along.
    pub fn forward(&self) -> Vec2 {
        -self.camera_offset()
    }

    /// Ground-plane direction of screen right.
    pub fn right(&self) -> Vec2 {
        Vec2::new(-self.forward().y, self.forward().x)
    }

    /// Maps a screen-space request onto the ground plane.
    pub fn world_direction(&self, screen: Vec2) -> Vec2 {
        self.right() * screen.x + self.forward() * screen.y
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Normalized screen-space request from the real arrow keys. Opposite keys
/// cancel, and diagonals are normalized so they never outrun a cardinal.
pub fn arrow_input(keys: &ButtonInput<KeyCode>) -> Vec2 {
    let mut request = Vec2::ZERO;
    if keys.pressed(KeyCode::ArrowRight) {
        request.x += 1.0;
    }
    if keys.pressed(KeyCode::ArrowLeft) {
        request.x -= 1.0;
    }
    if keys.pressed(KeyCode::ArrowUp) {
        request.y += 1.0;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        request.y -= 1.0;
    }
    request.normalize_or_zero()
}

// ---------------------------------------------------------------------------
// Movement resolution
// ---------------------------------------------------------------------------

/// The outcome of resolving one requested step against the hall.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MoveResolution {
    /// Ground position after the step.
    pub position: Vec2,
    /// Displacement the hall actually accepted.
    pub accepted: Vec2,
    /// Prop that rejected the world X component, if any.
    pub blocked_x: Option<PropId>,
    /// Prop that rejected the world Z component, if any.
    pub blocked_z: Option<PropId>,
    /// Whether the world X component hit the room boundary.
    pub clamped_x: bool,
    /// Whether the world Z component hit the room boundary.
    pub clamped_z: bool,
}

impl MoveResolution {
    /// Whether any collider or boundary reduced the requested step.
    pub fn was_restricted(&self) -> bool {
        self.blocked_x.is_some() || self.blocked_z.is_some() || self.clamped_x || self.clamped_z
    }
}

/// Resolves world X and world Z separately so the technician slides along a
/// rack face instead of sticking to it.
pub fn resolve_move(
    from: Vec2,
    delta: Vec2,
    colliders: &HallColliders,
    room: Vec2,
    radius: f32,
) -> MoveResolution {
    let limit = (room * 0.5 - Vec2::splat(radius)).max(Vec2::ZERO);
    let mut resolution = MoveResolution {
        position: from,
        ..default()
    };

    let requested_x = from.x + delta.x;
    let target_x = requested_x.clamp(-limit.x, limit.x);
    resolution.clamped_x = target_x != requested_x;
    if target_x != from.x {
        let candidate = Vec2::new(target_x, resolution.position.y);
        match colliders.first_overlap(candidate, radius) {
            Some(collider) => resolution.blocked_x = Some(collider.id.clone()),
            None => resolution.position.x = target_x,
        }
    }

    let requested_z = from.y + delta.y;
    let target_z = requested_z.clamp(-limit.y, limit.y);
    resolution.clamped_z = target_z != requested_z;
    if target_z != from.y {
        let candidate = Vec2::new(resolution.position.x, target_z);
        match colliders.first_overlap(candidate, radius) {
            Some(collider) => resolution.blocked_z = Some(collider.id.clone()),
            None => resolution.position.y = target_z,
        }
    }

    resolution.accepted = resolution.position - from;
    resolution
}

/// Rotation that points the authored model's forward along `direction`.
/// Returns `None` for a step the hall rejected entirely, so facing is never
/// derived from a requested direction the technician did not actually take.
pub fn facing_rotation(direction: Vec2) -> Option<Quat> {
    (direction.length_squared() > PLAYER_MOVE_EPSILON * PLAYER_MOVE_EPSILON)
        .then(|| Quat::from_rotation_y(direction.x.atan2(direction.y)))
}

// ---------------------------------------------------------------------------
// Rig discovery
// ---------------------------------------------------------------------------

/// Every way the technician rig can fail to satisfy its contract. Each variant
/// names the offending node so a failure is never silent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlayerRigError {
    /// A required named node is absent from the spawned technician.
    MissingPart {
        /// Required node name.
        name: String,
    },
    /// A required named node appears more than once, so no single handle is
    /// authoritative.
    DuplicatePart {
        /// Required node name.
        name: String,
        /// How many entities carried that name.
        found: usize,
    },
    /// A discovered handle no longer resolves to its named node.
    StalePart {
        /// Required node name.
        name: String,
    },
    /// The spawned technician instance is not resolvable this frame: either no
    /// materialised instance root exists, or more than one claims to be the
    /// technician. Every bound handle is untrustworthy until it resolves.
    TechnicianInstanceUnavailable {
        /// How many materialised technician instances were found.
        found: usize,
    },
    /// The technician instance exists but exposes no named nodes at all, which
    /// is what a mid-respawn instance looks like from the binder.
    TechnicianRigNodesUnavailable,
    /// The generated technician document is missing or unloaded.
    MissingTechnicianDocument,
    /// The spawned technician exposes no `AnimationPlayer`.
    MissingAnimationPlayer,
    /// The generated technician document lacks a declared clip.
    MissingAnimationClip {
        /// Declared clip name.
        name: String,
    },
}

/// One discovered rig node: its declared name, its entity, and the rest
/// transform the idle transition restores.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerPart {
    /// Declared node name.
    pub name: String,
    /// Entity carrying that name.
    pub entity: Entity,
    /// Transform captured before any clip played.
    pub rest: Transform,
}

/// Explicit handles for every required technician rig node.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct PlayerParts {
    parts: Vec<PlayerPart>,
}

impl PlayerParts {
    /// Joins the named nodes of a spawned technician onto the required rig
    /// contract. Missing and duplicate names are reported together.
    pub fn discover(
        named: impl IntoIterator<Item = (String, Entity, Transform)>,
    ) -> Result<Self, Vec<PlayerRigError>> {
        let discovered = named.into_iter().collect::<Vec<_>>();
        let mut parts = Vec::new();
        let mut errors = Vec::new();

        for name in required_player_parts() {
            let matches = discovered
                .iter()
                .filter(|(found, _, _)| found == name)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [] => errors.push(PlayerRigError::MissingPart {
                    name: name.to_owned(),
                }),
                [(_, entity, rest)] => parts.push(PlayerPart {
                    name: name.to_owned(),
                    entity: *entity,
                    rest: *rest,
                }),
                found => errors.push(PlayerRigError::DuplicatePart {
                    name: name.to_owned(),
                    found: found.len(),
                }),
            }
        }

        if errors.is_empty() {
            Ok(Self { parts })
        } else {
            Err(errors)
        }
    }

    /// Every discovered part, in required order.
    pub fn all(&self) -> &[PlayerPart] {
        &self.parts
    }

    /// One discovered part by declared name.
    pub fn get(&self, name: &str) -> Option<&PlayerPart> {
        self.parts.iter().find(|part| part.name == name)
    }

    /// The entity for one declared name.
    pub fn entity(&self, name: &str) -> Option<Entity> {
        self.get(name).map(|part| part.entity)
    }
}

/// Explicit lifecycle of the technician rig.
#[derive(States, Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PlayerRigState {
    /// The technician has not spawned, or its nodes are not discovered yet.
    #[default]
    Pending,
    /// Every required node and clip is bound.
    Ready,
    /// A required node or clip is missing, duplicated, or stale.
    Failed,
}

/// Every rig failure observed so far. Movement stops while it is non-empty.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct PlayerRigReport {
    errors: Vec<PlayerRigError>,
}

impl PlayerRigReport {
    /// Every recorded failure, in discovery order.
    pub fn errors(&self) -> &[PlayerRigError] {
        &self.errors
    }

    /// Whether the rig currently satisfies its contract.
    pub fn is_healthy(&self) -> bool {
        self.errors.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Animation
// ---------------------------------------------------------------------------

/// The generated technician clips, in declaration order.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PlayerClip {
    /// Standing still.
    #[default]
    Idle,
    /// Moving under accepted displacement.
    Walk,
    /// Repairing a rack, which is exactly while movement is locked.
    Repair,
}

impl PlayerClip {
    /// Every clip, in generated declaration order.
    pub const ALL: [Self; 3] = [Self::Idle, Self::Walk, Self::Repair];

    /// Declared clip name inside the generated technician document.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Idle => TECHNICIAN_CLIPS[0],
            Self::Walk => TECHNICIAN_CLIPS[1],
            Self::Repair => TECHNICIAN_CLIPS[2],
        }
    }
}

/// The technician animation graph and the node index of each declared clip.
#[derive(Resource, Clone, Debug)]
pub struct PlayerAnimations {
    /// Entity carrying the glTF-provided `AnimationPlayer`.
    pub player: Entity,
    /// The one graph built for the technician.
    pub graph: Handle<AnimationGraph>,
    idle: AnimationNodeIndex,
    walk: AnimationNodeIndex,
    repair: AnimationNodeIndex,
}

impl PlayerAnimations {
    /// Graph node index of one declared clip.
    pub fn node(&self, clip: PlayerClip) -> AnimationNodeIndex {
        match clip {
            PlayerClip::Idle => self.idle,
            PlayerClip::Walk => self.walk,
            PlayerClip::Repair => self.repair,
        }
    }
}

/// The clip the technician is currently playing.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerAnimationState {
    current: PlayerClip,
}

impl PlayerAnimationState {
    /// The clip currently playing.
    pub fn current(&self) -> PlayerClip {
        self.current
    }
}

/// The last resolved step, published for verification and gameplay systems.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct PlayerMotion {
    /// Normalized screen-space arrow request.
    pub requested_screen: Vec2,
    /// The same request mapped through [`ViewBasis`].
    pub requested_world: Vec2,
    /// How the hall resolved the requested step.
    pub resolution: MoveResolution,
}

impl PlayerMotion {
    /// Displacement the hall accepted last frame.
    pub fn accepted(&self) -> Vec2 {
        self.resolution.accepted
    }

    /// Whether the accepted displacement counts as walking.
    pub fn is_walking(&self) -> bool {
        self.accepted().length() > PLAYER_MOVE_EPSILON
    }
}

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

/// The spawned technician root that movement transforms.
#[derive(Component, Clone, Copy, Debug)]
pub struct Technician;

/// A spawned technician whose glTF instance has materialised.
type SpawnedTechnician = (With<Technician>, With<WorldInstance>, With<Children>);

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Spawns the rigged technician and drives real keyboard movement.
pub struct TechnicianPlugin;

impl Plugin for TechnicianPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<PlayerRigState>()
            .init_resource::<ViewBasis>()
            .init_resource::<PlayerRigReport>()
            .init_resource::<PlayerMotion>()
            .init_resource::<PlayerAnimationState>()
            .add_systems(
                Update,
                (spawn_technician, bind_technician_rig)
                    .chain()
                    .in_set(CellShiftSet::SpawnWorld)
                    .run_if(in_state(HallState::Ready)),
            )
            .add_systems(
                Update,
                move_player
                    .in_set(CellShiftSet::MovePlayer)
                    .run_if(player_rig_is_ready),
            )
            .add_systems(
                Update,
                update_player_animation
                    .in_set(CellShiftSet::UpdateAnimation)
                    .run_if(player_rig_is_ready),
            );
    }
}

/// Run condition gating every technician system on a healthy, fully bound rig.
pub fn player_rig_is_ready(
    parts: Option<Res<PlayerParts>>,
    animations: Option<Res<PlayerAnimations>>,
    report: Res<PlayerRigReport>,
) -> bool {
    parts.is_some() && animations.is_some() && report.is_healthy()
}

fn spawn_technician(
    mut commands: Commands,
    spawn: Option<Res<PlayerSpawnPoint>>,
    generated: Res<GeneratedAssets>,
    existing: Query<(), With<Technician>>,
    mut report: ResMut<PlayerRigReport>,
    mut next: ResMut<NextState<PlayerRigState>>,
) {
    if !existing.is_empty() {
        return;
    }
    let Some(spawn) = spawn else {
        return;
    };
    let Some(scene) = generated.scene(TECHNICIAN_ASSET, TECHNICIAN_MODULE) else {
        error!("the generated technician module scene is not tracked");
        report.errors = vec![PlayerRigError::MissingTechnicianDocument];
        next.set(PlayerRigState::Failed);
        return;
    };

    commands.spawn((
        Technician,
        Name::new("technician-root"),
        WorldAssetRoot(scene.clone()),
        Transform::from_translation(Vec3::new(spawn.0.x, 0.0, spawn.0.y)),
    ));
}

/// Publishes a transient rebinding condition, so [`player_rig_is_ready`] is
/// false in the same frame the instance stops resolving.
///
/// The rig stays `Pending` rather than `Failed`: a whole-instance respawn is a
/// legal engine behaviour, and an unhealthy report forces the next frame to
/// rescan, so a complete rig recovers on its own. An existing failure is never
/// overwritten — it already stops every consumer and names its cause precisely.
fn report_rig_unavailable(
    report: &mut PlayerRigReport,
    next: &mut NextState<PlayerRigState>,
    error: PlayerRigError,
) {
    if !report.is_healthy() {
        return;
    }
    warn!("technician rig is rebinding: {error:?}");
    report.errors = vec![error];
    next.set(PlayerRigState::Pending);
}

/// Binds, verifies, and rebinds the explicit rig handles.
///
/// Every frame the bound handles are checked against the names they claim. A
/// healthy rig costs twelve name lookups and stops there. Otherwise the system
/// rescans the spawned instance: Bevy despawns and respawns a world instance
/// whenever a glTF sub-asset event arrives, so a complete rescan is a legal
/// rebind, while an incomplete one fails loudly.
///
/// No path returns while the report still claims health. An unresolvable
/// instance root and an instance with no named nodes each publish their own
/// typed condition first, because the bound handles are dead in exactly those
/// frames and movement would otherwise run against them.
#[allow(clippy::too_many_arguments)]
fn bind_technician_rig(
    mut commands: Commands,
    technicians: Query<Entity, SpawnedTechnician>,
    children: Query<&Children>,
    named: Query<(&Name, &Transform)>,
    players: Query<Entity, With<AnimationPlayer>>,
    parts: Option<Res<PlayerParts>>,
    generated: Res<GeneratedAssets>,
    documents: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut report: ResMut<PlayerRigReport>,
    mut next: ResMut<NextState<PlayerRigState>>,
) {
    let Ok(root) = technicians.single() else {
        report_rig_unavailable(
            &mut report,
            &mut next,
            PlayerRigError::TechnicianInstanceUnavailable {
                found: technicians.iter().count(),
            },
        );
        return;
    };

    let bound_before = parts.as_deref().map_or(0, |parts| parts.all().len());
    let stale = parts
        .as_deref()
        .map(|parts| {
            parts
                .all()
                .iter()
                .filter(|part| {
                    named.get(part.entity).map(|(name, _)| name.as_str()) != Ok(part.name.as_str())
                })
                .map(|part| PlayerRigError::StalePart {
                    name: part.name.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if parts.is_some() && stale.is_empty() && report.is_healthy() {
        return;
    }

    let mut discovered = Vec::new();
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        if let Ok((name, transform)) = named.get(entity)
            && entity != root
        {
            discovered.push((name.as_str().to_owned(), entity, *transform));
        }
        if let Ok(kids) = children.get(entity) {
            stack.extend(kids.iter());
        }
    }

    if discovered.is_empty() {
        // The instance is mid-respawn. The bound handles stay for the next
        // rescan, but nothing may consume them until the whole rig is back.
        report_rig_unavailable(
            &mut report,
            &mut next,
            PlayerRigError::TechnicianRigNodesUnavailable,
        );
        return;
    }

    let mut errors = Vec::new();
    let parts = match PlayerParts::discover(discovered) {
        Ok(parts) => Some(parts),
        Err(found) => {
            errors.extend(found);
            None
        }
    };

    let animation_player = parts.as_ref().and_then(|_| {
        let mut stack = vec![root];
        while let Some(entity) = stack.pop() {
            if players.contains(entity) {
                return Some(entity);
            }
            if let Ok(kids) = children.get(entity) {
                stack.extend(kids.iter());
            }
        }
        None
    });
    if parts.is_some() && animation_player.is_none() {
        errors.push(PlayerRigError::MissingAnimationPlayer);
    }

    let document = generated
        .document(TECHNICIAN_ASSET)
        .and_then(|handle| documents.get(handle));
    let clips = match document {
        None => {
            errors.push(PlayerRigError::MissingTechnicianDocument);
            None
        }
        Some(document) => {
            let mut clips = Vec::new();
            for clip in PlayerClip::ALL {
                match document.named_animations.get(clip.name()) {
                    Some(handle) => clips.push(handle.clone()),
                    None => errors.push(PlayerRigError::MissingAnimationClip {
                        name: clip.name().to_owned(),
                    }),
                }
            }
            (clips.len() == PlayerClip::ALL.len()).then_some(clips)
        }
    };

    if !errors.is_empty() {
        // A previously bound handle that no longer resolves names a lost node
        // more precisely than a rescan does, but only while the rest of the rig
        // survives it. When every bound handle died the instance itself was
        // replaced, so the rescan's own findings are the specific truth and a
        // wall of StalePart would bury them.
        let errors = if stale.is_empty() || stale.len() == bound_before {
            errors
        } else {
            stale
        };
        for error in &errors {
            error!("technician rig failure: {error:?}");
        }
        report.errors = errors;
        next.set(PlayerRigState::Failed);
        return;
    }

    let parts = parts.expect("a rig with no errors discovered every required part");
    let player = animation_player.expect("a rig with no errors exposes an AnimationPlayer");
    let clips = clips.expect("a rig with no errors resolved every declared clip");
    let (graph, nodes) = AnimationGraph::from_clips(clips);
    let graph = graphs.add(graph);

    let animations = PlayerAnimations {
        player,
        graph: graph.clone(),
        idle: nodes[0],
        walk: nodes[1],
        repair: nodes[2],
    };
    commands
        .entity(player)
        .insert(AnimationGraphHandle(graph.clone()));
    commands.insert_resource(parts);
    commands.insert_resource(animations);
    commands.insert_resource(PlayerAnimationState {
        current: PlayerClip::Idle,
    });
    report.errors.clear();
    next.set(PlayerRigState::Ready);
}

fn move_player(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    basis: Res<ViewBasis>,
    colliders: Res<HallColliders>,
    lock: Option<Res<MovementLock>>,
    mut motion: ResMut<PlayerMotion>,
    mut technicians: Query<&mut Transform, With<Technician>>,
) {
    let Ok(mut transform) = technicians.single_mut() else {
        return;
    };

    // A repair holds the technician still. The arrow keys are dropped rather
    // than merely blocked, so the published motion really is standing still and
    // the animation state cannot keep a stale walking pose.
    if lock.is_some_and(|lock| lock.is_locked()) {
        *motion = PlayerMotion::default();
        return;
    }

    let requested_screen = arrow_input(&keys);
    let requested_world = basis.world_direction(requested_screen);
    let from = Vec2::new(transform.translation.x, transform.translation.z);
    let resolution = resolve_move(
        from,
        requested_world * PLAYER_SPEED * movement_delta_secs(time.delta_secs()),
        &colliders,
        ROOM_SIZE,
        PLAYER_RADIUS,
    );

    transform.translation.x = resolution.position.x;
    transform.translation.z = resolution.position.y;
    if let Some(rotation) = facing_rotation(resolution.accepted) {
        transform.rotation = rotation;
    }

    *motion = PlayerMotion {
        requested_screen,
        requested_world,
        resolution,
    };
}

/// Drives the generated Idle, Walk, and Repair clips.
///
/// A locked repair outranks displacement, because movement is already zero
/// while it holds. Otherwise accepted displacement chooses Walk or Idle. Every
/// transition stops the clips it is leaving and explicitly restores every
/// discovered rest transform first, so no stale mid-stride or mid-repair pose
/// survives a change of clip.
pub fn update_player_animation(
    motion: Res<PlayerMotion>,
    lock: Option<Res<MovementLock>>,
    parts: Res<PlayerParts>,
    animations: Res<PlayerAnimations>,
    mut state: ResMut<PlayerAnimationState>,
    mut players: Query<&mut AnimationPlayer>,
    mut transforms: Query<&mut Transform>,
) {
    let Ok(mut player) = players.get_mut(animations.player) else {
        return;
    };
    let desired = if lock.is_some_and(|lock| lock.is_locked()) {
        PlayerClip::Repair
    } else if motion.is_walking() {
        PlayerClip::Walk
    } else {
        PlayerClip::Idle
    };

    if state.current == desired && player.is_playing_animation(animations.node(desired)) {
        return;
    }

    for clip in PlayerClip::ALL {
        if clip != desired {
            player.stop(animations.node(clip));
        }
    }
    if desired != PlayerClip::Walk {
        for part in parts.all() {
            if let Ok(mut transform) = transforms.get_mut(part.entity) {
                *transform = part.rest;
            }
        }
    }
    player.play(animations.node(desired)).repeat();
    state.current = desired;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::{AISLE_CENTER_X, CAMERA_OFFSET_DIRECTION, RACK_ROW_X};

    const HEADINGS: [f32; 4] = [45.0, 135.0, 225.0, 315.0];

    fn colliders() -> HallColliders {
        HallColliders::from(crate::design::SceneBlueprint::v0().colliders)
    }

    fn keys(pressed: &[KeyCode]) -> ButtonInput<KeyCode> {
        let mut input = ButtonInput::default();
        for key in pressed {
            input.press(*key);
        }
        input
    }

    fn close(actual: Vec2, expected: Vec2) -> bool {
        actual.distance(expected) < 1.0e-5
    }

    #[test]
    fn player_view_basis_defaults_to_the_north_east_camera_basis() {
        let basis = ViewBasis::default();

        assert_eq!(basis.yaw_degrees(), INITIAL_CAMERA_YAW_DEGREES);
        assert!(close(
            basis.camera_offset(),
            CAMERA_OFFSET_DIRECTION.xz().normalize()
        ));
        assert!(close(basis.forward(), Vec2::new(-0.70710677, -0.70710677)));
        assert!(close(basis.right(), Vec2::new(0.70710677, -0.70710677)));
    }

    #[test]
    fn player_view_basis_stays_orthonormal_at_every_quarter_turn() {
        for degrees in HEADINGS {
            let basis = ViewBasis::from_yaw_degrees(degrees);
            assert!((basis.forward().length() - 1.0).abs() < 1.0e-5, "{degrees}");
            assert!((basis.right().length() - 1.0).abs() < 1.0e-5, "{degrees}");
            assert!(
                basis.forward().dot(basis.right()).abs() < 1.0e-5,
                "{degrees}"
            );
            assert!(close(basis.forward(), -basis.camera_offset()), "{degrees}");
        }

        let mut basis = ViewBasis::default();
        basis.set_yaw_degrees(135.0);
        assert_eq!(basis, ViewBasis::from_yaw_degrees(135.0));
        assert!(close(
            basis.camera_offset(),
            Vec2::new(0.70710677, -0.70710677)
        ));
    }

    #[test]
    fn player_arrow_input_covers_the_whole_real_key_matrix() {
        let diagonal = 0.70710677;
        let cases: [(&[KeyCode], Vec2); 10] = [
            (&[], Vec2::ZERO),
            (&[KeyCode::ArrowLeft, KeyCode::ArrowRight], Vec2::ZERO),
            (&[KeyCode::ArrowUp, KeyCode::ArrowDown], Vec2::ZERO),
            (
                &[
                    KeyCode::ArrowUp,
                    KeyCode::ArrowDown,
                    KeyCode::ArrowLeft,
                    KeyCode::ArrowRight,
                ],
                Vec2::ZERO,
            ),
            (&[KeyCode::ArrowUp], Vec2::new(0.0, 1.0)),
            (&[KeyCode::ArrowDown], Vec2::new(0.0, -1.0)),
            (&[KeyCode::ArrowLeft], Vec2::new(-1.0, 0.0)),
            (&[KeyCode::ArrowRight], Vec2::new(1.0, 0.0)),
            (
                &[KeyCode::ArrowUp, KeyCode::ArrowRight],
                Vec2::new(diagonal, diagonal),
            ),
            (
                &[KeyCode::ArrowDown, KeyCode::ArrowLeft],
                Vec2::new(-diagonal, -diagonal),
            ),
        ];

        for (pressed, expected) in cases {
            let actual = arrow_input(&keys(pressed));
            assert!(close(actual, expected), "{pressed:?} -> {actual:?}");
            assert!(actual.length() <= 1.0 + 1.0e-5, "{pressed:?}");
        }
    }

    #[test]
    fn player_world_direction_maps_every_arrow_combination_through_the_basis() {
        let basis = ViewBasis::default();
        let cases: [(&[KeyCode], Vec2); 4] = [
            (
                &[KeyCode::ArrowUp, KeyCode::ArrowRight],
                Vec2::new(0.0, -1.0),
            ),
            (
                &[KeyCode::ArrowDown, KeyCode::ArrowLeft],
                Vec2::new(0.0, 1.0),
            ),
            (
                &[KeyCode::ArrowUp, KeyCode::ArrowLeft],
                Vec2::new(-1.0, 0.0),
            ),
            (
                &[KeyCode::ArrowDown, KeyCode::ArrowRight],
                Vec2::new(1.0, 0.0),
            ),
        ];

        for (pressed, expected) in cases {
            let actual = basis.world_direction(arrow_input(&keys(pressed)));
            assert!(close(actual, expected), "{pressed:?} -> {actual:?}");
        }

        // The same screen request rotates with the basis and never changes length.
        for degrees in HEADINGS {
            let rotated = ViewBasis::from_yaw_degrees(degrees);
            let direction = rotated.world_direction(Vec2::new(0.0, 1.0));
            assert!((direction.length() - 1.0).abs() < 1.0e-5, "{degrees}");
            assert!(close(direction, rotated.forward()), "{degrees}");
        }
    }

    #[test]
    fn player_move_resolution_slides_along_a_blocked_axis() {
        let colliders = colliders();
        let from = Vec2::new(AISLE_CENTER_X[0], 0.0);
        let into_rack = Vec2::new(-4.0, 0.5);

        let resolution = resolve_move(from, into_rack, &colliders, ROOM_SIZE, PLAYER_RADIUS);

        assert_eq!(
            resolution.blocked_x.as_ref().map(PropId::as_str),
            Some("rack-row-01")
        );
        assert_eq!(resolution.blocked_z, None);
        assert_eq!(resolution.position, Vec2::new(from.x, 0.5));
        assert_eq!(resolution.accepted, Vec2::new(0.0, 0.5));
        assert!(resolution.was_restricted());
    }

    #[test]
    fn player_move_resolution_accepts_a_free_step_without_restriction() {
        let colliders = colliders();
        let from = Vec2::new(AISLE_CENTER_X[1], -11.0);
        let step = Vec2::new(0.1, -0.2);

        let resolution = resolve_move(from, step, &colliders, ROOM_SIZE, PLAYER_RADIUS);

        assert!(close(resolution.position, from + step));
        assert!(close(resolution.accepted, step));
        assert!(!resolution.was_restricted());
    }

    #[test]
    fn player_move_resolution_clamps_to_radius_aware_room_bounds() {
        let colliders = colliders();
        let limit = ROOM_SIZE * 0.5 - Vec2::splat(PLAYER_RADIUS);
        let from = Vec2::new(AISLE_CENTER_X[1], limit.y - 0.05);

        let resolution = resolve_move(
            from,
            Vec2::new(0.0, 5.0),
            &colliders,
            ROOM_SIZE,
            PLAYER_RADIUS,
        );

        assert!(resolution.clamped_z);
        assert_eq!(resolution.position.y, limit.y);
        assert_eq!(resolution.position.y, ROOM_SIZE.y * 0.5 - PLAYER_RADIUS);
        assert!(!colliders.overlaps(resolution.position, PLAYER_RADIUS));
    }

    #[test]
    fn player_move_resolution_stops_dead_against_a_rack_face() {
        let colliders = colliders();
        let rack_face = RACK_ROW_X[0] + 0.8 + PLAYER_RADIUS;
        let from = Vec2::new(rack_face + 0.01, 0.0);

        let resolution = resolve_move(
            from,
            Vec2::new(-0.05, 0.0),
            &colliders,
            ROOM_SIZE,
            PLAYER_RADIUS,
        );

        assert_eq!(
            resolution.blocked_x.as_ref().map(PropId::as_str),
            Some("rack-row-01")
        );
        assert_eq!(resolution.blocked_z, None);
        assert_eq!(resolution.accepted, Vec2::ZERO);
        assert_eq!(resolution.position, from);
    }

    #[test]
    fn player_move_resolution_slides_around_a_rack_corner_instead_of_sticking() {
        let colliders = colliders();
        // Diagonally outside the north-east corner of the first rack row. A
        // convex box can only ever reject one axis from outside, which is
        // exactly what makes separate-axis resolution slide.
        let corner = Vec2::new(RACK_ROW_X[0] + 0.8 + PLAYER_RADIUS, 8.05 + PLAYER_RADIUS);
        let from = corner + Vec2::splat(0.05);

        let resolution = resolve_move(
            from,
            Vec2::new(-0.1, -0.1),
            &colliders,
            ROOM_SIZE,
            PLAYER_RADIUS,
        );

        assert_eq!(resolution.blocked_x, None);
        assert_eq!(
            resolution.blocked_z.as_ref().map(PropId::as_str),
            Some("rack-row-01")
        );
        assert!(close(resolution.accepted, Vec2::new(-0.1, 0.0)));
        assert!(!colliders.overlaps(resolution.position, PLAYER_RADIUS));
    }

    #[test]
    fn player_facing_rotation_points_the_model_forward_along_accepted_displacement() {
        for direction in [
            Vec2::new(0.0, -1.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(-1.0, 0.0),
            Vec2::new(0.6, 0.8),
        ] {
            let rotation = facing_rotation(direction).expect("a real step must set facing");
            let forward = (rotation * TECHNICIAN_MODEL_FORWARD).xz();
            assert!(close(forward, direction.normalize()), "{direction:?}");
        }

        assert_eq!(facing_rotation(Vec2::ZERO), None);
        assert_eq!(facing_rotation(Vec2::new(0.0, 1.0e-9)), None);
    }

    #[test]
    fn player_required_parts_cover_the_generated_technician_rig() {
        let required = required_player_parts();

        assert_eq!(required.len(), TECHNICIAN_BONES.len() + 1);
        assert_eq!(required[0], TECHNICIAN_SKIN_NODE);
        assert_eq!(&required[1..], &TECHNICIAN_BONES);

        let mut unique = required.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), required.len());
    }

    #[test]
    fn player_parts_discovery_binds_every_required_node_and_its_rest_transform() {
        let mut world = World::new();
        let entries = required_player_parts()
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                (
                    name.to_owned(),
                    world.spawn_empty().id(),
                    Transform::from_xyz(index as f32, 0.0, 0.0),
                )
            })
            .collect::<Vec<_>>();

        let parts = PlayerParts::discover(entries.clone()).expect("a complete rig must bind");

        assert_eq!(parts.all().len(), required_player_parts().len());
        for (index, (name, entity, rest)) in entries.into_iter().enumerate() {
            let part = parts.get(&name).expect("every required part is bound");
            assert_eq!(part.entity, entity);
            assert_eq!(parts.entity(&name), Some(entity));
            assert_eq!(part.rest, rest);
            assert_eq!(parts.all()[index].name, name);
        }
        assert_eq!(parts.get("bone-does-not-exist"), None);
    }

    #[test]
    fn player_parts_discovery_reports_missing_and_duplicate_rig_nodes() {
        let mut world = World::new();
        let mut entries = required_player_parts()
            .into_iter()
            .map(|name| {
                (
                    name.to_owned(),
                    world.spawn_empty().id(),
                    Transform::IDENTITY,
                )
            })
            .collect::<Vec<_>>();
        entries.retain(|(name, _, _)| name != "bone-tool");
        entries.push((
            "bone-head".to_owned(),
            world.spawn_empty().id(),
            Transform::IDENTITY,
        ));

        let errors = PlayerParts::discover(entries).expect_err("an incomplete rig must fail");

        assert_eq!(
            errors,
            [
                PlayerRigError::DuplicatePart {
                    name: "bone-head".to_owned(),
                    found: 2,
                },
                PlayerRigError::MissingPart {
                    name: "bone-tool".to_owned(),
                },
            ]
        );
        assert!(PlayerParts::discover([]).is_err());
    }

    #[test]
    fn player_clips_match_the_generated_animation_names() {
        assert_eq!(PlayerClip::ALL.len(), TECHNICIAN_CLIPS.len());
        for (index, clip) in PlayerClip::ALL.into_iter().enumerate() {
            assert_eq!(clip.name(), TECHNICIAN_CLIPS[index]);
        }
        assert_eq!(PlayerClip::default(), PlayerClip::Idle);
        assert_eq!(PlayerAnimationState::default().current(), PlayerClip::Idle);
    }

    #[test]
    fn player_motion_reports_walking_only_for_accepted_displacement() {
        let mut motion = PlayerMotion {
            requested_screen: Vec2::new(0.0, 1.0),
            requested_world: Vec2::new(-0.70710677, -0.70710677),
            resolution: MoveResolution {
                blocked_x: Some(PropId::new("rack-row-01")),
                blocked_z: Some(PropId::new("rack-row-01")),
                ..default()
            },
        };
        assert!(!motion.is_walking());
        assert_eq!(motion.accepted(), Vec2::ZERO);

        motion.resolution.accepted = Vec2::new(0.0, 0.05);
        assert!(motion.is_walking());

        assert!(
            !PlayerRigReport {
                errors: vec![PlayerRigError::StalePart {
                    name: "bone-hips".to_owned()
                }]
            }
            .is_healthy()
        );
        assert!(PlayerRigReport::default().is_healthy());
    }

    #[test]
    fn player_movement_delta_clamps_a_hitch_to_the_anti_tunneling_maximum() {
        assert_eq!(movement_delta_secs(1.0 / 60.0), 1.0 / 60.0);
        assert_eq!(movement_delta_secs(0.0), 0.0);
        assert_eq!(
            movement_delta_secs(PLAYER_MAX_MOVE_DELTA),
            PLAYER_MAX_MOVE_DELTA
        );
        assert_eq!(
            movement_delta_secs(PLAYER_MAX_MOVE_DELTA + 1.0e-3),
            PLAYER_MAX_MOVE_DELTA
        );
        assert_eq!(movement_delta_secs(1.0), PLAYER_MAX_MOVE_DELTA);
        assert_eq!(movement_delta_secs(600.0), PLAYER_MAX_MOVE_DELTA);
        assert_eq!(movement_delta_secs(f32::INFINITY), PLAYER_MAX_MOVE_DELTA);
        assert_eq!(movement_delta_secs(-1.0), 0.0);
        assert_eq!(movement_delta_secs(f32::NAN), 0.0);
    }

    #[test]
    fn player_maximum_step_never_spans_a_radius_inflated_obstacle() {
        let step = PLAYER_SPEED * PLAYER_MAX_MOVE_DELTA;
        assert!(
            PLAYER_MAX_MOVE_DELTA <= Time::<Virtual>::default().max_delta().as_secs_f32(),
            "the movement clamp must match or be stricter than the virtual clock"
        );

        let colliders = crate::design::SceneBlueprint::v0().colliders;
        assert!(!colliders.is_empty());
        let mut narrowest = f32::MAX;
        for collider in &colliders {
            for (axis, thickness) in [
                ("x", 2.0 * (collider.half_extents.x + PLAYER_RADIUS)),
                ("z", 2.0 * (collider.half_extents.y + PLAYER_RADIUS)),
            ] {
                assert!(
                    step < thickness,
                    "one step of {step} spans {} on {axis} ({thickness})",
                    collider.id
                );
                narrowest = narrowest.min(thickness);
            }
        }
        assert_eq!(
            narrowest,
            2.0 * (0.2 + PLAYER_RADIUS),
            "the hose drop is the narrowest authored obstacle"
        );
    }

    #[test]
    fn player_clamped_step_cannot_tunnel_through_the_narrowest_hose_drop() {
        let colliders = colliders();
        let hose = colliders
            .get(&PropId::new("hose-drop-01"))
            .expect("the authored hose drop")
            .clone();
        let blocked_half = hose.half_extents.y + PLAYER_RADIUS;
        let from = Vec2::new(hose.center.x, hose.center.y - blocked_half - 0.25);
        let north = Vec2::new(0.0, 1.0);
        let hitch = 1.0;
        assert!(!colliders.overlaps(from, PLAYER_RADIUS));

        // Integrated raw, a hitch frame steps straight over the obstacle: this
        // is the tunnel the clamp exists to close.
        let raw = resolve_move(
            from,
            north * PLAYER_SPEED * hitch,
            &colliders,
            ROOM_SIZE,
            PLAYER_RADIUS,
        );
        assert_eq!(raw.blocked_z, None);
        assert!(
            raw.position.y > hose.center.y + blocked_half,
            "an unclamped hitch lands past the hose at {:?}",
            raw.position
        );

        // Clamped, the same hitch is stopped by the hose it would have crossed.
        let clamped = resolve_move(
            from,
            north * PLAYER_SPEED * movement_delta_secs(hitch),
            &colliders,
            ROOM_SIZE,
            PLAYER_RADIUS,
        );
        assert_eq!(
            clamped.blocked_z.as_ref().map(PropId::as_str),
            Some("hose-drop-01")
        );
        assert_eq!(clamped.position, from);
        assert_eq!(clamped.accepted, Vec2::ZERO);
    }

    #[test]
    fn player_rack_geometry_leaves_the_aisle_walkable_at_the_authored_speed() {
        let colliders = colliders();
        let step = PLAYER_SPEED / 60.0;
        assert!(step > 0.0 && step < PLAYER_RADIUS);

        for center_x in AISLE_CENTER_X {
            assert!(!colliders.overlaps(Vec2::new(center_x, 0.0), PLAYER_RADIUS));
        }
        for rack_x in RACK_ROW_X {
            assert!(colliders.overlaps(Vec2::new(rack_x, 0.0), PLAYER_RADIUS));
        }
    }
}
