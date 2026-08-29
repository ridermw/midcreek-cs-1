use std::{fs::File, io::Read, path::Path};

use bevy::{color::palettes::css::BLACK, prelude::*};
use midcreek_cs_1::{
    CellShiftPlugin, CellShiftSet,
    design::{
        AssetKind, CAMERA_ELEVATION_DEGREES, CAMERA_OFFSET_DIRECTION,
        CAMERA_ORBIT_DURATION_SECONDS, CHARACTER_SHEET_REFERENCE_PATH, CHARACTER_SHEET_SHA256,
        ColliderSpec, DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, FAULT_INTERVAL_SECONDS,
        FAULT_RED, FLOOR_LIGHT, FLOOR_SHADOW, HEALTHY_GREEN, HOSE_CHARCOAL,
        INITIAL_CAMERA_YAW_DEGREES, INK, KEY_ART_REFERENCE_PATH, KEY_ART_SHA256,
        MAX_ACTIVE_TICKETS, ORTHOGRAPHIC_HEIGHT, ORTHOGRAPHIC_WIDTH, PLAYER_RADIUS, PaletteRole,
        PropId, RACK_COOLDOWN_SECONDS, RACK_SHADOW, RACK_WHITE, REPAIR_DURATION_SECONDS,
        REPAIR_INTERACTION_RANGE, RESOLVED_DISPLAY_SECONDS, ROOM_SIZE, SIGNATURE_YELLOW,
        SKY_BOUNCE_BLUE, SceneBlueprint, SceneValidationError, TEAL_ACCENT,
        VERIFICATION_WINDOW_HEIGHT, VERIFICATION_WINDOW_WIDTH, VisualSpec, WORKER_BOOTS,
        WORKER_HARD_HAT, WORKER_HI_VIS, WORKER_SKIN, WORKER_SLATE, WORKER_TROUSERS,
    },
};
use sha2::{Digest, Sha256};

fn prop(id: &str) -> PropId {
    PropId::new(id)
}

