use std::{
    collections::{BTreeSet, VecDeque},
    fmt,
};

use bevy::prelude::{Srgba, Vec2, Vec3};
use serde::{Deserialize, Serialize};

pub const DEFAULT_WINDOW_WIDTH: u32 = 1280;
pub const DEFAULT_WINDOW_HEIGHT: u32 = 720;
pub const VERIFICATION_WINDOW_WIDTH: u32 = 960;
pub const VERIFICATION_WINDOW_HEIGHT: u32 = 540;

pub const ROOM_SIZE: Vec2 = Vec2::new(40.0, 40.0);
/// Height of the low perimeter wall that frames the hall.
pub const WALL_HEIGHT: f32 = 1.2;
/// Thickness of the low perimeter wall. Walls sit immediately outside the play
/// area so a player clamped by the room bounds stops flush against them.
pub const WALL_THICKNESS: f32 = 0.4;
/// Painted floor markings are thin decals lifted just clear of the floor.
pub const FLOOR_MARKING_WIDTH: f32 = 0.12;
/// Height above the floor at which markings are drawn.
pub const FLOOR_MARKING_HEIGHT: f32 = 0.01;
/// Height of the overhead cable tray module origin.
pub const OVERHEAD_TRAY_HEIGHT: f32 = 4.0;
/// Height of the hose drop module origin, so its trunk stops under the tray.
pub const HOSE_DROP_HEIGHT: f32 = 2.05;
/// X positions of the four parallel rack rows.
pub const RACK_ROW_X: [f32; 4] = [-9.0, -3.0, 3.0, 9.0];
/// X positions of the three traversable aisles between the rack rows.
pub const AISLE_CENTER_X: [f32; 3] = [-6.0, 0.0, 6.0];
/// Half width of every aisle corridor.
pub const AISLE_HALF_WIDTH: f32 = 1.25;
/// Z extent of every aisle corridor.
pub const AISLE_Z_MIN: f32 = -12.0;
/// Z extent of every aisle corridor.
pub const AISLE_Z_MAX: f32 = 12.0;
/// Spacing between the aisle centreline checkpoints the flood fill must reach.
pub const AISLE_CHECKPOINT_SPACING: f32 = 2.0;
/// Cell size of the walkability grid used by the reachability flood fill.
pub const WALKABLE_CELL_SIZE: f32 = 0.25;
/// Z position of the hose drops that hang from the overhead trays.
pub const HOSE_DROP_Z: f32 = 7.0;
/// Length of the painted cross-hall walkway markings.
pub const WALKWAY_MARKING_LENGTH: f32 = 32.0;
pub const ORTHOGRAPHIC_WIDTH: f32 = 26.0;
pub const ORTHOGRAPHIC_HEIGHT: f32 = 14.625;
pub const INITIAL_CAMERA_YAW_DEGREES: f32 = 45.0;
pub const CAMERA_ELEVATION_DEGREES: f32 = 57.0;
#[allow(clippy::excessive_precision)]
pub const CAMERA_OFFSET_DIRECTION: Vec3 = Vec3::new(1.0, 2.177_697_9, 1.0);
pub const CAMERA_ORBIT_DURATION_SECONDS: f32 = 0.30;

pub const FAULT_INTERVAL_SECONDS: f32 = 4.0;
pub const MAX_ACTIVE_TICKETS: usize = 3;
pub const PLAYER_RADIUS: f32 = 0.35;
pub const REPAIR_INTERACTION_RANGE: f32 = 1.5;
pub const REPAIR_DURATION_SECONDS: f32 = 3.0;
pub const RESOLVED_DISPLAY_SECONDS: f32 = 2.0;
pub const RACK_COOLDOWN_SECONDS: f32 = 8.0;

pub const KEY_ART_REFERENCE_PATH: &str = "docs/reference/cel-shift-key-art.png";
pub const CHARACTER_SHEET_REFERENCE_PATH: &str = "docs/reference/cel-shift-character-sheet.png";
pub const KEY_ART_SHA256: &str = "a30e12b63a36743015b1c73eeca6248a8b8ee974cf007f23666dc101f06c0e75";
pub const CHARACTER_SHEET_SHA256: &str =
    "8a5a31e7bceb8ad16b3481d2bae89e7a32bb4edd0ef711b7d07a26f177cf6b25";

