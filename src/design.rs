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
pub const ORTHOGRAPHIC_WIDTH: f32 = 26.0;
pub const ORTHOGRAPHIC_HEIGHT: f32 = 14.625;
pub const INITIAL_CAMERA_YAW_DEGREES: f32 = 45.0;
pub const CAMERA_ELEVATION_DEGREES: f32 = 57.0;
#[allow(clippy::excessive_precision)]
pub const CAMERA_OFFSET_DIRECTION: Vec3 = Vec3::new(1.0, 2.177_697_9, 1.0);
pub const CAMERA_ORBIT_DURATION_SECONDS: f32 = 0.30;

pub const FAULT_INTERVAL_SECONDS: f32 = 4.0;
pub const MAX_ACTIVE_TICKETS: usize = 3;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AssetKind {
    RackRow,
    CoolingUnit,
    OverheadTray,
    HoseDrop,
    UtilityCart,
    StepStool,
    FloorMarking,
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

impl SceneBlueprint {
    pub fn v0() -> Self {
        let mut visuals = Vec::new();
        let mut colliders = Vec::new();

        for (index, x) in [-9.0, -3.0, 3.0, 9.0].into_iter().enumerate() {
            let id = format!("rack-row-{:02}", index + 1);
            visuals.push(VisualSpec::new(
                id.clone(),
                AssetKind::RackRow,
                Vec3::new(x, 0.0, 0.0),
                true,
            ));
            colliders.push(ColliderSpec {
                id: PropId::new(id),
                center: Vec2::new(x, 0.0),
                half_extents: Vec2::new(0.75, 8.0),
            });
        }

        add_colliding_prop(
            &mut visuals,
            &mut colliders,
            "cooling-unit-west",
            AssetKind::CoolingUnit,
            Vec2::new(-13.0, 0.0),
            Vec2::new(1.0, 2.0),
        );
        add_colliding_prop(
            &mut visuals,
            &mut colliders,
            "cooling-unit-east",
            AssetKind::CoolingUnit,
            Vec2::new(13.0, 0.0),
            Vec2::new(1.0, 2.0),
        );
        visuals.push(VisualSpec::new(
            "overhead-tray-center",
            AssetKind::OverheadTray,
            Vec3::new(0.0, 4.0, 0.0),
            false,
        ));
        visuals.push(VisualSpec::new(
            "hose-drop-01",
            AssetKind::HoseDrop,
            Vec3::new(-9.0, 2.0, -4.0),
            false,
        ));
        add_colliding_prop(
            &mut visuals,
            &mut colliders,
            "utility-cart",
            AssetKind::UtilityCart,
            Vec2::new(-13.0, -10.0),
            Vec2::new(1.0, 0.6),
        );
        add_colliding_prop(
            &mut visuals,
            &mut colliders,
            "step-stool",
            AssetKind::StepStool,
            Vec2::new(13.0, 10.0),
            Vec2::splat(0.6),
        );
        visuals.push(VisualSpec::new(
            "floor-marking-01",
            AssetKind::FloorMarking,
            Vec3::new(0.0, 0.01, 0.0),
            false,
        ));

        Self {
            room: RoomSpec { size: ROOM_SIZE },
            visuals,
            colliders,
            aisles: vec![
                AisleSpec {
                    center_x: -6.0,
                    z_min: -12.0,
                    z_max: 12.0,
                    half_width: 1.25,
                },
                AisleSpec {
                    center_x: 0.0,
                    z_min: -12.0,
                    z_max: 12.0,
                    half_width: 1.25,
                },
                AisleSpec {
                    center_x: 6.0,
                    z_min: -12.0,
                    z_max: 12.0,
                    half_width: 1.25,
                },
            ],
            player_spawn: Vec2::new(-6.0, -11.0),
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

        if !self.room.contains_point(self.player_spawn) {
            errors.push(SceneValidationError::PlayerSpawnOutsideRoom);
        } else {
            for collider in &self.colliders {
                if point_inside_collider(self.player_spawn, collider, 0.0) {
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

        for (index, aisle) in self.aisles.iter().enumerate() {
            if !aisle_is_traversable(*aisle, &self.colliders) {
                errors.push(SceneValidationError::BlockedAisle { index });
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
    center: Vec2,
    half_extents: Vec2,
) {
    visuals.push(VisualSpec::new(
        id,
        asset,
        Vec3::new(center.x, 0.0, center.y),
        true,
    ));
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

fn aisle_is_traversable(aisle: AisleSpec, colliders: &[ColliderSpec]) -> bool {
    const CELL_SIZE: f32 = 0.25;
    const PLAYER_RADIUS: f32 = 0.35;

    if aisle.half_width <= 0.0 || aisle.z_min >= aisle.z_max {
        return false;
    }

    let columns = ((aisle.half_width * 2.0 / CELL_SIZE).round() as usize).max(1) + 1;
    let rows = (((aisle.z_max - aisle.z_min) / CELL_SIZE).round() as usize).max(1) + 1;
    let mut visited = vec![false; columns * rows];
    let mut queue = VecDeque::new();

    for (column, visited_cell) in visited.iter_mut().take(columns).enumerate() {
        if !aisle_cell_blocked(aisle, column, 0, columns, rows, colliders, PLAYER_RADIUS) {
            *visited_cell = true;
            queue.push_back((column, 0));
        }
    }

    while let Some((column, row)) = queue.pop_front() {
        if row == rows - 1 {
            return true;
        }

        for (next_column, next_row) in grid_neighbors(column, row, columns, rows) {
            let index = next_row * columns + next_column;
            if !visited[index]
                && !aisle_cell_blocked(
                    aisle,
                    next_column,
                    next_row,
                    columns,
                    rows,
                    colliders,
                    PLAYER_RADIUS,
                )
            {
                visited[index] = true;
                queue.push_back((next_column, next_row));
            }
        }
    }

    false
}

fn aisle_cell_blocked(
    aisle: AisleSpec,
    column: usize,
    row: usize,
    columns: usize,
    rows: usize,
    colliders: &[ColliderSpec],
    player_radius: f32,
) -> bool {
    let x_fraction = column as f32 / (columns - 1) as f32;
    let z_fraction = row as f32 / (rows - 1) as f32;
    let point = Vec2::new(
        aisle.center_x - aisle.half_width + x_fraction * aisle.half_width * 2.0,
        aisle.z_min + z_fraction * (aisle.z_max - aisle.z_min),
    );

    colliders
        .iter()
        .any(|collider| point_inside_collider(point, collider, player_radius))
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