fn sha256(path: impl AsRef<Path>) -> String {
    let mut file = File::open(path).expect("reference image should exist");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .expect("reference image should be readable");
    let mut digest = Sha256::new();
    digest.update(bytes);
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn validation_errors(mut mutate: impl FnMut(&mut SceneBlueprint)) -> Vec<SceneValidationError> {
    let mut scene = SceneBlueprint::v0();
    mutate(&mut scene);
    scene.validate()
}

#[test]
fn design_palette_maps_every_role_to_the_reviewed_srgba_constant() {
    let expected = [
        (PaletteRole::RackWhite, "#FBFCFD", RACK_WHITE),
        (PaletteRole::RackShadow, "#C6D5E0", RACK_SHADOW),
        (PaletteRole::FloorLight, "#DEE6EB", FLOOR_LIGHT),
        (PaletteRole::FloorShadow, "#B2C0CB", FLOOR_SHADOW),
        (PaletteRole::SignatureYellow, "#FFC93C", SIGNATURE_YELLOW),
        (PaletteRole::TealAccent, "#2FB8A8", TEAL_ACCENT),
        (PaletteRole::HoseCharcoal, "#2E353B", HOSE_CHARCOAL),
        (PaletteRole::Ink, "#1F2A33", INK),
        (PaletteRole::SkyBounceBlue, "#9FD0F0", SKY_BOUNCE_BLUE),
        (PaletteRole::HealthyGreen, "#4ADE80", HEALTHY_GREEN),
        (PaletteRole::FaultRed, "#FF4B4B", FAULT_RED),
        (PaletteRole::WorkerHiVis, "#C8D94A", WORKER_HI_VIS),
        (PaletteRole::WorkerSlate, "#55707F", WORKER_SLATE),
        (PaletteRole::WorkerTrousers, "#2F3A42", WORKER_TROUSERS),
        (PaletteRole::WorkerBoots, "#7A5233", WORKER_BOOTS),
        (PaletteRole::WorkerHardHat, "#2C6FB8", WORKER_HARD_HAT),
        (PaletteRole::WorkerSkin, "#C98F6A", WORKER_SKIN),
    ];

    assert_eq!(PaletteRole::ALL.len(), expected.len());
    for (index, (role, hex, color)) in expected.into_iter().enumerate() {
        assert_eq!(PaletteRole::ALL[index], role);
        assert_eq!(role.hex(), hex);
        assert_eq!(role.color(), color);
    }
}

#[test]
fn design_palette_constants_store_reviewed_channels_without_runtime_hex_parsing() {
    assert_eq!(RACK_WHITE, Srgba::rgb_u8(0xFB, 0xFC, 0xFD));
    assert_eq!(RACK_SHADOW, Srgba::rgb_u8(0xC6, 0xD5, 0xE0));
    assert_eq!(FLOOR_LIGHT, Srgba::rgb_u8(0xDE, 0xE6, 0xEB));
    assert_eq!(FLOOR_SHADOW, Srgba::rgb_u8(0xB2, 0xC0, 0xCB));
    assert_eq!(SIGNATURE_YELLOW, Srgba::rgb_u8(0xFF, 0xC9, 0x3C));
    assert_eq!(TEAL_ACCENT, Srgba::rgb_u8(0x2F, 0xB8, 0xA8));
    assert_eq!(HOSE_CHARCOAL, Srgba::rgb_u8(0x2E, 0x35, 0x3B));
    assert_eq!(INK, Srgba::rgb_u8(0x1F, 0x2A, 0x33));
    assert_eq!(SKY_BOUNCE_BLUE, Srgba::rgb_u8(0x9F, 0xD0, 0xF0));
    assert_eq!(HEALTHY_GREEN, Srgba::rgb_u8(0x4A, 0xDE, 0x80));
    assert_eq!(FAULT_RED, Srgba::rgb_u8(0xFF, 0x4B, 0x4B));
    assert_eq!(WORKER_HI_VIS, Srgba::rgb_u8(0xC8, 0xD9, 0x4A));
    assert_eq!(WORKER_SLATE, Srgba::rgb_u8(0x55, 0x70, 0x7F));
    assert_eq!(WORKER_TROUSERS, Srgba::rgb_u8(0x2F, 0x3A, 0x42));
    assert_eq!(WORKER_BOOTS, Srgba::rgb_u8(0x7A, 0x52, 0x33));
    assert_eq!(WORKER_HARD_HAT, Srgba::rgb_u8(0x2C, 0x6F, 0xB8));
    assert_eq!(WORKER_SKIN, Srgba::rgb_u8(0xC9, 0x8F, 0x6A));
}

#[test]
#[allow(clippy::excessive_precision)]
fn design_constants_match_the_reviewed_product_contract() {
    assert_eq!((DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT), (1280, 720));
    assert_eq!(
        (VERIFICATION_WINDOW_WIDTH, VERIFICATION_WINDOW_HEIGHT),
        (960, 540)
    );
    assert_eq!(ROOM_SIZE, Vec2::new(40.0, 40.0));
    assert_eq!((ORTHOGRAPHIC_WIDTH, ORTHOGRAPHIC_HEIGHT), (26.0, 14.625));
    assert_eq!(INITIAL_CAMERA_YAW_DEGREES, 45.0);
    assert_eq!(CAMERA_ELEVATION_DEGREES, 57.0);
    assert_eq!(CAMERA_OFFSET_DIRECTION, Vec3::new(1.0, 2.177_697_9, 1.0));
    assert_eq!(CAMERA_ORBIT_DURATION_SECONDS, 0.30);
    assert_eq!(FAULT_INTERVAL_SECONDS, 4.0);
    assert_eq!(MAX_ACTIVE_TICKETS, 3);
    assert_eq!(REPAIR_INTERACTION_RANGE, 1.5);
    assert_eq!(REPAIR_DURATION_SECONDS, 3.0);
    assert_eq!(RESOLVED_DISPLAY_SECONDS, 2.0);
    assert_eq!(RACK_COOLDOWN_SECONDS, 8.0);
}

#[test]
fn design_references_match_the_approved_pngs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let references = [
        (
            KEY_ART_REFERENCE_PATH,
            KEY_ART_SHA256,
            image::ImageFormat::Png,
        ),
        (
            CHARACTER_SHEET_REFERENCE_PATH,
            CHARACTER_SHEET_SHA256,
            image::ImageFormat::Png,
        ),
    ];

    for (relative_path, expected_hash, format) in references {
        let path = root.join(relative_path);
        assert_eq!(sha256(&path), expected_hash);
        let dimensions = image::ImageReader::with_format(
            std::io::BufReader::new(File::open(path).expect("reference should open")),
            format,
        )
        .into_dimensions()
        .expect("reference should be a valid PNG");
        assert_eq!(dimensions, (1536, 1024));
    }
}