macro_rules! srgba_u8 {
    ($red:expr, $green:expr, $blue:expr) => {
        Srgba::new(
            $red as f32 / 255.0,
            $green as f32 / 255.0,
            $blue as f32 / 255.0,
            1.0,
        )
    };
}

pub const RACK_WHITE: Srgba = srgba_u8!(0xFB, 0xFC, 0xFD);
pub const RACK_SHADOW: Srgba = srgba_u8!(0xC6, 0xD5, 0xE0);
pub const FLOOR_LIGHT: Srgba = srgba_u8!(0xDE, 0xE6, 0xEB);
pub const FLOOR_SHADOW: Srgba = srgba_u8!(0xB2, 0xC0, 0xCB);
pub const SIGNATURE_YELLOW: Srgba = srgba_u8!(0xFF, 0xC9, 0x3C);
pub const TEAL_ACCENT: Srgba = srgba_u8!(0x2F, 0xB8, 0xA8);
pub const HOSE_CHARCOAL: Srgba = srgba_u8!(0x2E, 0x35, 0x3B);
pub const INK: Srgba = srgba_u8!(0x1F, 0x2A, 0x33);
pub const SKY_BOUNCE_BLUE: Srgba = srgba_u8!(0x9F, 0xD0, 0xF0);
pub const HEALTHY_GREEN: Srgba = srgba_u8!(0x4A, 0xDE, 0x80);
pub const FAULT_RED: Srgba = srgba_u8!(0xFF, 0x4B, 0x4B);
pub const WORKER_HI_VIS: Srgba = srgba_u8!(0xC8, 0xD9, 0x4A);
pub const WORKER_SLATE: Srgba = srgba_u8!(0x55, 0x70, 0x7F);
pub const WORKER_TROUSERS: Srgba = srgba_u8!(0x2F, 0x3A, 0x42);
pub const WORKER_BOOTS: Srgba = srgba_u8!(0x7A, 0x52, 0x33);
pub const WORKER_HARD_HAT: Srgba = srgba_u8!(0x2C, 0x6F, 0xB8);
pub const WORKER_SKIN: Srgba = srgba_u8!(0xC9, 0x8F, 0x6A);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum PaletteRole {
    RackWhite,
    RackShadow,
    FloorLight,
    FloorShadow,
    SignatureYellow,
    TealAccent,
    HoseCharcoal,
    Ink,
    SkyBounceBlue,
    HealthyGreen,
    FaultRed,
    WorkerHiVis,
    WorkerSlate,
    WorkerTrousers,
    WorkerBoots,
    WorkerHardHat,
    WorkerSkin,
}

impl PaletteRole {
    pub const ALL: [Self; 17] = [
        Self::RackWhite,
        Self::RackShadow,
        Self::FloorLight,
        Self::FloorShadow,
        Self::SignatureYellow,
        Self::TealAccent,
        Self::HoseCharcoal,
        Self::Ink,
        Self::SkyBounceBlue,
        Self::HealthyGreen,
        Self::FaultRed,
        Self::WorkerHiVis,
        Self::WorkerSlate,
        Self::WorkerTrousers,
        Self::WorkerBoots,
        Self::WorkerHardHat,
        Self::WorkerSkin,
    ];

    pub const fn color(self) -> Srgba {
        match self {
            Self::RackWhite => RACK_WHITE,
            Self::RackShadow => RACK_SHADOW,
            Self::FloorLight => FLOOR_LIGHT,
            Self::FloorShadow => FLOOR_SHADOW,
            Self::SignatureYellow => SIGNATURE_YELLOW,
            Self::TealAccent => TEAL_ACCENT,
            Self::HoseCharcoal => HOSE_CHARCOAL,
            Self::Ink => INK,
            Self::SkyBounceBlue => SKY_BOUNCE_BLUE,
            Self::HealthyGreen => HEALTHY_GREEN,
            Self::FaultRed => FAULT_RED,
            Self::WorkerHiVis => WORKER_HI_VIS,
            Self::WorkerSlate => WORKER_SLATE,
            Self::WorkerTrousers => WORKER_TROUSERS,
            Self::WorkerBoots => WORKER_BOOTS,
            Self::WorkerHardHat => WORKER_HARD_HAT,
            Self::WorkerSkin => WORKER_SKIN,
        }
    }

    pub const fn hex(self) -> &'static str {
        match self {
            Self::RackWhite => "#FBFCFD",
            Self::RackShadow => "#C6D5E0",
            Self::FloorLight => "#DEE6EB",
            Self::FloorShadow => "#B2C0CB",
            Self::SignatureYellow => "#FFC93C",
            Self::TealAccent => "#2FB8A8",
            Self::HoseCharcoal => "#2E353B",
            Self::Ink => "#1F2A33",
            Self::SkyBounceBlue => "#9FD0F0",
            Self::HealthyGreen => "#4ADE80",
            Self::FaultRed => "#FF4B4B",
            Self::WorkerHiVis => "#C8D94A",
            Self::WorkerSlate => "#55707F",
            Self::WorkerTrousers => "#2F3A42",
            Self::WorkerBoots => "#7A5233",
            Self::WorkerHardHat => "#2C6FB8",
            Self::WorkerSkin => "#C98F6A",
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PropId(String);

impl PropId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PropId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reusable unit meshes. One mesh handle per shape is created at startup and
/// shared by every prop that is not a generated glTF module.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum PrimitiveShape {
    /// Unit cube centred on its origin.
    Cuboid,
    /// Unit quad on the XZ plane facing +Y.
    Quad,
}

impl PrimitiveShape {
    /// Every shape, in stable order.
    pub const ALL: [Self; 2] = [Self::Cuboid, Self::Quad];
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum AssetKind {
    Floor,
    Wall,
    RackRow,
    CoolingUnit,
    OverheadTray,
    HoseDrop,
    UtilityCart,
    StepStool,
    FloorMarking,
}

impl AssetKind {
    /// Every asset kind, in stable order.
    pub const ALL: [Self; 9] = [
        Self::Floor,
        Self::Wall,
        Self::RackRow,
        Self::CoolingUnit,
        Self::OverheadTray,
        Self::HoseDrop,
        Self::UtilityCart,
        Self::StepStool,
        Self::FloorMarking,
    ];