#[test]
fn design_v0_blueprint_is_ordered_and_valid() {
    let scene = SceneBlueprint::v0();

    assert_eq!(scene.room.size, ROOM_SIZE);
    assert_eq!(scene.rack_row_count(), 4);
    assert_eq!(scene.aisles.len(), 3);
    assert_eq!(scene.player_spawn, Vec2::new(-6.0, -11.0));
    assert_eq!(
        scene
            .visuals
            .iter()
            .take(4)
            .map(|visual| visual.id.as_str())
            .collect::<Vec<_>>(),
        ["rack-row-01", "rack-row-02", "rack-row-03", "rack-row-04"]
    );
    assert_eq!(
        scene
            .colliders
            .iter()
            .take(4)
            .map(|collider| collider.id.as_str())
            .collect::<Vec<_>>(),
        ["rack-row-01", "rack-row-02", "rack-row-03", "rack-row-04"]
    );
    assert_eq!(scene.validate(), Vec::<SceneValidationError>::new());
}

#[test]
fn design_validator_reports_duplicate_visual_ids() {
    let errors = validation_errors(|scene| {
        let duplicate = scene
            .visuals
            .iter()
            .find(|visual| visual.id == prop("utility-cart"))
            .expect("fixture visual")
            .clone();
        scene.visuals.push(duplicate);
    });

    assert_eq!(
        errors,
        [SceneValidationError::DuplicateVisualId(prop(
            "utility-cart"
        ))]
    );
}

#[test]
fn design_validator_reports_duplicate_collider_ids() {
    let errors = validation_errors(|scene| {
        let duplicate = scene
            .colliders
            .iter()
            .find(|collider| collider.id == prop("utility-cart"))
            .expect("fixture collider")
            .clone();
        scene.colliders.push(duplicate);
    });

    assert_eq!(
        errors,
        [SceneValidationError::DuplicateColliderId(prop(
            "utility-cart"
        ))]
    );
}

#[test]
fn design_validator_reports_every_duplicate_in_one_pass() {
    let errors = validation_errors(|scene| {
        let visual = scene
            .visuals
            .iter()
            .find(|visual| visual.id == prop("utility-cart"))
            .expect("fixture visual")
            .clone();
        let collider = scene
            .colliders
            .iter()
            .find(|collider| collider.id == prop("step-stool"))
            .expect("fixture collider")
            .clone();
        scene.visuals.push(visual);
        scene.colliders.push(collider);
    });

    assert_eq!(
        errors,
        [
            SceneValidationError::DuplicateVisualId(prop("utility-cart")),
            SceneValidationError::DuplicateColliderId(prop("step-stool")),
        ]
    );
}

#[test]
fn design_validator_reports_missing_required_colliders() {
    let errors = validation_errors(|scene| {
        scene
            .colliders
            .retain(|collider| collider.id != prop("utility-cart"));
    });

    assert_eq!(
        errors,
        [SceneValidationError::MissingRequiredCollider(prop(
            "utility-cart"
        ))]
    );
}