    /// The cached unit mesh and palette role for kinds drawn from primitives.
    /// Kinds that resolve to a generated glTF module return `None`; the two
    /// sets are disjoint and jointly exhaustive.
    pub const fn primitive(self) -> Option<(PrimitiveShape, PaletteRole)> {
        match self {
            Self::Floor => Some((PrimitiveShape::Quad, PaletteRole::FloorLight)),
            Self::Wall => Some((PrimitiveShape::Cuboid, PaletteRole::FloorShadow)),
            Self::FloorMarking => Some((PrimitiveShape::Quad, PaletteRole::SignatureYellow)),
            Self::RackRow
            | Self::CoolingUnit
            | Self::OverheadTray
            | Self::HoseDrop
            | Self::UtilityCart
            | Self::StepStool => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransformSpec {
    pub translation: Vec3,
    pub rotation_y_degrees: f32,
    pub scale: Vec3,
}

impl TransformSpec {
    pub const fn from_translation(translation: Vec3) -> Self {
        Self {
            translation,
            rotation_y_degrees: 0.0,
            scale: Vec3::ONE,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisualSpec {
    pub id: PropId,
    pub asset: AssetKind,
    pub transform: TransformSpec,
    pub collision_required: bool,
}

impl VisualSpec {
    pub fn new(
        id: impl Into<String>,
        asset: AssetKind,
        translation: Vec3,
        collision_required: bool,
    ) -> Self {
        Self {
            id: PropId::new(id),
            asset,
            transform: TransformSpec::from_translation(translation),
            collision_required,
        }
    }

    /// Sets the authored non-uniform scale applied to a unit primitive.
    pub fn with_scale(mut self, scale: Vec3) -> Self {
        self.transform.scale = scale;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColliderSpec {
    pub id: PropId,
    pub center: Vec2,
    pub half_extents: Vec2,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AisleSpec {
    pub center_x: f32,
    pub z_min: f32,
    pub z_max: f32,
    pub half_width: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoomSpec {
    pub size: Vec2,
}

impl RoomSpec {
    fn contains_point(self, point: Vec2) -> bool {
        let half_size = self.size * 0.5;
        point.x.abs() <= half_size.x && point.y.abs() <= half_size.y
    }

    fn contains_collider(self, collider: &ColliderSpec) -> bool {
        let half_size = self.size * 0.5;
        collider.center.x.abs() + collider.half_extents.x <= half_size.x
            && collider.center.y.abs() + collider.half_extents.y <= half_size.y
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneBlueprint {
    pub room: RoomSpec,
    pub visuals: Vec<VisualSpec>,
    pub colliders: Vec<ColliderSpec>,
    pub aisles: Vec<AisleSpec>,
    pub player_spawn: Vec2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SceneValidationError {
    DuplicateVisualId(PropId),
    DuplicateColliderId(PropId),
    MissingRequiredCollider(PropId),
    OrphanCollider(PropId),
    ColliderOutsideRoom(PropId),
    PlayerSpawnOutsideRoom,
    PlayerSpawnInsideCollider(PropId),
    RackRowCount { expected: usize, actual: usize },
    AisleCount { expected: usize, actual: usize },
    BlockedAisle { index: usize },
    EmptyCameraTargetInterval { yaw_degrees: u16 },
}

/// One aisle centreline sample that the walkability flood fill must reach from
/// the player spawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AisleCheckpoint {
    /// Index of the aisle in [`SceneBlueprint::aisles`].
    pub aisle: usize,
    /// Index of the sample along that aisle, from `z_min` to `z_max`.
    pub index: usize,
    /// Sampled ground position.
    pub point: Vec2,
}

/// Result of the room-wide walkability flood fill.
#[derive(Clone, Debug, PartialEq)]
pub struct WalkableReport {
    /// Grid resolution used for the fill.
    pub cell_size: f32,
    /// Number of grid nodes reachable from the player spawn.
    pub reachable_cells: usize,
    /// Number of grid nodes in the room.
    pub total_cells: usize,
    /// Aisle checkpoints that do not share the spawn's walkable component.
    pub unreachable: Vec<AisleCheckpoint>,
    /// Narrowest walkable width measured across any aisle checkpoint, sampled
    /// on the same grid. Connectivity alone would accept a hairline gap between
    /// two colliders, so this records how much room the technician really has.
    pub narrowest_aisle_clearance: f32,
}

impl WalkableReport {
    /// True when every aisle checkpoint shares one walkable component with the
    /// player spawn.
    pub fn is_connected(&self) -> bool {
        self.unreachable.is_empty()
    }
}

impl SceneBlueprint {
    /// The authored 40 m square data hall: a polished floor inside low
    /// perimeter walls, four rack rows separated by three traversable aisles,
    /// four cooling units, overhead trays with hose drops, painted yellow
    /// markings, the red service cart, and the yellow step stool.
    pub fn v0() -> Self {
        let mut visuals = Vec::new();
        let mut colliders = Vec::new();

        visuals.push(
            VisualSpec::new("floor", AssetKind::Floor, Vec3::ZERO, false).with_scale(Vec3::new(
                ROOM_SIZE.x,
                1.0,
                ROOM_SIZE.y,
            )),
        );

        let wall_offset = Vec2::new(
            ROOM_SIZE.x * 0.5 + WALL_THICKNESS * 0.5,
            ROOM_SIZE.y * 0.5 + WALL_THICKNESS * 0.5,
        );
        let wall_span = Vec2::new(
            ROOM_SIZE.x + WALL_THICKNESS * 2.0,
            ROOM_SIZE.y + WALL_THICKNESS * 2.0,
        );
        for (id, translation, scale) in [
            (
                "wall-north",
                Vec3::new(0.0, WALL_HEIGHT * 0.5, -wall_offset.y),
                Vec3::new(wall_span.x, WALL_HEIGHT, WALL_THICKNESS),
            ),
            (
                "wall-south",
                Vec3::new(0.0, WALL_HEIGHT * 0.5, wall_offset.y),
                Vec3::new(wall_span.x, WALL_HEIGHT, WALL_THICKNESS),
            ),
            (
                "wall-west",
                Vec3::new(-wall_offset.x, WALL_HEIGHT * 0.5, 0.0),
                Vec3::new(WALL_THICKNESS, WALL_HEIGHT, wall_span.y),
            ),
            (
                "wall-east",
                Vec3::new(wall_offset.x, WALL_HEIGHT * 0.5, 0.0),
                Vec3::new(WALL_THICKNESS, WALL_HEIGHT, wall_span.y),
            ),
        ] {
            visuals
                .push(VisualSpec::new(id, AssetKind::Wall, translation, false).with_scale(scale));
        }

        for (index, x) in RACK_ROW_X.into_iter().enumerate() {
            add_colliding_prop(
                &mut visuals,
                &mut colliders,
                &format!("rack-row-{:02}", index + 1),
                AssetKind::RackRow,
                Vec3::new(x, 0.0, 0.0),
                Vec2::new(x, 0.0),
                Vec2::new(0.8, 8.05),
            );
        }

        for (id, center) in [
            ("cooling-unit-west-north", Vec2::new(-13.0, -6.0)),
            ("cooling-unit-west-south", Vec2::new(-13.0, 6.0)),
            ("cooling-unit-east-north", Vec2::new(13.0, -6.0)),
            ("cooling-unit-east-south", Vec2::new(13.0, 6.0)),
        ] {
            add_colliding_prop(
                &mut visuals,
                &mut colliders,
                id,
                AssetKind::CoolingUnit,
                Vec3::new(center.x, 0.0, center.y),
                center,
                Vec2::new(1.05, 2.05),
            );
        }

        for (index, x) in AISLE_CENTER_X.into_iter().enumerate() {
            visuals.push(VisualSpec::new(
                format!("overhead-tray-{:02}", index + 1),
                AssetKind::OverheadTray,
                Vec3::new(x, OVERHEAD_TRAY_HEIGHT, 0.0),
                false,
            ));
        }
        for (index, x) in AISLE_CENTER_X.into_iter().enumerate() {
            add_colliding_prop(
                &mut visuals,
                &mut colliders,
                &format!("hose-drop-{:02}", index + 1),
                AssetKind::HoseDrop,
                Vec3::new(x, HOSE_DROP_HEIGHT, HOSE_DROP_Z),
                Vec2::new(x, HOSE_DROP_Z),
                Vec2::splat(0.2),
            );
        }

        add_colliding_prop(
            &mut visuals,
            &mut colliders,
            "utility-cart",
            AssetKind::UtilityCart,
            Vec3::new(-13.0, 0.0, -10.0),
            Vec2::new(-13.0, -10.0),
            Vec2::new(0.95, 0.65),
        );
        add_colliding_prop(
            &mut visuals,
            &mut colliders,
            "step-stool",
            AssetKind::StepStool,
            Vec3::new(13.0, 0.0, 10.0),
            Vec2::new(13.0, 10.0),
            Vec2::splat(0.6),
        );

        for (index, x) in AISLE_CENTER_X.into_iter().enumerate() {
            for (side, offset) in [("west", -AISLE_HALF_WIDTH), ("east", AISLE_HALF_WIDTH)] {
                visuals.push(
                    VisualSpec::new(
                        format!("floor-marking-aisle-{:02}-{side}", index + 1),
                        AssetKind::FloorMarking,
                        Vec3::new(x + offset, FLOOR_MARKING_HEIGHT, 0.0),
                        false,
                    )
                    .with_scale(Vec3::new(
                        FLOOR_MARKING_WIDTH,
                        1.0,
                        AISLE_Z_MAX - AISLE_Z_MIN,
                    )),
                );
            }
        }
        for (id, z) in [
            ("floor-marking-walkway-north", -14.0),
            ("floor-marking-walkway-south", 14.0),
        ] {
            visuals.push(
                VisualSpec::new(
                    id,
                    AssetKind::FloorMarking,
                    Vec3::new(0.0, FLOOR_MARKING_HEIGHT, z),
                    false,
                )
                .with_scale(Vec3::new(
                    WALKWAY_MARKING_LENGTH,
                    1.0,
                    FLOOR_MARKING_WIDTH,
                )),
            );
        }

        Self {
            room: RoomSpec { size: ROOM_SIZE },
            visuals,
            colliders,
            aisles: AISLE_CENTER_X
                .into_iter()
                .map(|center_x| AisleSpec {
                    center_x,
                    z_min: AISLE_Z_MIN,
                    z_max: AISLE_Z_MAX,
                    half_width: AISLE_HALF_WIDTH,
                })
                .collect(),
            player_spawn: Vec2::new(AISLE_CENTER_X[0], -11.0),
        }
    }

    /// Looks up one authored visual by its stable [`PropId`] text.
    pub fn visual(&self, id: &str) -> Option<&VisualSpec> {
        self.visuals.iter().find(|visual| visual.id.as_str() == id)
    }

    /// Looks up one authored collider by its stable [`PropId`] text.
    pub fn collider(&self, id: &str) -> Option<&ColliderSpec> {
        self.colliders
            .iter()
            .find(|collider| collider.id.as_str() == id)
    }

    /// Every authored visual of one kind, in declaration order.
    pub fn visuals_of(&self, kind: AssetKind) -> impl Iterator<Item = &VisualSpec> {
        self.visuals
            .iter()
            .filter(move |visual| visual.asset == kind)
    }

    /// Number of authored visuals of one kind.
    pub fn count_of(&self, kind: AssetKind) -> usize {
        self.visuals_of(kind).count()
    }

    /// Centreline samples of every aisle, from `z_min` to `z_max`.
    pub fn aisle_checkpoints(&self) -> Vec<AisleCheckpoint> {
        let mut checkpoints = Vec::new();
        for (aisle_index, aisle) in self.aisles.iter().enumerate() {
            let span = aisle.z_max - aisle.z_min;
            if span <= 0.0 {
                continue;
            }
            let samples = (span / AISLE_CHECKPOINT_SPACING).round().max(1.0) as usize;
            for index in 0..=samples {
                let fraction = index as f32 / samples as f32;
                checkpoints.push(AisleCheckpoint {
                    aisle: aisle_index,
                    index,
                    point: Vec2::new(aisle.center_x, aisle.z_min + fraction * span),
                });
            }
        }
        checkpoints
    }

    /// Floods the walkable grid from the player spawn and reports which aisle
    /// checkpoints fail to share that component.
    pub fn walkable_report(&self) -> WalkableReport {
        let grid = WalkableGrid::new(self.room, &self.colliders);
        let reachable = grid.flood_from(self.player_spawn);
        let unreachable = self
            .aisle_checkpoints()
            .into_iter()
            .filter(|checkpoint| {
                !grid
                    .node(checkpoint.point)
                    .is_some_and(|index| reachable[index])
            })
            .collect();

        let narrowest_aisle_clearance = self
            .aisle_checkpoints()
            .into_iter()
            .map(|checkpoint| {
                let aisle = self.aisles[checkpoint.aisle];
                grid.clear_width(checkpoint.point.y, aisle.center_x, aisle.half_width)
            })
            .fold(f32::INFINITY, f32::min);

        WalkableReport {
            cell_size: WALKABLE_CELL_SIZE,
            reachable_cells: reachable.iter().filter(|open| **open).count(),
            total_cells: grid.columns * grid.rows,
            unreachable,
            narrowest_aisle_clearance: if narrowest_aisle_clearance.is_finite() {
                narrowest_aisle_clearance
            } else {
                0.0
            },
        }
    }

    pub fn rack_row_count(&self) -> usize {
        self.visuals
            .iter()
            .filter(|visual| visual.asset == AssetKind::RackRow)
            .count()
    }

    pub fn validate(&self) -> Vec<SceneValidationError> {
        let mut errors = Vec::new();
        append_duplicate_errors(
            self.visuals.iter().map(|visual| &visual.id),
            &mut errors,
            SceneValidationError::DuplicateVisualId,
        );
        append_duplicate_errors(
            self.colliders.iter().map(|collider| &collider.id),
            &mut errors,
            SceneValidationError::DuplicateColliderId,
        );

        let visual_ids = self
            .visuals
            .iter()
            .map(|visual| &visual.id)
            .collect::<BTreeSet<_>>();
        let collider_ids = self
            .colliders
            .iter()
            .map(|collider| &collider.id)
            .collect::<BTreeSet<_>>();

        let mut missing = BTreeSet::new();
        for visual in &self.visuals {
            if visual.collision_required
                && !collider_ids.contains(&visual.id)
                && missing.insert(visual.id.clone())
            {
                errors.push(SceneValidationError::MissingRequiredCollider(
                    visual.id.clone(),
                ));
            }
        }

        let mut orphaned = BTreeSet::new();
        for collider in &self.colliders {
            if !visual_ids.contains(&collider.id) && orphaned.insert(collider.id.clone()) {
                errors.push(SceneValidationError::OrphanCollider(collider.id.clone()));
            }
            if !self.room.contains_collider(collider) {
                errors.push(SceneValidationError::ColliderOutsideRoom(
                    collider.id.clone(),
                ));
            }
        }

        let mut spawn_is_walkable = self.room.contains_point(self.player_spawn);
        if !spawn_is_walkable {
            errors.push(SceneValidationError::PlayerSpawnOutsideRoom);
        } else {
            for collider in &self.colliders {
                if point_inside_collider(self.player_spawn, collider, PLAYER_RADIUS) {
                    spawn_is_walkable = false;
                    errors.push(SceneValidationError::PlayerSpawnInsideCollider(
                        collider.id.clone(),
                    ));
                }
            }
        }

        let rack_rows = self.rack_row_count();
        if rack_rows != 4 {
            errors.push(SceneValidationError::RackRowCount {
                expected: 4,
                actual: rack_rows,
            });
        }
        if self.aisles.len() != 3 {
            errors.push(SceneValidationError::AisleCount {
                expected: 3,
                actual: self.aisles.len(),
            });
        }

        // Reachability is only meaningful from a walkable spawn; an invalid
        // spawn is already reported above and would blame every aisle.
        if spawn_is_walkable {
            let mut blocked = BTreeSet::new();
            for checkpoint in self.walkable_report().unreachable {
                if blocked.insert(checkpoint.aisle) {
                    errors.push(SceneValidationError::BlockedAisle {
                        index: checkpoint.aisle,
                    });
                }
            }
        }

        for yaw_degrees in [45_u16, 90, 135, 180, 225, 270, 315, 0] {
            if camera_target_interval(self.room, yaw_degrees as f32).is_none() {
                errors.push(SceneValidationError::EmptyCameraTargetInterval { yaw_degrees });
            }
        }

        errors
    }
}

fn add_colliding_prop(
    visuals: &mut Vec<VisualSpec>,
    colliders: &mut Vec<ColliderSpec>,
    id: &str,
    asset: AssetKind,
    translation: Vec3,
    center: Vec2,
    half_extents: Vec2,
) {
    visuals.push(VisualSpec::new(id, asset, translation, true));
    colliders.push(ColliderSpec {
        id: PropId::new(id),
        center,
        half_extents,
    });
}

fn append_duplicate_errors<'a>(
    ids: impl Iterator<Item = &'a PropId>,
    errors: &mut Vec<SceneValidationError>,
    make_error: impl Fn(PropId) -> SceneValidationError,
) {
    let mut seen = BTreeSet::new();
    let mut reported = BTreeSet::new();
    for id in ids {
        if !seen.insert(id.clone()) && reported.insert(id.clone()) {
            errors.push(make_error(id.clone()));
        }
    }
}

fn point_inside_collider(point: Vec2, collider: &ColliderSpec, padding: f32) -> bool {
    let delta = (point - collider.center).abs();
    delta.x <= collider.half_extents.x + padding && delta.y <= collider.half_extents.y + padding
}

/// Uniform grid of candidate standing positions over the whole room. A node is
/// open when the player's disc fits there without leaving the room or entering
/// any collider, so a flood fill over the grid answers "can the technician walk
/// from here to there" for the single authored hall.
struct WalkableGrid {
    origin: Vec2,
    columns: usize,
    rows: usize,
    open: Vec<bool>,
}

impl WalkableGrid {
    fn new(room: RoomSpec, colliders: &[ColliderSpec]) -> Self {
        let half_size = room.size * 0.5;
        let origin = -half_size;
        let columns = (room.size.x / WALKABLE_CELL_SIZE).round().max(1.0) as usize + 1;
        let rows = (room.size.y / WALKABLE_CELL_SIZE).round().max(1.0) as usize + 1;
        let mut open = Vec::with_capacity(columns * rows);
        for row in 0..rows {
            for column in 0..columns {
                let point = origin + Vec2::new(column as f32, row as f32) * WALKABLE_CELL_SIZE;
                let inside_room = point.x.abs() <= half_size.x - PLAYER_RADIUS
                    && point.y.abs() <= half_size.y - PLAYER_RADIUS;
                let clear = colliders
                    .iter()
                    .all(|collider| !point_inside_collider(point, collider, PLAYER_RADIUS));
                open.push(inside_room && clear);
            }
        }

        Self {
            origin,
            columns,
            rows,
            open,
        }
    }

    /// Index of the grid node nearest `point`, when that node is open.
    fn node(&self, point: Vec2) -> Option<usize> {
        let offset = (point - self.origin) / WALKABLE_CELL_SIZE;
        let column = offset.x.round();
        let row = offset.y.round();
        if column < 0.0 || row < 0.0 {
            return None;
        }
        let (column, row) = (column as usize, row as usize);
        if column >= self.columns || row >= self.rows {
            return None;
        }
        let index = row * self.columns + column;
        self.open[index].then_some(index)
    }

    /// Longest contiguous run of open nodes across one aisle corridor at `z`,
    /// expressed as a width in metres.
    fn clear_width(&self, z: f32, center_x: f32, half_width: f32) -> f32 {
        let row = ((z - self.origin.y) / WALKABLE_CELL_SIZE).round();
        if row < 0.0 || row as usize >= self.rows {
            return 0.0;
        }
        let row = row as usize;

        let mut longest = 0usize;
        let mut run = 0usize;
        let mut column = 0usize;
        while column < self.columns {
            let x = self.origin.x + column as f32 * WALKABLE_CELL_SIZE;
            if (x - center_x).abs() <= half_width + f32::EPSILON * 16.0 {
                if self.open[row * self.columns + column] {
                    run += 1;
                    longest = longest.max(run);
                } else {
                    run = 0;
                }
            }
            column += 1;
        }

        longest.saturating_sub(1) as f32 * WALKABLE_CELL_SIZE
    }

    fn flood_from(&self, start: Vec2) -> Vec<bool> {
        let mut visited = vec![false; self.open.len()];
        let Some(seed) = self.node(start) else {
            return visited;
        };

        let mut queue = VecDeque::from([seed]);
        visited[seed] = true;
        while let Some(index) = queue.pop_front() {
            let column = index % self.columns;
            let row = index / self.columns;
            for (next_column, next_row) in grid_neighbors(column, row, self.columns, self.rows) {
                let next = next_row * self.columns + next_column;
                if self.open[next] && !visited[next] {
                    visited[next] = true;
                    queue.push_back(next);
                }
            }
        }

        visited
    }
}

fn grid_neighbors(
    column: usize,
    row: usize,
    columns: usize,
    rows: usize,
) -> impl Iterator<Item = (usize, usize)> {
    let mut neighbors = [(0, 0); 4];
    let mut count = 0;
    if column > 0 {
        neighbors[count] = (column - 1, row);
        count += 1;
    }
    if column + 1 < columns {
        neighbors[count] = (column + 1, row);
        count += 1;
    }
    if row > 0 {
        neighbors[count] = (column, row - 1);
        count += 1;
    }
    if row + 1 < rows {
        neighbors[count] = (column, row + 1);
        count += 1;
    }
    neighbors.into_iter().take(count)
}

fn camera_target_interval(room: RoomSpec, yaw_degrees: f32) -> Option<(Vec2, Vec2)> {
    let yaw = yaw_degrees.to_radians();
    let half_view_width = ORTHOGRAPHIC_WIDTH * 0.5;
    let half_ground_depth = ORTHOGRAPHIC_HEIGHT * 0.5 / CAMERA_ELEVATION_DEGREES.to_radians().sin();
    let footprint = Vec2::new(
        yaw.cos().abs() * half_view_width + yaw.sin().abs() * half_ground_depth,
        yaw.sin().abs() * half_view_width + yaw.cos().abs() * half_ground_depth,
    );
    let half_room = room.size * 0.5;
    let remaining = half_room - footprint;

    (remaining.min_element() >= 0.0).then(|| (-remaining, remaining))
}