#[test]
fn design_validator_reports_orphan_colliders() {
    let errors = validation_errors(|scene| {
        scene.colliders.push(ColliderSpec {
            id: prop("orphan"),
            center: Vec2::new(14.0, 14.0),
            half_extents: Vec2::splat(0.5),
        });
    });

    assert_eq!(
        errors,
        [SceneValidationError::OrphanCollider(prop("orphan"))]
    );
}

#[test]
fn design_validator_reports_colliders_outside_the_room() {
    let errors = validation_errors(|scene| {
        scene
            .colliders
            .iter_mut()
            .find(|collider| collider.id == prop("utility-cart"))
            .expect("fixture collider")
            .center = Vec2::new(20.0, 0.0);
    });

    assert_eq!(
        errors,
        [SceneValidationError::ColliderOutsideRoom(prop(
            "utility-cart"
        ))]
    );
}

#[test]
fn design_validator_reports_player_spawn_outside_the_room() {
    let errors = validation_errors(|scene| {
        scene.player_spawn = Vec2::new(21.0, 0.0);
    });

    assert_eq!(errors, [SceneValidationError::PlayerSpawnOutsideRoom]);
}

#[test]
fn design_validator_reports_player_spawn_inside_a_collider() {
    let errors = validation_errors(|scene| {
        scene.player_spawn = scene
            .colliders
            .iter()
            .find(|collider| collider.id == prop("rack-row-01"))
            .expect("fixture collider")
            .center;
    });

    assert_eq!(
        errors,
        [SceneValidationError::PlayerSpawnInsideCollider(prop(
            "rack-row-01"
        ))]
    );
}

#[test]
fn design_validator_reports_player_spawn_within_player_radius_of_a_collider() {
    let errors = validation_errors(|scene| {
        let collider = scene
            .colliders
            .iter()
            .find(|collider| collider.id == prop("utility-cart"))
            .expect("fixture collider");
        scene.player_spawn = Vec2::new(
            collider.center.x + collider.half_extents.x + PLAYER_RADIUS * 0.5,
            collider.center.y,
        );
    });

    assert_eq!(
        errors,
        [SceneValidationError::PlayerSpawnInsideCollider(prop(
            "utility-cart"
        ))]
    );
}

#[test]
fn design_validator_reports_wrong_rack_row_count() {
    let errors = validation_errors(|scene| {
        scene
            .visuals
            .retain(|visual| visual.id != prop("rack-row-04"));
        scene
            .colliders
            .retain(|collider| collider.id != prop("rack-row-04"));
    });

    assert_eq!(
        errors,
        [SceneValidationError::RackRowCount {
            expected: 4,
            actual: 3,
        }]
    );
}

#[test]
fn design_validator_reports_excess_rack_rows() {
    let errors = validation_errors(|scene| {
        scene.visuals.push(VisualSpec::new(
            "rack-row-05",
            AssetKind::RackRow,
            Vec3::new(15.0, 0.0, 0.0),
            false,
        ));
    });

    assert_eq!(
        errors,
        [SceneValidationError::RackRowCount {
            expected: 4,
            actual: 5,
        }]
    );
}

#[test]
fn design_validator_reports_wrong_aisle_count() {
    let errors = validation_errors(|scene| {
        scene.aisles.pop();
    });

    assert_eq!(
        errors,
        [SceneValidationError::AisleCount {
            expected: 3,
            actual: 2,
        }]
    );
}

#[test]
fn design_validator_reports_excess_aisles() {
    let errors = validation_errors(|scene| {
        scene.aisles.push(scene.aisles[2]);
    });

    assert_eq!(
        errors,
        [SceneValidationError::AisleCount {
            expected: 3,
            actual: 4,
        }]
    );
}

#[test]
fn design_validator_reports_blocked_aisle_topology() {
    let errors = validation_errors(|scene| {
        let aisle = scene.aisles[0];
        scene.visuals.push(VisualSpec::new(
            "aisle-blocker",
            AssetKind::UtilityCart,
            Vec3::new(aisle.center_x, 0.0, 0.0),
            true,
        ));
        scene.colliders.push(ColliderSpec {
            id: prop("aisle-blocker"),
            center: Vec2::new(aisle.center_x, 0.0),
            half_extents: Vec2::new(aisle.half_width, 0.5),
        });
    });

    assert_eq!(errors, [SceneValidationError::BlockedAisle { index: 0 }]);
}

#[test]
fn design_validator_reports_empty_camera_target_intervals() {
    let errors = validation_errors(|scene| {
        scene.room.size = Vec2::splat(30.0);
    });

    assert!(errors.contains(&SceneValidationError::EmptyCameraTargetInterval { yaw_degrees: 45 }));
    assert!(errors.contains(&SceneValidationError::EmptyCameraTargetInterval { yaw_degrees: 135 }));
    assert!(errors.contains(&SceneValidationError::EmptyCameraTargetInterval { yaw_degrees: 225 }));
    assert!(errors.contains(&SceneValidationError::EmptyCameraTargetInterval { yaw_degrees: 315 }));
}

#[test]
fn design_validator_checks_mid_orbit_camera_target_intervals() {
    let errors = validation_errors(|scene| {
        scene.room.size = Vec2::new(25.0, 40.0);
    });

    assert!(errors.contains(&SceneValidationError::EmptyCameraTargetInterval { yaw_degrees: 0 }));
    assert!(errors.contains(&SceneValidationError::EmptyCameraTargetInterval { yaw_degrees: 180 }));
}

#[derive(Resource, Default)]
struct SetTrace(Vec<CellShiftSet>);

macro_rules! trace_system {
    ($name:ident, $set:expr) => {
        fn $name(mut trace: ResMut<SetTrace>) {
            trace.0.push($set);
        }
    };
}

trace_system!(trace_asset_ready, CellShiftSet::AssetReady);
trace_system!(trace_spawn_world, CellShiftSet::SpawnWorld);
trace_system!(trace_read_input, CellShiftSet::ReadInput);
trace_system!(trace_orbit_intent, CellShiftSet::UpdateOrbitIntent);
trace_system!(trace_operations, CellShiftSet::UpdateOperations);
trace_system!(trace_move_player, CellShiftSet::MovePlayer);
trace_system!(trace_animation, CellShiftSet::UpdateAnimation);
trace_system!(trace_follow_camera, CellShiftSet::FollowCamera);
trace_system!(trace_hud, CellShiftSet::UpdateHudAndBadges);
trace_system!(trace_verification, CellShiftSet::VerificationProbe);

#[test]
fn design_plugin_configures_shared_system_sets_in_reviewed_order() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(CellShiftPlugin)
        .init_resource::<SetTrace>()
        .add_systems(
            Update,
            (
                trace_verification.in_set(CellShiftSet::VerificationProbe),
                trace_hud.in_set(CellShiftSet::UpdateHudAndBadges),
                trace_follow_camera.in_set(CellShiftSet::FollowCamera),
                trace_animation.in_set(CellShiftSet::UpdateAnimation),
                trace_move_player.in_set(CellShiftSet::MovePlayer),
                trace_operations.in_set(CellShiftSet::UpdateOperations),
                trace_orbit_intent.in_set(CellShiftSet::UpdateOrbitIntent),
                trace_read_input.in_set(CellShiftSet::ReadInput),
                trace_spawn_world.in_set(CellShiftSet::SpawnWorld),
                trace_asset_ready.in_set(CellShiftSet::AssetReady),
            ),
        );

    app.update();

    assert_eq!(
        app.world().resource::<SetTrace>().0,
        [
            CellShiftSet::AssetReady,
            CellShiftSet::SpawnWorld,
            CellShiftSet::ReadInput,
            CellShiftSet::UpdateOrbitIntent,
            CellShiftSet::UpdateOperations,
            CellShiftSet::MovePlayer,
            CellShiftSet::UpdateAnimation,
            CellShiftSet::FollowCamera,
            CellShiftSet::UpdateHudAndBadges,
            CellShiftSet::VerificationProbe,
        ]
    );
    assert_ne!(app.world().resource::<ClearColor>().0, Color::Srgba(BLACK));
}
