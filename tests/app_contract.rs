use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use bevy::{
    asset::{AssetPlugin as BevyAssetPlugin, RecursiveDependencyLoadState},
    camera::ScalingMode,
    color::palettes::css::BLACK,
    input::{
        ButtonState,
        keyboard::{Key, KeyboardInput, NativeKey},
    },
    prelude::*,
    render::{
        RenderPlugin,
        settings::{RenderCreation, WgpuSettings},
    },
    time::TimeUpdateStrategy,
};
use midcreek_cs_1::{
    CellShiftPlugin, CellShiftSet,
    assetgen::{ASSET_MODULES, ASSET_NAMES, TECHNICIAN_BONES, generate_glb, load_source},
    assets::{
        AssetLoadReport, AssetLoadState, GENERATED_ASSET_DIRECTORY, GeneratedAssets, RenderAssets,
        generated_modules, module_for,
    },
    camera::{
        CAMERA_DISTANCE, CameraHeading, CameraOrbit, CellShiftCamera, camera_target_bounds,
        clamp_follow_target, ground_half_depth, ground_quadrilateral,
    },
    design::{
        AISLE_CENTER_X, AISLE_CHECKPOINT_SPACING, AISLE_HALF_WIDTH, AISLE_Z_MAX, AISLE_Z_MIN,
        AssetKind, CAMERA_ELEVATION_DEGREES, CAMERA_OFFSET_DIRECTION,
        CAMERA_ORBIT_DURATION_SECONDS, CHARACTER_SHEET_REFERENCE_PATH, CHARACTER_SHEET_SHA256,
        ColliderSpec, DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, FAULT_INTERVAL_SECONDS,
        FAULT_RED, FLOOR_LIGHT, FLOOR_MARKING_HEIGHT, FLOOR_MARKING_WIDTH, FLOOR_SHADOW,
        HEALTHY_GREEN, HOSE_CHARCOAL, HOSE_DROP_HEIGHT, HOSE_DROP_Z, INITIAL_CAMERA_YAW_DEGREES,
        INK, KEY_ART_REFERENCE_PATH, KEY_ART_SHA256, MAX_ACTIVE_TICKETS, MIN_AISLE_CLEARANCE,
        ORTHOGRAPHIC_HEIGHT, ORTHOGRAPHIC_WIDTH, OVERHEAD_TRAY_HEIGHT, PLAYER_RADIUS, PaletteRole,
        PrimitiveShape, PropId, RACK_COOLDOWN_SECONDS, RACK_ROW_X, RACK_SHADOW, RACK_WHITE,
        REPAIR_DURATION_SECONDS, REPAIR_INTERACTION_RANGE, RESOLVED_DISPLAY_SECONDS, ROOM_SIZE,
        SIGNATURE_YELLOW, SKY_BOUNCE_BLUE, SceneBlueprint, SceneValidationError, TEAL_ACCENT,
        VERIFICATION_WINDOW_HEIGHT, VERIFICATION_WINDOW_WIDTH, VisualSpec, WALKABLE_CELL_SIZE,
        WALL_HEIGHT, WALL_THICKNESS, WORKER_BOOTS, WORKER_HARD_HAT, WORKER_HI_VIS, WORKER_SKIN,
        WORKER_SLATE, WORKER_TROUSERS,
    },
    player::{
        PLAYER_MAX_MOVE_DELTA, PLAYER_SPEED, PlayerAnimationState, PlayerAnimations, PlayerClip,
        PlayerMotion, PlayerParts, PlayerRigError, PlayerRigReport, PlayerRigState,
        TECHNICIAN_MODEL_FORWARD, Technician, ViewBasis, arrow_input, required_player_parts,
        update_player_animation,
    },
    world::{
        HallBlueprint, HallColliders, HallErrors, HallProp, HallRoot, HallState, PlayerSpawnPoint,
    },
};
use sha2::{Digest, Sha256};

fn prop(id: &str) -> PropId {
    PropId::new(id)
}

fn sha256(path: impl AsRef<Path>) -> String {
    let bytes = fs::read(path.as_ref()).expect("reference image should be readable");
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
            std::io::BufReader::new(fs::File::open(path).expect("reference should open")),
            format,
        )
        .into_dimensions()
        .expect("reference should be a valid PNG");
        assert_eq!(dimensions, (1536, 1024));
    }
}

#[test]
fn design_v0_blueprint_lists_the_authored_hall_in_reviewed_order() {
    let scene = SceneBlueprint::v0();

    assert_eq!(scene.room.size, ROOM_SIZE);
    assert_eq!(scene.player_spawn, Vec2::new(-6.0, -11.0));
    assert_eq!(
        scene
            .visuals
            .iter()
            .map(|visual| visual.id.as_str())
            .collect::<Vec<_>>(),
        [
            "floor",
            "wall-north",
            "wall-south",
            "wall-west",
            "wall-east",
            "rack-row-01",
            "rack-row-02",
            "rack-row-03",
            "rack-row-04",
            "cooling-unit-west-north",
            "cooling-unit-west-south",
            "cooling-unit-east-north",
            "cooling-unit-east-south",
            "overhead-tray-01",
            "overhead-tray-02",
            "overhead-tray-03",
            "hose-drop-01",
            "hose-drop-02",
            "hose-drop-03",
            "utility-cart",
            "step-stool",
            "floor-marking-aisle-01-west",
            "floor-marking-aisle-01-east",
            "floor-marking-aisle-02-west",
            "floor-marking-aisle-02-east",
            "floor-marking-aisle-03-west",
            "floor-marking-aisle-03-east",
            "floor-marking-walkway-north",
            "floor-marking-walkway-south",
        ]
    );
    assert_eq!(
        scene
            .colliders
            .iter()
            .map(|collider| collider.id.as_str())
            .collect::<Vec<_>>(),
        [
            "rack-row-01",
            "rack-row-02",
            "rack-row-03",
            "rack-row-04",
            "cooling-unit-west-north",
            "cooling-unit-west-south",
            "cooling-unit-east-north",
            "cooling-unit-east-south",
            "hose-drop-01",
            "hose-drop-02",
            "hose-drop-03",
            "utility-cart",
            "step-stool",
        ]
    );
    assert_eq!(scene.validate(), Vec::<SceneValidationError>::new());
}

#[test]
fn design_v0_blueprint_counts_every_authored_category() {
    let scene = SceneBlueprint::v0();

    assert_eq!(scene.rack_row_count(), 4);
    assert_eq!(scene.aisles.len(), 3);
    assert_eq!(scene.count_of(AssetKind::Floor), 1);
    assert_eq!(scene.count_of(AssetKind::Wall), 4);
    assert_eq!(scene.count_of(AssetKind::RackRow), 4);
    assert_eq!(scene.count_of(AssetKind::CoolingUnit), 4);
    assert_eq!(scene.count_of(AssetKind::OverheadTray), 3);
    assert_eq!(scene.count_of(AssetKind::HoseDrop), 3);
    assert_eq!(scene.count_of(AssetKind::UtilityCart), 1);
    assert_eq!(scene.count_of(AssetKind::StepStool), 1);
    assert_eq!(scene.count_of(AssetKind::FloorMarking), 8);
    assert_eq!(scene.visuals.len(), 29);
    assert_eq!(scene.colliders.len(), 13);
}

#[test]
fn design_v0_blueprint_places_the_room_shell_and_equipment_exactly() {
    let scene = SceneBlueprint::v0();
    let visual = |id: &str| scene.visual(id).expect("authored visual").clone();

    let floor = visual("floor");
    assert_eq!(floor.asset, AssetKind::Floor);
    assert_eq!(floor.transform.translation, Vec3::ZERO);
    assert_eq!(floor.transform.scale, Vec3::new(40.0, 1.0, 40.0));
    assert!(!floor.collision_required);

    let half_room = ROOM_SIZE.x * 0.5;
    let wall_offset = half_room + WALL_THICKNESS * 0.5;
    let wall_span = ROOM_SIZE.x + WALL_THICKNESS * 2.0;
    for (id, translation, scale) in [
        (
            "wall-north",
            Vec3::new(0.0, WALL_HEIGHT * 0.5, -wall_offset),
            Vec3::new(wall_span, WALL_HEIGHT, WALL_THICKNESS),
        ),
        (
            "wall-south",
            Vec3::new(0.0, WALL_HEIGHT * 0.5, wall_offset),
            Vec3::new(wall_span, WALL_HEIGHT, WALL_THICKNESS),
        ),
        (
            "wall-west",
            Vec3::new(-wall_offset, WALL_HEIGHT * 0.5, 0.0),
            Vec3::new(WALL_THICKNESS, WALL_HEIGHT, wall_span),
        ),
        (
            "wall-east",
            Vec3::new(wall_offset, WALL_HEIGHT * 0.5, 0.0),
            Vec3::new(WALL_THICKNESS, WALL_HEIGHT, wall_span),
        ),
    ] {
        let wall = visual(id);
        assert_eq!(wall.asset, AssetKind::Wall);
        assert_eq!(wall.transform.translation, translation);
        assert_eq!(wall.transform.scale, scale);
        assert!(!wall.collision_required);
    }

    for (index, x) in RACK_ROW_X.into_iter().enumerate() {
        let id = format!("rack-row-{:02}", index + 1);
        let rack = visual(&id);
        assert_eq!(rack.asset, AssetKind::RackRow);
        assert_eq!(rack.transform.translation, Vec3::new(x, 0.0, 0.0));
        assert_eq!(rack.transform.scale, Vec3::ONE);
        assert!(rack.collision_required);
        let collider = scene.collider(&id).expect("rack collider");
        assert_eq!(collider.center, Vec2::new(x, 0.0));
        assert_eq!(collider.half_extents, Vec2::new(0.8, 8.05));
    }

    for (index, x) in AISLE_CENTER_X.into_iter().enumerate() {
        let tray = visual(&format!("overhead-tray-{:02}", index + 1));
        assert_eq!(tray.asset, AssetKind::OverheadTray);
        assert_eq!(
            tray.transform.translation,
            Vec3::new(x, OVERHEAD_TRAY_HEIGHT, 0.0)
        );
        assert!(!tray.collision_required);

        let hose_id = format!("hose-drop-{:02}", index + 1);
        let hose = visual(&hose_id);
        assert_eq!(hose.asset, AssetKind::HoseDrop);
        assert_eq!(
            hose.transform.translation,
            Vec3::new(x, HOSE_DROP_HEIGHT, 7.0)
        );
        assert!(hose.collision_required);
        assert_eq!(
            scene
                .collider(&hose_id)
                .expect("hose collider")
                .half_extents,
            Vec2::splat(0.2)
        );

        let aisle = scene.aisles[index];
        assert_eq!(aisle.center_x, x);
        assert_eq!(aisle.half_width, AISLE_HALF_WIDTH);
        assert_eq!((aisle.z_min, aisle.z_max), (AISLE_Z_MIN, AISLE_Z_MAX));

        for (side, offset) in [("west", -AISLE_HALF_WIDTH), ("east", AISLE_HALF_WIDTH)] {
            let marking = visual(&format!("floor-marking-aisle-{:02}-{side}", index + 1));
            assert_eq!(marking.asset, AssetKind::FloorMarking);
            assert_eq!(
                marking.transform.translation,
                Vec3::new(x + offset, FLOOR_MARKING_HEIGHT, 0.0)
            );
            assert_eq!(
                marking.transform.scale,
                Vec3::new(FLOOR_MARKING_WIDTH, 1.0, AISLE_Z_MAX - AISLE_Z_MIN)
            );
        }
    }

    for (id, center) in [
        ("cooling-unit-west-north", Vec2::new(-13.0, -6.0)),
        ("cooling-unit-west-south", Vec2::new(-13.0, 6.0)),
        ("cooling-unit-east-north", Vec2::new(13.0, -6.0)),
        ("cooling-unit-east-south", Vec2::new(13.0, 6.0)),
    ] {
        let unit = visual(id);
        assert_eq!(unit.asset, AssetKind::CoolingUnit);
        assert_eq!(
            unit.transform.translation,
            Vec3::new(center.x, 0.0, center.y)
        );
        let collider = scene.collider(id).expect("cooling collider");
        assert_eq!(collider.center, center);
        assert_eq!(collider.half_extents, Vec2::new(1.05, 2.05));
    }

    assert_eq!(
        visual("utility-cart").transform.translation,
        Vec3::new(-13.0, 0.0, -10.0)
    );
    assert_eq!(
        visual("step-stool").transform.translation,
        Vec3::new(13.0, 0.0, 10.0)
    );
    for (id, translation) in [
        (
            "floor-marking-walkway-north",
            Vec3::new(0.0, FLOOR_MARKING_HEIGHT, -14.0),
        ),
        (
            "floor-marking-walkway-south",
            Vec3::new(0.0, FLOOR_MARKING_HEIGHT, 14.0),
        ),
    ] {
        let marking = visual(id);
        assert_eq!(marking.transform.translation, translation);
        assert_eq!(
            marking.transform.scale,
            Vec3::new(32.0, 1.0, FLOOR_MARKING_WIDTH)
        );
    }
}

#[test]
fn design_every_asset_kind_is_either_a_generated_module_or_a_unit_primitive() {
    assert_eq!(AssetKind::ALL.len(), 9);
    for kind in AssetKind::ALL {
        let primitive = kind.primitive();
        let module = module_for(kind);
        assert_ne!(
            primitive.is_some(),
            module.is_some(),
            "{kind:?} must be exactly one of a unit primitive or a generated module"
        );
    }

    assert_eq!(
        AssetKind::Floor.primitive(),
        Some((PrimitiveShape::Quad, PaletteRole::FloorLight))
    );
    assert_eq!(
        AssetKind::Wall.primitive(),
        Some((PrimitiveShape::Cuboid, PaletteRole::FloorShadow))
    );
    assert_eq!(
        AssetKind::FloorMarking.primitive(),
        Some((PrimitiveShape::Quad, PaletteRole::SignatureYellow))
    );
    assert_eq!(PrimitiveShape::ALL.len(), 2);
}

#[test]
fn design_generated_modules_cover_every_declared_asset_scene() {
    let modules = generated_modules();
    let declared = ASSET_MODULES
        .into_iter()
        .flat_map(|(asset, modules)| {
            modules
                .iter()
                .enumerate()
                .map(move |(index, module)| (asset.to_owned(), (*module).to_owned(), index))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        modules
            .iter()
            .map(|module| (
                module.asset.to_owned(),
                module.module.to_owned(),
                module.scene_index
            ))
            .collect::<Vec<_>>(),
        declared
    );
    for module in &modules {
        assert_eq!(
            module.path(),
            format!("{GENERATED_ASSET_DIRECTORY}/{}.glb", module.asset)
        );
        assert_eq!(
            module.scene_path(),
            format!(
                "{GENERATED_ASSET_DIRECTORY}/{}.glb#Scene{}",
                module.asset, module.scene_index
            )
        );
    }

    for kind in AssetKind::ALL {
        if let Some(module) = module_for(kind) {
            assert!(
                modules.contains(&module),
                "{kind:?} maps outside the pipeline"
            );
        }
    }
}

#[test]
fn design_aisle_checkpoints_sample_every_aisle_centreline() {
    let scene = SceneBlueprint::v0();
    let checkpoints = scene.aisle_checkpoints();
    let per_aisle = ((AISLE_Z_MAX - AISLE_Z_MIN) / AISLE_CHECKPOINT_SPACING) as usize + 1;

    assert_eq!(checkpoints.len(), per_aisle * scene.aisles.len());
    for (aisle_index, aisle) in scene.aisles.iter().enumerate() {
        let aisle_points = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.aisle == aisle_index)
            .collect::<Vec<_>>();
        assert_eq!(aisle_points.len(), per_aisle);
        assert_eq!(
            aisle_points[0].point,
            Vec2::new(aisle.center_x, aisle.z_min)
        );
        assert_eq!(
            aisle_points[per_aisle - 1].point,
            Vec2::new(aisle.center_x, aisle.z_max)
        );
        for point in &aisle_points {
            assert_eq!(point.point.x, aisle.center_x);
        }
    }
}

#[test]
fn design_flood_fill_joins_every_aisle_checkpoint_to_the_player_spawn() {
    let scene = SceneBlueprint::v0();
    let report = scene.walkable_report();

    assert_eq!(report.cell_size, WALKABLE_CELL_SIZE);
    assert!(report.unreachable.is_empty(), "{:?}", report.unreachable);
    assert!(report.is_connected());
    assert!(
        report.reachable_cells > report.total_cells / 2,
        "expected a mostly open hall, got {}/{}",
        report.reachable_cells,
        report.total_cells
    );
    assert_eq!(
        report.narrowest_aisle_clearance, MIN_AISLE_CLEARANCE,
        "the authored hose pinch is the narrowest point of any aisle"
    );
    assert_eq!(report.aisle_clearances, vec![MIN_AISLE_CLEARANCE; 3]);
    assert!(report.narrowest_aisle_clearance >= MIN_AISLE_CLEARANCE);
}

#[test]
fn design_clearance_scans_every_aisle_row_and_measures_the_authored_hose_pinch() {
    let scene = SceneBlueprint::v0();
    let grid = scene.walkable_grid();

    // The authored pinch sits between two checkpoints, so a checkpoint-only
    // sample can never see it.
    assert_ne!(HOSE_DROP_Z % AISLE_CHECKPOINT_SPACING, 0.0);
    assert!(
        scene
            .aisle_checkpoints()
            .iter()
            .all(|checkpoint| checkpoint.point.y != HOSE_DROP_Z)
    );

    for aisle in &scene.aisles {
        assert_eq!(
            grid.widest_open_run(0.0, aisle.center_x, aisle.half_width),
            AISLE_HALF_WIDTH * 2.0,
            "an unobstructed aisle row spans the full authored corridor"
        );
        assert_eq!(
            grid.widest_open_run(HOSE_DROP_Z, aisle.center_x, aisle.half_width),
            MIN_AISLE_CLEARANCE,
            "the hose drop pinches its aisle to the minimum walkable run"
        );

        let rows = ((aisle.z_max - aisle.z_min) / WALKABLE_CELL_SIZE).round() as i32;
        let scanned = (0..=rows)
            .map(|row| {
                grid.widest_open_run(
                    aisle.z_min + row as f32 * WALKABLE_CELL_SIZE,
                    aisle.center_x,
                    aisle.half_width,
                )
            })
            .fold(f32::INFINITY, f32::min);
        assert_eq!(rows, 96);
        assert_eq!(
            grid.aisle_clearance(aisle),
            scanned,
            "aisle clearance must be the minimum over every grid row of the aisle"
        );
        assert_eq!(grid.aisle_clearance(aisle), MIN_AISLE_CLEARANCE);
    }
}

#[test]
fn design_clearance_measures_center_space_runs_without_double_counting_the_player() {
    // The grid is already inflated by PLAYER_RADIUS, so an open node is a
    // standing position and a run of N adjacent nodes spans (N - 1) cells.
    let mut scene = SceneBlueprint::v0();
    scene.colliders.clear();
    scene.visuals.retain(|visual| !visual.collision_required);

    let grid = scene.walkable_grid();
    for cells in 0..=5u32 {
        let half_width = cells as f32 * WALKABLE_CELL_SIZE;
        let nodes = 2 * cells + 1;
        assert_eq!(
            grid.widest_open_run(0.0, 0.0, half_width),
            (nodes as f32 - 1.0) * WALKABLE_CELL_SIZE,
            "a run of {nodes} open nodes spans (n - 1) cells of centre space"
        );
    }
    assert_eq!(grid.widest_open_run(0.0, 0.0, 0.0), 0.0);
}

#[test]
fn design_flood_fill_reports_checkpoints_cut_off_from_the_player_spawn() {
    // A thin cross-hall barrier at an odd z, between two checkpoint rows. Every
    // checkpoint node stays open, so only real traversal can tell the two sides
    // of the hall apart.
    let mut scene = SceneBlueprint::v0();
    scene.visuals.push(VisualSpec::new(
        "cross-aisle-barrier",
        AssetKind::UtilityCart,
        Vec3::new(0.0, 0.0, -9.0),
        true,
    ));
    scene.colliders.push(ColliderSpec {
        id: prop("cross-aisle-barrier"),
        center: Vec2::new(0.0, -9.0),
        half_extents: Vec2::new(ROOM_SIZE.x * 0.5, 0.3),
    });

    let grid = scene.walkable_grid();
    let report = scene.walkable_report();
    assert!(!report.is_connected());

    for checkpoint in &report.unreachable {
        assert!(
            grid.is_open(checkpoint.point),
            "{checkpoint:?} must stay an open standing position, only disconnected"
        );
    }
    assert!(
        report
            .unreachable
            .iter()
            .all(|checkpoint| checkpoint.point.y > -9.0),
        "{:?}",
        report.unreachable
    );
    assert_eq!(
        report
            .unreachable
            .iter()
            .filter(|checkpoint| checkpoint.aisle == 0)
            .map(|checkpoint| checkpoint.point.y)
            .collect::<Vec<_>>(),
        [-8.0, -6.0, -4.0, -2.0, 0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0]
    );
    assert_eq!(report.unreachable.len(), 33);
    for checkpoint in scene.aisle_checkpoints() {
        assert!(
            grid.is_open(checkpoint.point),
            "{checkpoint:?} must stay open on both sides of the barrier"
        );
    }

    assert_eq!(
        scene.validate(),
        [
            SceneValidationError::BlockedAisle { index: 0 },
            SceneValidationError::BlockedAisle { index: 1 },
            SceneValidationError::BlockedAisle { index: 2 },
            SceneValidationError::InsufficientAisleClearance { index: 0 },
            SceneValidationError::InsufficientAisleClearance { index: 1 },
            SceneValidationError::InsufficientAisleClearance { index: 2 },
        ]
    );
}

#[test]
fn design_validator_reports_insufficient_aisle_clearance() {
    // A pinch at an odd z, off every checkpoint row, that narrows aisle 1 to a
    // single cell without ever disconnecting it.
    let mut scene = SceneBlueprint::v0();
    scene.visuals.push(VisualSpec::new(
        "aisle-pinch",
        AssetKind::UtilityCart,
        Vec3::new(AISLE_CENTER_X[1] + 0.35, 0.0, 3.0),
        true,
    ));
    scene.colliders.push(ColliderSpec {
        id: prop("aisle-pinch"),
        center: Vec2::new(AISLE_CENTER_X[1] + 0.35, 3.0),
        half_extents: Vec2::new(0.9, 0.2),
    });

    let report = scene.walkable_report();
    assert!(
        report.is_connected(),
        "the pinch must narrow the aisle, not block it: {:?}",
        report.unreachable
    );
    assert_eq!(report.aisle_clearances[1], WALKABLE_CELL_SIZE);
    assert_eq!(report.narrowest_aisle_clearance, WALKABLE_CELL_SIZE);
    assert_eq!(
        scene.validate(),
        [SceneValidationError::InsufficientAisleClearance { index: 1 }]
    );
}

#[test]
fn design_validator_accepts_the_authored_pinch_and_rejects_one_cell_more() {
    assert_eq!(
        SceneBlueprint::v0().validate(),
        Vec::<SceneValidationError>::new()
    );
    assert_eq!(MIN_AISLE_CLEARANCE, 0.5);

    // Growing every hose drop by one grid cell in x takes the authored pinch
    // below the threshold, so the gate is one cell away from biting.
    let errors = validation_errors(|scene| {
        for collider in &mut scene.colliders {
            if collider.id.as_str().starts_with("hose-drop") {
                collider.half_extents.x += WALKABLE_CELL_SIZE;
            }
        }
    });
    assert_eq!(
        errors,
        [
            SceneValidationError::InsufficientAisleClearance { index: 0 },
            SceneValidationError::InsufficientAisleClearance { index: 1 },
            SceneValidationError::InsufficientAisleClearance { index: 2 },
        ]
    );
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

    assert_eq!(
        errors,
        [
            SceneValidationError::BlockedAisle { index: 0 },
            SceneValidationError::InsufficientAisleClearance { index: 0 },
        ]
    );
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

// ---------------------------------------------------------------------------
// Real Bevy app harness
// ---------------------------------------------------------------------------

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TempAssets(PathBuf);

impl TempAssets {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempAssets {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn repo_assets() -> PathBuf {
    repo_root().join("assets")
}

/// Copies every committed generated asset into a private directory so a test
/// can corrupt or remove exactly one file without touching the repository.
fn temp_assets(label: &str, mutate: impl FnOnce(&Path)) -> TempAssets {
    let root = std::env::temp_dir().join(format!(
        "midcreek-hall-{label}-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let generated = root.join(GENERATED_ASSET_DIRECTORY);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&generated).expect("temp asset root should be creatable");
    for name in ASSET_NAMES {
        let file = format!("{name}.glb");
        fs::copy(
            repo_assets().join(GENERATED_ASSET_DIRECTORY).join(&file),
            generated.join(&file),
        )
        .expect("committed asset should be copyable");
    }
    mutate(&generated);
    TempAssets(root)
}

fn hall_app(assets: &Path) -> App {
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::log::LogPlugin>()
            .disable::<bevy::winit::WinitPlugin>()
            .disable::<bevy::app::PanicHandlerPlugin>()
            .disable::<bevy::app::TerminalCtrlCHandlerPlugin>()
            .set(BevyAssetPlugin {
                file_path: assets.to_string_lossy().into_owned(),
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                    backends: None,
                    ..default()
                })),
                ..default()
            }),
    )
    .add_plugins(CellShiftPlugin);
    app.finish();
    app.cleanup();
    app
}

fn settle_assets(app: &mut App) -> AssetLoadState {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        app.update();
        let state = *app.world().resource::<State<AssetLoadState>>().get();
        if state != AssetLoadState::Loading {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "generated assets never left AssetLoadState::Loading"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn pump(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

fn built_hall(assets: &Path) -> App {
    let mut app = hall_app(assets);
    assert_eq!(settle_assets(&mut app), AssetLoadState::Ready);
    pump(&mut app, 4);
    assert_eq!(
        *app.world().resource::<State<HallState>>().get(),
        HallState::Ready
    );
    app
}

fn props(app: &mut App) -> Vec<(String, AssetKind, Transform)> {
    app.world_mut()
        .query::<(&HallProp, &Transform)>()
        .iter(app.world())
        .map(|(prop, transform)| (prop.id.as_str().to_owned(), prop.asset, *transform))
        .collect()
}

// ---------------------------------------------------------------------------
// Asset loading contracts
// ---------------------------------------------------------------------------

#[test]
fn hall_asset_plugin_becomes_ready_only_after_every_generated_module_loads() {
    let mut app = hall_app(&repo_assets());
    assert_eq!(
        *app.world().resource::<State<AssetLoadState>>().get(),
        AssetLoadState::Loading
    );

    assert_eq!(settle_assets(&mut app), AssetLoadState::Ready);
    assert!(
        app.world()
            .resource::<AssetLoadReport>()
            .failures()
            .is_empty()
    );

    let server = app.world().resource::<AssetServer>().clone();
    let generated = app.world().resource::<GeneratedAssets>();
    assert_eq!(generated.documents().len(), ASSET_NAMES.len());
    assert_eq!(generated.scenes().len(), generated_modules().len());
    for handle in generated.handle_ids() {
        assert!(
            matches!(
                server.get_recursive_dependency_load_state(handle),
                Some(RecursiveDependencyLoadState::Loaded)
            ),
            "every tracked handle must be loaded before Ready"
        );
    }

    let gltfs = app.world().resource::<Assets<Gltf>>();
    for (asset, modules) in ASSET_MODULES {
        let document = generated
            .document(asset)
            .and_then(|handle| gltfs.get(handle))
            .unwrap_or_else(|| panic!("{asset} document should be loaded"));
        assert_eq!(document.scenes.len(), modules.len());
        for (scene_index, module) in modules.iter().enumerate() {
            let named = document
                .named_scenes
                .get(*module)
                .unwrap_or_else(|| panic!("{asset} must expose the {module} scene"));
            assert_eq!(
                named.id(),
                document.scenes[scene_index].id(),
                "{asset} must bind {module} to scene {scene_index}"
            );
            assert_eq!(
                generated
                    .scene(asset, module)
                    .unwrap_or_else(|| panic!("{asset} must track the {module} scene"))
                    .id(),
                named.id(),
                "the handle the hall spawns must be the named module scene"
            );
        }
    }
    for kind in AssetKind::ALL {
        assert_eq!(
            generated.module_scene(kind).is_some(),
            module_for(kind).is_some()
        );
    }
}

#[test]
fn hall_asset_plugin_fails_loudly_when_a_generated_asset_is_missing() {
    let assets = temp_assets("missing", |generated| {
        fs::remove_file(generated.join("rack.glb")).expect("fixture asset should be removable");
    });
    let mut app = hall_app(assets.path());

    assert_eq!(settle_assets(&mut app), AssetLoadState::Failed);
    let failures = app
        .world()
        .resource::<AssetLoadReport>()
        .failures()
        .to_vec();
    assert!(
        failures.iter().any(|failure| failure.contains("rack.glb")),
        "failure report must name the missing file, got {failures:?}"
    );
    assert_eq!(
        *app.world().resource::<State<HallState>>().get(),
        HallState::Unbuilt
    );

    pump(&mut app, 4);
    assert!(
        props(&mut app).is_empty(),
        "a failed asset load must never fall back to procedural props"
    );
}

#[test]
fn hall_asset_plugin_fails_loudly_when_a_generated_asset_is_corrupt() {
    let assets = temp_assets("corrupt", |generated| {
        let path = generated.join("utility-props.glb");
        let mut bytes = fs::read(&path).expect("fixture asset should be readable");
        bytes.truncate(bytes.len() / 2);
        fs::write(&path, bytes).expect("fixture asset should be writable");
    });
    let mut app = hall_app(assets.path());

    assert_eq!(settle_assets(&mut app), AssetLoadState::Failed);
    let failures = app
        .world()
        .resource::<AssetLoadReport>()
        .failures()
        .to_vec();
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("utility-props.glb")),
        "failure report must name the corrupt file, got {failures:?}"
    );
    assert!(props(&mut app).is_empty());
}

#[test]
fn hall_asset_plugin_fails_loudly_when_a_module_scene_name_is_wrong() {
    let assets = temp_assets("mislabelled", |generated| {
        let cooling = fs::read(generated.join("cooling-unit.glb")).expect("fixture readable");
        fs::write(generated.join("rack.glb"), cooling).expect("fixture writable");
    });
    let mut app = hall_app(assets.path());

    assert_eq!(settle_assets(&mut app), AssetLoadState::Failed);
    let failures = app
        .world()
        .resource::<AssetLoadReport>()
        .failures()
        .to_vec();
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("rack") && failure.contains("rack-row")),
        "failure report must name the missing module scene, got {failures:?}"
    );
    assert!(props(&mut app).is_empty());
}

#[test]
fn hall_asset_plugin_fails_loudly_when_a_module_scene_binding_is_swapped() {
    // A private multi-scene fixture whose two scene names are swapped: both
    // declared module names are still present, but scene 0 is now named
    // `hose-drop` and scene 1 `overhead-tray`, so the indexed handle the hall
    // spawns is no longer the module it claims to be.
    let assets = temp_assets("swapped", |generated| {
        let mut source = load_source(&repo_root(), "infrastructure").expect("fixture source");
        assert_eq!(source.modules.len(), 2);
        let first = source.modules[0].name.clone();
        source.modules[0].name = source.modules[1].name.clone();
        source.modules[1].name = first;
        assert_eq!(source.modules[0].name, "hose-drop");
        assert_eq!(source.modules[1].name, "overhead-tray");

        let bytes = generate_glb(&source, "swapped-infrastructure").expect("fixture glb");
        fs::write(generated.join("infrastructure.glb"), bytes).expect("fixture writable");
    });
    let mut app = hall_app(assets.path());

    assert_eq!(settle_assets(&mut app), AssetLoadState::Failed);
    let failures = app
        .world()
        .resource::<AssetLoadReport>()
        .failures()
        .to_vec();
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("infrastructure.glb") && failure.contains("Scene0")),
        "failure report must name the mismatched scene binding, got {failures:?}"
    );

    pump(&mut app, 4);
    assert!(
        props(&mut app).is_empty(),
        "a mismatched scene binding must never spawn hall props"
    );
    assert_eq!(
        *app.world().resource::<State<HallState>>().get(),
        HallState::Unbuilt
    );
}

#[test]
fn hall_render_assets_cache_one_unit_mesh_per_shape_and_one_material_per_role() {
    let mut app = hall_app(&repo_assets());
    app.update();

    let render = app.world().resource::<RenderAssets>();
    let meshes = PrimitiveShape::ALL
        .into_iter()
        .map(|shape| render.mesh(shape).id())
        .collect::<Vec<_>>();
    let materials = PaletteRole::ALL
        .into_iter()
        .map(|role| render.material(role).id())
        .collect::<Vec<_>>();

    let mut unique_meshes = meshes.clone();
    unique_meshes.sort();
    unique_meshes.dedup();
    let mut unique_materials = materials.clone();
    unique_materials.sort();
    unique_materials.dedup();
    assert_eq!(unique_meshes.len(), PrimitiveShape::ALL.len());
    assert_eq!(unique_materials.len(), PaletteRole::ALL.len());

    for shape in PrimitiveShape::ALL {
        assert_eq!(render.mesh(shape).id(), render.mesh(shape).id());
    }
    let stored = app.world().resource::<Assets<StandardMaterial>>();
    for role in PaletteRole::ALL {
        let material = stored
            .get(&render.material(role))
            .expect("every palette role must have one cached material");
        assert_eq!(material.base_color, Color::Srgba(role.color()));
        assert!(material.unlit, "cel shift materials must be unlit");
    }
}

// ---------------------------------------------------------------------------
// Hall spawning contracts
// ---------------------------------------------------------------------------

#[test]
fn hall_spawns_no_props_until_every_asset_handle_is_loaded() {
    let mut app = hall_app(&repo_assets());
    app.update();

    assert_eq!(
        *app.world().resource::<State<AssetLoadState>>().get(),
        AssetLoadState::Loading
    );
    assert_eq!(
        *app.world().resource::<State<HallState>>().get(),
        HallState::Unbuilt
    );
    assert!(props(&mut app).is_empty());

    assert_eq!(settle_assets(&mut app), AssetLoadState::Ready);
    pump(&mut app, 4);
    assert_eq!(
        *app.world().resource::<State<HallState>>().get(),
        HallState::Ready
    );
    assert!(!props(&mut app).is_empty());
}

#[test]
fn hall_spawns_every_authored_visual_once_with_the_reviewed_transform() {
    let mut app = built_hall(&repo_assets());
    let scene = SceneBlueprint::v0();
    let spawned = props(&mut app);

    assert_eq!(spawned.len(), scene.visuals.len());
    assert_eq!(
        spawned
            .iter()
            .map(|(id, _, _)| id.as_str())
            .collect::<Vec<_>>()
            .len(),
        scene.visuals.len()
    );
    for visual in &scene.visuals {
        let matches = spawned
            .iter()
            .filter(|(id, _, _)| id == visual.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{} must spawn exactly once", visual.id);
        let (_, kind, transform) = matches[0];
        assert_eq!(*kind, visual.asset);
        assert_eq!(transform.translation, visual.transform.translation);
        assert_eq!(transform.scale, visual.transform.scale);
        assert_eq!(
            transform.rotation,
            Quat::from_rotation_y(visual.transform.rotation_y_degrees.to_radians())
        );
    }

    let mut roots = app.world_mut().query::<&HallRoot>();
    assert_eq!(roots.iter(app.world()).count(), 1);
}

#[test]
fn hall_spawns_unit_primitives_with_cached_meshes_and_palette_materials() {
    let mut app = built_hall(&repo_assets());
    let render = app.world().resource::<RenderAssets>().clone();
    let scene = SceneBlueprint::v0();

    let spawned = app
        .world_mut()
        .query::<(&HallProp, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>()
        .iter(app.world())
        .map(|(prop, mesh, material)| (prop.id.as_str().to_owned(), mesh.0.id(), material.0.id()))
        .collect::<Vec<_>>();

    let primitives = scene
        .visuals
        .iter()
        .filter(|visual| visual.asset.primitive().is_some())
        .collect::<Vec<_>>();
    assert_eq!(spawned.len(), primitives.len());
    assert_eq!(primitives.len(), 13);

    for visual in primitives {
        let (shape, role) = visual.asset.primitive().expect("primitive kind");
        let (_, mesh, material) = spawned
            .iter()
            .find(|(id, _, _)| id == visual.id.as_str())
            .unwrap_or_else(|| panic!("{} should spawn a primitive", visual.id));
        assert_eq!(*mesh, render.mesh(shape).id());
        assert_eq!(*material, render.material(role).id());
    }
}

#[test]
fn hall_spawns_generated_modules_as_shared_scene_roots_without_new_materials() {
    let mut app = built_hall(&repo_assets());
    let generated = app.world().resource::<GeneratedAssets>().clone();
    let scene = SceneBlueprint::v0();
    let material_count = app.world().resource::<Assets<StandardMaterial>>().len();

    let spawned = app
        .world_mut()
        .query::<(&HallProp, &WorldAssetRoot)>()
        .iter(app.world())
        .map(|(prop, root)| (prop.id.as_str().to_owned(), prop.asset, root.0.id()))
        .collect::<Vec<_>>();

    let modules = scene
        .visuals
        .iter()
        .filter(|visual| module_for(visual.asset).is_some())
        .collect::<Vec<_>>();
    assert_eq!(spawned.len(), modules.len());
    assert_eq!(modules.len(), 16);

    for visual in &modules {
        let (_, kind, handle) = spawned
            .iter()
            .find(|(id, _, _)| id == visual.id.as_str())
            .unwrap_or_else(|| panic!("{} should spawn a generated module", visual.id));
        assert_eq!(*kind, visual.asset);
        assert_eq!(
            *handle,
            generated
                .module_scene(visual.asset)
                .expect("loaded module handle")
                .id(),
            "{} must reuse the one cached scene handle for {:?}",
            visual.id,
            visual.asset
        );
    }

    pump(&mut app, 4);
    assert_eq!(
        app.world().resource::<Assets<StandardMaterial>>().len(),
        material_count,
        "spawning the hall must not create additional materials"
    );

    let mut meshes = app.world_mut().query::<&Mesh3d>();
    assert!(
        meshes.iter(app.world()).count() > modules.len(),
        "generated module scenes must actually instantiate their merged meshes"
    );
}

#[test]
fn hall_caches_collider_rectangles_once_and_joins_them_to_visual_ids() {
    let app = built_hall(&repo_assets());
    let scene = SceneBlueprint::v0();
    let colliders = app.world().resource::<HallColliders>();

    assert_eq!(colliders.all(), scene.colliders.as_slice());
    assert_eq!(
        app.world().resource::<PlayerSpawnPoint>().0,
        scene.player_spawn
    );
    assert!(app.world().resource::<HallErrors>().errors().is_empty());

    for collider in scene.colliders.iter() {
        assert!(
            scene
                .visual(collider.id.as_str())
                .is_some_and(|visual| visual.collision_required),
            "{} must join a collision-required visual",
            collider.id
        );
        assert_eq!(
            colliders.get(&collider.id).map(|found| found.center),
            Some(collider.center)
        );
        assert!(colliders.overlaps(collider.center, 0.0));
    }

    assert_eq!(
        colliders
            .first_overlap(Vec2::new(RACK_ROW_X[0], 0.0), PLAYER_RADIUS)
            .map(|collider| collider.id.as_str().to_owned()),
        Some("rack-row-01".to_owned())
    );
    assert!(
        colliders
            .first_overlap(scene.player_spawn, PLAYER_RADIUS)
            .is_none()
    );
    for checkpoint in scene.aisle_checkpoints() {
        assert!(
            colliders
                .first_overlap(checkpoint.point, PLAYER_RADIUS)
                .is_none(),
            "aisle checkpoint {:?} must stay walkable",
            checkpoint.point
        );
    }
}

#[test]
fn hall_refuses_to_spawn_an_invalid_blueprint() {
    let mut app = hall_app(&repo_assets());
    let mut broken = SceneBlueprint::v0();
    broken
        .colliders
        .retain(|collider| collider.id != prop("utility-cart"));
    app.insert_resource(HallBlueprint(broken));

    assert_eq!(settle_assets(&mut app), AssetLoadState::Ready);
    pump(&mut app, 4);

    assert_eq!(
        *app.world().resource::<State<HallState>>().get(),
        HallState::Invalid
    );
    assert_eq!(
        app.world().resource::<HallErrors>().errors(),
        [SceneValidationError::MissingRequiredCollider(prop(
            "utility-cart"
        ))]
    );
    assert!(props(&mut app).is_empty());
    assert!(app.world().get_resource::<HallColliders>().is_none());
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
    let mut app = hall_app(&repo_assets());
    app.init_resource::<SetTrace>().add_systems(
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

// ---------------------------------------------------------------------------
// Technician movement contracts
// ---------------------------------------------------------------------------

/// Every arrow-key combination the movement matrix must define, including the
/// empty press and both opposing pairs.
const ARROW_MATRIX: [&[KeyCode]; 10] = [
    &[],
    &[KeyCode::ArrowLeft, KeyCode::ArrowRight],
    &[KeyCode::ArrowUp, KeyCode::ArrowDown],
    &[KeyCode::ArrowUp],
    &[KeyCode::ArrowDown],
    &[KeyCode::ArrowLeft],
    &[KeyCode::ArrowRight],
    &[KeyCode::ArrowUp, KeyCode::ArrowRight],
    &[KeyCode::ArrowUp, KeyCode::ArrowLeft],
    &[KeyCode::ArrowDown, KeyCode::ArrowRight],
];

/// The eight real key combinations the waypoint driver may choose from.
const DRIVE_KEYS: [&[KeyCode]; 8] = [
    &[KeyCode::ArrowUp],
    &[KeyCode::ArrowDown],
    &[KeyCode::ArrowLeft],
    &[KeyCode::ArrowRight],
    &[KeyCode::ArrowUp, KeyCode::ArrowRight],
    &[KeyCode::ArrowUp, KeyCode::ArrowLeft],
    &[KeyCode::ArrowDown, KeyCode::ArrowRight],
    &[KeyCode::ArrowDown, KeyCode::ArrowLeft],
];

const HEADINGS: [f32; 4] = [45.0, 135.0, 225.0, 315.0];
const FIXED_STEP: f64 = 1.0 / 60.0;

fn rig_is_bound(app: &mut App) -> bool {
    if !app.world().resource::<PlayerRigReport>().is_healthy() {
        return false;
    }
    let Some(parts) = app.world().get_resource::<PlayerParts>().cloned() else {
        return false;
    };
    parts.all().iter().all(|part| {
        app.world().get::<Name>(part.entity).map(Name::as_str) == Some(part.name.as_str())
    })
}

/// Boots the hall, then settles the technician.
///
/// Bevy respawns a glTF world instance when a sub-asset event arrives, which
/// happens in `SpawnScene` after `Update`. Systems always rebind before they
/// consume the rig, but a test reads between frames, so the harness waits for a
/// run of frames in which the bound handles still resolve.
fn walking_hall(assets: &Path) -> App {
    let mut app = built_hall(assets);
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
        FIXED_STEP,
    )));
    let mut stable = 0usize;
    for _ in 0..600 {
        app.update();
        let state = *app.world().resource::<State<PlayerRigState>>().get();
        match state {
            PlayerRigState::Failed => panic!(
                "technician rig failed: {:?}",
                app.world().resource::<PlayerRigReport>().errors()
            ),
            PlayerRigState::Ready if rig_is_bound(&mut app) => {
                stable += 1;
                if stable >= 30 {
                    return app;
                }
            }
            PlayerRigState::Ready | PlayerRigState::Pending => stable = 0,
        }
    }
    panic!("the technician rig never settled into a stable bound state");
}

fn technician_entity(app: &mut App) -> Entity {
    let entities = app
        .world_mut()
        .query_filtered::<Entity, With<Technician>>()
        .iter(app.world())
        .collect::<Vec<_>>();
    assert_eq!(entities.len(), 1, "exactly one technician must exist");
    entities[0]
}

fn technician_transform(app: &mut App) -> Transform {
    let entity = technician_entity(app);
    *app.world()
        .get::<Transform>(entity)
        .expect("the technician carries a transform")
}

fn player_position(app: &mut App) -> Vec2 {
    let transform = technician_transform(app);
    Vec2::new(transform.translation.x, transform.translation.z)
}

fn player_facing(app: &mut App) -> Vec2 {
    (technician_transform(app).rotation * TECHNICIAN_MODEL_FORWARD).xz()
}

fn place_player(app: &mut App, position: Vec2) {
    let entity = technician_entity(app);
    let mut transform = app
        .world_mut()
        .get_mut::<Transform>(entity)
        .expect("the technician carries a transform");
    transform.translation.x = position.x;
    transform.translation.z = position.y;
}

fn hold(app: &mut App, keys: &[KeyCode]) {
    let mut input = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    input.release_all();
    for key in keys {
        input.press(*key);
    }
}

fn drive(app: &mut App, keys: &[KeyCode], frames: usize) {
    hold(app, keys);
    for _ in 0..frames {
        app.update();
    }
}

/// Settles the camera on the heading a yaw names. The camera plugin is the
/// sole runtime updater of `ViewBasis`, so a test cannot poke the basis
/// directly without having it overwritten on the next frame.
fn set_heading(app: &mut App, degrees: f32) {
    let heading = CameraHeading::from_yaw_degrees(degrees)
        .unwrap_or_else(|| panic!("{degrees} degrees is not a settled heading"));
    app.world_mut()
        .insert_resource(CameraOrbit::settled(heading));
    app.world_mut()
        .resource_mut::<ViewBasis>()
        .set_yaw_degrees(degrees);
}

/// Despawns everything under the technician root, which is what Bevy's world
/// instance spawner does to the whole instance before it rebuilds it.
fn despawn_rig_instance(app: &mut App) {
    let root = technician_entity(app);
    let children = app
        .world()
        .get::<Children>(root)
        .map(|children| children.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        !children.is_empty(),
        "the technician instance must exist before it can be respawned"
    );
    for child in children {
        app.world_mut().entity_mut(child).despawn();
    }
    app.world_mut().flush();
}

/// Rebuilds a technician instance by hand from a list of node names. Rig
/// discovery joins on names, so this is exactly what the binder sees when the
/// engine hands it a replacement instance.
fn respawn_rig_instance(app: &mut App, names: &[&str]) {
    let root = technician_entity(app);
    let holder = app
        .world_mut()
        .spawn((
            AnimationPlayer::default(),
            Transform::IDENTITY,
            ChildOf(root),
        ))
        .id();
    for name in names {
        app.world_mut().spawn((
            Name::new((*name).to_owned()),
            Transform::IDENTITY,
            ChildOf(holder),
        ));
    }
    app.world_mut().flush();
}

fn rig_report(app: &App) -> PlayerRigReport {
    app.world().resource::<PlayerRigReport>().clone()
}

fn rig_state(app: &App) -> PlayerRigState {
    *app.world().resource::<State<PlayerRigState>>().get()
}

fn view_basis(app: &App) -> ViewBasis {
    *app.world().resource::<ViewBasis>()
}

fn animation_player(
    app: &mut App,
) -> (
    AnimationNodeIndex,
    AnimationNodeIndex,
    Vec<AnimationNodeIndex>,
) {
    let animations = app.world().resource::<PlayerAnimations>().clone();
    let playing = app
        .world()
        .get::<AnimationPlayer>(animations.player)
        .expect("the technician exposes an AnimationPlayer")
        .playing_animations()
        .map(|(index, _)| *index)
        .collect::<Vec<_>>();
    (
        animations.node(PlayerClip::Idle),
        animations.node(PlayerClip::Walk),
        playing,
    )
}

fn part_transforms(app: &mut App) -> Vec<(String, Transform, Transform)> {
    let parts = app.world().resource::<PlayerParts>().clone();
    parts
        .all()
        .iter()
        .map(|part| {
            (
                part.name.clone(),
                *app.world()
                    .get::<Transform>(part.entity)
                    .unwrap_or_else(|| panic!("{} must still exist", part.name)),
                part.rest,
            )
        })
        .collect()
}

/// Chooses the real key combination whose world direction best matches the
/// direction the route wants next. The journey therefore moves only through
/// accepted `ButtonInput<KeyCode>` state, never by writing a transform.
fn keys_towards(basis: &ViewBasis, desired: Vec2) -> &'static [KeyCode] {
    let desired = desired.normalize_or_zero();
    DRIVE_KEYS
        .into_iter()
        .max_by(|left, right| {
            let score = |keys: &[KeyCode]| {
                let mut input = ButtonInput::default();
                for key in keys {
                    input.press(*key);
                }
                basis.world_direction(arrow_input(&input)).dot(desired)
            };
            score(left)
                .partial_cmp(&score(right))
                .expect("world directions are finite")
        })
        .expect("the drive matrix is not empty")
}

#[derive(Resource, Default)]
struct RestProbe(Vec<(String, Transform, Transform)>);

fn capture_rest_probe(
    state: Res<PlayerAnimationState>,
    parts: Res<PlayerParts>,
    transforms: Query<&Transform>,
    mut probe: ResMut<RestProbe>,
) {
    if !state.is_changed() || state.current() != PlayerClip::Idle {
        return;
    }
    probe.0 = parts
        .all()
        .iter()
        .map(|part| {
            (
                part.name.clone(),
                *transforms
                    .get(part.entity)
                    .expect("every discovered part exists"),
                part.rest,
            )
        })
        .collect();
}

#[test]
fn player_spawns_the_rigged_technician_only_after_the_hall_is_ready() {
    let mut app = hall_app(&repo_assets());
    assert_eq!(
        *app.world().resource::<State<PlayerRigState>>().get(),
        PlayerRigState::Pending
    );
    assert_eq!(
        app.world_mut()
            .query_filtered::<Entity, With<Technician>>()
            .iter(app.world())
            .count(),
        0,
        "nothing may spawn while the generated assets are still loading"
    );
    assert_eq!(view_basis(&app), ViewBasis::from_yaw_degrees(45.0));

    let mut app = walking_hall(&repo_assets());
    assert_eq!(
        *app.world().resource::<State<HallState>>().get(),
        HallState::Ready
    );
    let spawned = player_position(&mut app);
    assert_eq!(spawned, app.world().resource::<PlayerSpawnPoint>().0);
    assert!(
        app.world()
            .resource::<HallColliders>()
            .first_overlap(spawned, PLAYER_RADIUS)
            .is_none()
    );
    assert!(app.world().resource::<PlayerRigReport>().is_healthy());
}

#[test]
fn player_discovers_every_required_rig_node_and_animation_clip() {
    let mut app = walking_hall(&repo_assets());
    let parts = app.world().resource::<PlayerParts>().clone();

    assert_eq!(
        parts
            .all()
            .iter()
            .map(|part| part.name.as_str())
            .collect::<Vec<_>>(),
        required_player_parts()
    );
    assert_eq!(parts.all().len(), TECHNICIAN_BONES.len() + 1);
    for bone in TECHNICIAN_BONES {
        let part = parts.get(bone).unwrap_or_else(|| panic!("{bone} is bound"));
        assert!(
            app.world().get::<Name>(part.entity).map(Name::as_str) == Some(bone),
            "{bone} must resolve to the named rig node"
        );
    }
    assert_eq!(
        parts.get("bone-hips").map(|part| part.rest.translation),
        Some(Vec3::new(0.0, 0.95, 0.0)),
        "rest transforms are captured before any clip plays"
    );

    let animations = app.world().resource::<PlayerAnimations>().clone();
    let mut nodes = PlayerClip::ALL
        .into_iter()
        .map(|clip| animations.node(clip))
        .collect::<Vec<_>>();
    nodes.sort();
    nodes.dedup();
    assert_eq!(nodes.len(), 3, "each clip needs its own graph node");
    assert!(
        app.world()
            .resource::<Assets<AnimationGraph>>()
            .get(&animations.graph)
            .is_some()
    );
    assert_eq!(
        app.world()
            .get::<AnimationGraphHandle>(animations.player)
            .map(|handle| handle.id()),
        Some(animations.graph.id())
    );

    let (idle, _, playing) = animation_player(&mut app);
    assert_eq!(playing, vec![idle], "a standing technician plays Idle only");
    assert_eq!(
        app.world().resource::<PlayerAnimationState>().current(),
        PlayerClip::Idle
    );
}

#[test]
fn keyboard_movement_matrix_covers_every_arrow_combination_at_every_heading() {
    let mut app = walking_hall(&repo_assets());
    let origin = Vec2::new(AISLE_CENTER_X[1], 0.0);
    let frames = 10usize;
    let expected_distance = PLAYER_SPEED * FIXED_STEP as f32 * frames as f32;

    for heading in HEADINGS {
        set_heading(&mut app, heading);
        let basis = view_basis(&app);
        for keys in ARROW_MATRIX {
            place_player(&mut app, origin);
            drive(&mut app, keys, frames);

            let mut request = ButtonInput::default();
            for key in keys {
                request.press(*key);
            }
            let screen = arrow_input(&request);
            let expected = basis.world_direction(screen) * expected_distance;
            let actual = player_position(&mut app) - origin;

            assert!(
                actual.distance(expected) < 1.0e-4,
                "heading {heading} with {keys:?} moved {actual:?}, expected {expected:?}"
            );
            let motion = app.world().resource::<PlayerMotion>();
            assert_eq!(motion.requested_screen, screen, "{heading} {keys:?}");
            assert!(!motion.resolution.was_restricted(), "{heading} {keys:?}");
            assert_eq!(
                motion.is_walking(),
                screen != Vec2::ZERO,
                "{heading} {keys:?}"
            );
            if screen != Vec2::ZERO {
                assert!(
                    player_facing(&mut app).distance(expected.normalize()) < 1.0e-4,
                    "heading {heading} with {keys:?} must face its accepted step"
                );
            }
        }
    }

    // Opposing keys cancel to exactly the same result as pressing nothing.
    set_heading(&mut app, INITIAL_CAMERA_YAW_DEGREES);
    for keys in [
        [KeyCode::ArrowLeft, KeyCode::ArrowRight].as_slice(),
        [KeyCode::ArrowUp, KeyCode::ArrowDown].as_slice(),
        [
            KeyCode::ArrowUp,
            KeyCode::ArrowDown,
            KeyCode::ArrowLeft,
            KeyCode::ArrowRight,
        ]
        .as_slice(),
    ] {
        place_player(&mut app, origin);
        drive(&mut app, keys, 30);
        assert_eq!(player_position(&mut app), origin, "{keys:?} must cancel");
    }
}

#[test]
fn keyboard_movement_slides_along_racks_and_stops_at_the_room_boundary() {
    let mut app = walking_hall(&repo_assets());
    let colliders = app.world().resource::<HallColliders>().clone();
    let rack_face = RACK_ROW_X[0] + 0.8 + PLAYER_RADIUS;

    // Straight into the west rack row of the first aisle.
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[0], 0.0));
    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowLeft], 120);
    let stopped = player_position(&mut app);

    assert!(
        stopped.x > rack_face && stopped.x <= rack_face + PLAYER_SPEED * FIXED_STEP as f32,
        "the technician must stop against the rack face, got {stopped:?}"
    );
    assert_eq!(stopped.y, 0.0, "a head-on stop may not drift sideways");
    assert_eq!(
        app.world()
            .resource::<PlayerMotion>()
            .resolution
            .blocked_x
            .as_ref()
            .map(PropId::as_str),
        Some("rack-row-01")
    );
    assert!(!colliders.overlaps(stopped, PLAYER_RADIUS));

    // Still pressed against the rack, request a diagonal: X stays clamped
    // against the rack face and Z slides freely.
    drive(&mut app, &[KeyCode::ArrowUp], 30);
    let slid = player_position(&mut app);
    assert!(
        slid.x > rack_face && slid.x <= stopped.x,
        "the blocked axis may only creep up to the rack face, got {slid:?}"
    );
    assert!(slid.y < -0.5, "the free axis must slide, got {slid:?}");
    let motion = app.world().resource::<PlayerMotion>().clone();
    assert!(motion.resolution.blocked_x.is_some());
    assert_eq!(motion.resolution.blocked_z, None);
    assert!(
        player_facing(&mut app).distance(Vec2::new(0.0, -1.0)) < 1.0e-4,
        "facing follows the accepted slide, not the requested diagonal"
    );

    // Into the hose drop, which is the authored pinch of every aisle.
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[0], 4.0));
    drive(&mut app, &[KeyCode::ArrowDown, KeyCode::ArrowLeft], 120);
    let pinched = player_position(&mut app);
    assert!(
        pinched.y < HOSE_DROP_Z - 0.2 - PLAYER_RADIUS,
        "the hose drop must stop a centred approach, got {pinched:?}"
    );
    assert_eq!(
        app.world()
            .resource::<PlayerMotion>()
            .resolution
            .blocked_z
            .as_ref()
            .map(PropId::as_str),
        Some("hose-drop-01")
    );

    // Radius-aware room bounds.
    let limit = ROOM_SIZE * 0.5 - Vec2::splat(PLAYER_RADIUS);
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 19.0));
    drive(&mut app, &[KeyCode::ArrowDown, KeyCode::ArrowLeft], 120);
    assert_eq!(player_position(&mut app).y, limit.y);
    assert!(app.world().resource::<PlayerMotion>().resolution.clamped_z);

    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], -19.0));
    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 120);
    assert_eq!(player_position(&mut app).y, -limit.y);
    assert!(app.world().resource::<PlayerMotion>().resolution.clamped_z);
}

#[test]
fn keyboard_movement_drives_the_generated_walk_and_idle_clips() {
    let mut app = walking_hall(&repo_assets());
    app.init_resource::<RestProbe>().add_systems(
        Update,
        capture_rest_probe
            .in_set(CellShiftSet::UpdateAnimation)
            .after(update_player_animation),
    );
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));

    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 40);
    let (idle, walk, playing) = animation_player(&mut app);
    assert_eq!(
        app.world().resource::<PlayerAnimationState>().current(),
        PlayerClip::Walk
    );
    assert_eq!(playing, vec![walk], "walking stops Idle and plays Walk");
    assert!(!playing.contains(&idle));

    let moved = part_transforms(&mut app)
        .into_iter()
        .filter(|(_, current, rest)| current != rest)
        .count();
    assert!(
        moved > 0,
        "the Walk clip must actually pose the discovered rig nodes"
    );

    app.world_mut().resource_mut::<RestProbe>().0.clear();
    drive(&mut app, &[], 1);

    let (idle, walk, playing) = animation_player(&mut app);
    assert_eq!(
        app.world().resource::<PlayerAnimationState>().current(),
        PlayerClip::Idle
    );
    assert_eq!(playing, vec![idle], "stopping stops Walk and plays Idle");
    assert!(!playing.contains(&walk));

    let probe = app.world().resource::<RestProbe>();
    assert_eq!(
        probe.0.len(),
        required_player_parts().len(),
        "the idle transition must visit every discovered part"
    );
    for (name, restored, rest) in &probe.0 {
        assert_eq!(restored, rest, "{name} must be restored to its rest pose");
    }
}

#[test]
fn keyboard_movement_stops_while_the_technician_instance_is_unavailable() {
    let mut app = walking_hall(&repo_assets());
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));
    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 5);
    let before = player_position(&mut app);
    assert_ne!(before, Vec2::new(AISLE_CENTER_X[1], 0.0));

    despawn_rig_instance(&mut app);
    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 30);

    assert_eq!(
        rig_report(&app).errors(),
        [PlayerRigError::TechnicianInstanceUnavailable { found: 0 }],
        "an unresolvable instance root must be reported, not passed over"
    );
    assert!(!rig_report(&app).is_healthy());
    assert_eq!(rig_state(&app), PlayerRigState::Pending);
    assert_eq!(
        player_position(&mut app),
        before,
        "movement must stop while the technician instance is unavailable"
    );
}

#[test]
fn keyboard_movement_stops_while_the_technician_rig_nodes_are_unavailable() {
    let mut app = walking_hall(&repo_assets());
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));
    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 5);
    let before = player_position(&mut app);
    assert_ne!(before, Vec2::new(AISLE_CENTER_X[1], 0.0));

    // A respawning instance can have its root back before its named rig nodes.
    despawn_rig_instance(&mut app);
    let root = technician_entity(&mut app);
    app.world_mut().spawn((Transform::IDENTITY, ChildOf(root)));
    app.world_mut().flush();

    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 30);

    assert_eq!(
        rig_report(&app).errors(),
        [PlayerRigError::TechnicianRigNodesUnavailable],
        "an instance with no named nodes must be reported, not passed over"
    );
    assert_eq!(rig_state(&app), PlayerRigState::Pending);
    assert_eq!(
        player_position(&mut app),
        before,
        "movement must stop while the rig nodes are unavailable"
    );
}

#[test]
fn technician_rig_rebinds_automatically_after_a_whole_instance_respawn() {
    let mut app = walking_hall(&repo_assets());
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));
    let original = app.world().resource::<PlayerParts>().clone();

    despawn_rig_instance(&mut app);
    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 5);
    let stopped = player_position(&mut app);
    assert!(
        !rig_report(&app).is_healthy(),
        "a vanished instance may not leave the report healthy"
    );

    respawn_rig_instance(&mut app, &required_player_parts());
    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 20);

    let report = rig_report(&app);
    assert!(
        report.is_healthy(),
        "a complete respawn must recover on its own, got {:?}",
        report.errors()
    );
    assert_eq!(rig_state(&app), PlayerRigState::Ready);

    let rebound = app.world().resource::<PlayerParts>().clone();
    assert_eq!(
        rebound
            .all()
            .iter()
            .map(|part| part.name.as_str())
            .collect::<Vec<_>>(),
        required_player_parts()
    );
    for part in rebound.all() {
        assert_eq!(
            app.world().get::<Name>(part.entity).map(Name::as_str),
            Some(part.name.as_str())
        );
        assert_ne!(
            original.entity(&part.name),
            Some(part.entity),
            "{} must rebind onto the replacement instance",
            part.name
        );
    }
    assert_ne!(
        player_position(&mut app),
        stopped,
        "movement must resume once the complete rig returns"
    );
}

#[test]
fn technician_rig_reports_specific_errors_when_an_incomplete_instance_respawns() {
    let mut app = walking_hall(&repo_assets());
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));
    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 5);
    let before = player_position(&mut app);

    let mut names = required_player_parts();
    names.retain(|name| *name != "bone-tool");
    names.push("bone-head");

    despawn_rig_instance(&mut app);
    respawn_rig_instance(&mut app, &names);
    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 20);

    let report = rig_report(&app);
    assert_eq!(
        report.errors(),
        [
            PlayerRigError::DuplicatePart {
                name: "bone-head".to_owned(),
                found: 2,
            },
            PlayerRigError::MissingPart {
                name: "bone-tool".to_owned(),
            },
        ],
        "an incomplete replacement instance must name exactly what it lost"
    );
    assert!(
        !report
            .errors()
            .iter()
            .any(|error| matches!(error, PlayerRigError::StalePart { .. })),
        "specific rescan errors must survive, not be buried under stale handles"
    );
    assert_eq!(rig_state(&app), PlayerRigState::Failed);
    assert_eq!(player_position(&mut app), before);
}

#[test]
fn keyboard_movement_stops_when_a_rig_part_goes_stale() {
    let mut app = walking_hall(&repo_assets());
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));
    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 5);
    let before_injection = player_position(&mut app);
    assert_ne!(before_injection, Vec2::new(AISLE_CENTER_X[1], 0.0));

    let stale = app
        .world()
        .resource::<PlayerParts>()
        .entity("bone-tool")
        .expect("bone-tool is discovered");
    app.world_mut().entity_mut(stale).despawn();

    drive(&mut app, &[KeyCode::ArrowUp, KeyCode::ArrowRight], 30);

    assert_eq!(
        app.world().resource::<PlayerRigReport>().errors(),
        [PlayerRigError::StalePart {
            name: "bone-tool".to_owned()
        }]
    );
    assert_eq!(
        *app.world().resource::<State<PlayerRigState>>().get(),
        PlayerRigState::Failed
    );
    assert_eq!(
        player_position(&mut app),
        before_injection,
        "a stale rig handle must stop movement instead of silently skipping"
    );
}

#[test]
fn keyboard_movement_clamps_a_hitch_delta_instead_of_tunnelling_the_hose_drop() {
    let mut app = walking_hall(&repo_assets());
    let colliders = app.world().resource::<HallColliders>().clone();
    let hose = colliders
        .get(&prop("hose-drop-01"))
        .expect("the authored hose drop")
        .clone();
    let blocked_half = hose.half_extents.y + PLAYER_RADIUS;
    let face = hose.center.y - blocked_half;

    // The movement clamp must match or beat the engine clock it shadows.
    assert!(
        PLAYER_MAX_MOVE_DELTA
            <= app
                .world()
                .resource::<Time<Virtual>>()
                .max_delta()
                .as_secs_f32(),
        "the movement clamp must be at least as strict as Bevy's virtual clock"
    );

    // Widen the engine's own clamp, so what this regression proves is the
    // movement clamp rather than Bevy's default maximum delta.
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_max_delta(Duration::from_secs(4));
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));

    // One unclamped hitch frame would carry the technician PLAYER_SPEED metres:
    // straight over the narrowest radius-inflated obstacle in the hall.
    let start = Vec2::new(hose.center.x, face - 0.25);
    assert!(PLAYER_SPEED * 1.0 > 2.0 * blocked_half);
    assert!(!colliders.overlaps(start, PLAYER_RADIUS));
    place_player(&mut app, start);

    drive(&mut app, &[KeyCode::ArrowDown, KeyCode::ArrowLeft], 1);
    let after_hitch = player_position(&mut app);
    assert!(
        after_hitch.y <= face,
        "one hitch frame tunnelled past the hose drop to {after_hitch:?}"
    );
    assert_eq!(after_hitch, start, "the hose drop rejects the whole step");
    assert_eq!(
        app.world()
            .resource::<PlayerMotion>()
            .resolution
            .blocked_z
            .as_ref()
            .map(PropId::as_str),
        Some("hose-drop-01")
    );

    // Holding through a run of hitch frames never crosses it either.
    for _ in 0..30 {
        app.update();
        let position = player_position(&mut app);
        assert!(
            position.y <= face,
            "a hitch frame tunnelled past the hose drop to {position:?}"
        );
        assert!(!colliders.overlaps(position, PLAYER_RADIUS));
    }

    // A clamped step still walks: the same hitch frames move the technician the
    // clamped distance when nothing is in the way.
    let clear = Vec2::new(hose.center.x, AISLE_Z_MIN);
    place_player(&mut app, clear);
    drive(&mut app, &[KeyCode::ArrowDown, KeyCode::ArrowLeft], 1);
    let stepped = player_position(&mut app);
    assert!(
        (stepped.y - (clear.y + PLAYER_SPEED * PLAYER_MAX_MOVE_DELTA)).abs() < 1.0e-4,
        "a hitch frame must advance exactly one clamped step, got {stepped:?}"
    );
}

#[test]
fn aisle_waypoint_journey_reaches_every_aisle_through_the_authored_hose_pinch() {
    let mut app = walking_hall(&repo_assets());
    let colliders = app.world().resource::<HallColliders>().clone();
    let scene = SceneBlueprint::v0();
    let limit = ROOM_SIZE * 0.5 - Vec2::splat(PLAYER_RADIUS);
    let hose_half = 0.2 + PLAYER_RADIUS;

    // Every aisle end to end, detouring around each authored hose drop. The
    // detour offset is deliberately wider than the hose half-extent plus the
    // player radius and narrower than the rack faces.
    let detour = 1.3f32;
    let mut route = vec![Vec2::new(AISLE_CENTER_X[0], -11.0)];
    for (index, center_x) in AISLE_CENTER_X.into_iter().enumerate() {
        let (entry, exit) = if index % 2 == 0 {
            (-11.0, 11.0)
        } else {
            (11.0, -11.0)
        };
        let sign = if entry < exit { 1.0 } else { -1.0 };
        let offset = if index == 1 { detour } else { -detour };
        route.extend([
            Vec2::new(center_x, entry),
            Vec2::new(center_x, HOSE_DROP_Z - sign * 3.0),
            Vec2::new(center_x + offset, HOSE_DROP_Z - sign * 3.0),
            Vec2::new(center_x + offset, HOSE_DROP_Z + sign * 2.5),
            Vec2::new(center_x, HOSE_DROP_Z + sign * 2.5),
            Vec2::new(center_x, exit),
        ]);
    }

    let mut trace = Vec::new();
    let mut frames = 0usize;
    for target in &route {
        loop {
            let position = player_position(&mut app);
            trace.push(position);
            if position.distance(*target) <= 0.1 {
                break;
            }
            let keys = keys_towards(&view_basis(&app), *target - position);
            hold(&mut app, keys);
            app.update();
            frames += 1;
            assert!(
                frames < 6_000,
                "the waypoint journey stalled at {position:?} heading for {target:?}"
            );
        }
    }
    hold(&mut app, &[]);
    app.update();

    assert!(
        frames > 1_000,
        "the journey must be a real walk, got {frames}"
    );
    for position in &trace {
        assert!(
            !colliders.overlaps(*position, PLAYER_RADIUS),
            "the journey entered a collider at {position:?}"
        );
        assert!(
            position.x.abs() <= limit.x + 1.0e-4 && position.y.abs() <= limit.y + 1.0e-4,
            "the journey left the room at {position:?}"
        );
    }

    for (index, aisle) in scene.aisles.iter().enumerate() {
        // The authored pinch really does close the centre line, so the only way
        // through is the off-centre detour the driver walked.
        assert!(
            colliders.overlaps(Vec2::new(aisle.center_x, HOSE_DROP_Z), PLAYER_RADIUS),
            "aisle {index} centre must be blocked at the hose drop"
        );
        assert!(detour > hose_half && detour < 1.85);

        let band = aisle.half_width + detour;
        let samples = trace
            .iter()
            .filter(|point| (point.x - aisle.center_x).abs() <= band)
            .collect::<Vec<_>>();
        assert!(
            samples.iter().any(
                |point| point.y <= AISLE_Z_MIN + 1.1 && (point.x - aisle.center_x).abs() < 0.15
            ),
            "aisle {index} was never entered from its north end"
        );
        assert!(
            samples.iter().any(
                |point| point.y >= AISLE_Z_MAX - 1.1 && (point.x - aisle.center_x).abs() < 0.15
            ),
            "aisle {index} was never left from its south end"
        );

        let crossings = samples
            .iter()
            .filter(|point| (point.y - HOSE_DROP_Z).abs() <= 0.05)
            .collect::<Vec<_>>();
        assert!(
            !crossings.is_empty(),
            "aisle {index} never crossed the authored hose pinch"
        );
        for point in crossings {
            assert!(
                (point.x - aisle.center_x).abs() > hose_half,
                "aisle {index} crossed the hose at {point:?} instead of routing around it"
            );
        }
    }

    assert!(app.world().resource::<PlayerRigReport>().is_healthy());
    assert_eq!(
        app.world().resource::<PlayerAnimationState>().current(),
        PlayerClip::Idle
    );
}

// ---------------------------------------------------------------------------
// Camera orbit and follow contracts
// ---------------------------------------------------------------------------

/// The margin, in logical pixels, the followed technician must keep from every
/// viewport edge before the frame counts as framed.
const FRAMING_MARGIN_PIXELS: f32 = 32.0;

/// Frames of the fixed test step that make up one settled quarter turn.
const QUARTER_TURN_FRAMES: usize = 18;

fn orbit(app: &App) -> CameraOrbit {
    *app.world().resource::<CameraOrbit>()
}

fn camera_entity(app: &mut App) -> Entity {
    let entities = app
        .world_mut()
        .query_filtered::<Entity, With<CellShiftCamera>>()
        .iter(app.world())
        .collect::<Vec<_>>();
    assert_eq!(entities.len(), 1, "exactly one game camera must exist");
    entities[0]
}

fn camera_placement(app: &mut App) -> (Transform, GlobalTransform) {
    let entity = camera_entity(app);
    let transform = *app
        .world()
        .get::<Transform>(entity)
        .expect("the camera carries a transform");
    let global = *app
        .world()
        .get::<GlobalTransform>(entity)
        .expect("the camera carries a propagated transform");
    (transform, global)
}

/// The ground point the camera is centred on, recovered from the real camera
/// transform rather than from the resource that produced it.
fn camera_ground_target(app: &mut App) -> Vec2 {
    let (transform, _) = camera_placement(app);
    let forward = *transform.forward();
    let travel = transform.translation.y / -forward.y;
    let focus = transform.translation + forward * travel;
    Vec2::new(focus.x, focus.z)
}

/// Viewport position of a ground point, through the real Bevy projection.
fn viewport_of(app: &mut App, ground: Vec2) -> Vec2 {
    let entity = camera_entity(app);
    let global = *app
        .world()
        .get::<GlobalTransform>(entity)
        .expect("the camera carries a propagated transform");
    let camera = app
        .world()
        .get::<Camera>(entity)
        .expect("the camera carries a Camera")
        .clone();
    camera
        .world_to_viewport(&global, Vec3::new(ground.x, 0.0, ground.y))
        .unwrap_or_else(|error| panic!("{ground:?} did not project: {error:?}"))
}

fn viewport_size(app: &mut App) -> Vec2 {
    let entity = camera_entity(app);
    app.world()
        .get::<Camera>(entity)
        .expect("the camera carries a Camera")
        .logical_viewport_size()
        .expect("the camera must know its viewport size")
}

/// Smallest distance from a projected ground point to any viewport edge.
/// Negative when the point is off screen.
fn framing_margin(app: &mut App, ground: Vec2) -> f32 {
    let size = viewport_size(app);
    let point = viewport_of(app, ground);
    point
        .x
        .min(point.y)
        .min(size.x - point.x)
        .min(size.y - point.y)
}

/// Sends one real key press message, which is the only path that produces a
/// `just_pressed` frame once `keyboard_input_system` has cleared the resource.
fn key_message(app: &mut App, key: KeyCode, state: ButtonState) {
    let window = app
        .world_mut()
        .query_filtered::<Entity, With<Window>>()
        .iter(app.world())
        .next()
        .expect("the app has a primary window");
    app.world_mut().write_message(KeyboardInput {
        key_code: key,
        logical_key: Key::Unidentified(NativeKey::Unidentified),
        state,
        text: None,
        repeat: false,
        window,
    });
}

/// Presses and releases the given keys across exactly one frame.
fn tap(app: &mut App, keys: &[KeyCode]) {
    for key in keys {
        key_message(app, *key, ButtonState::Pressed);
    }
    app.update();
    for key in keys {
        key_message(app, *key, ButtonState::Released);
    }
}

/// Runs a settled quarter turn and asserts it lands exactly on the heading.
fn settle_orbit(app: &mut App, key: KeyCode) -> CameraHeading {
    tap(app, &[key]);
    pump(app, QUARTER_TURN_FRAMES - 1);
    let orbit = orbit(app);
    assert!(
        orbit.is_settled(),
        "a quarter turn must settle in {QUARTER_TURN_FRAMES} frames, {} s remained",
        orbit.remaining_seconds()
    );
    orbit.heading()
}

fn camera_app(assets: &Path) -> App {
    let mut app = built_hall(assets);
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
        FIXED_STEP,
    )));
    app.update();
    app
}

#[test]
fn camera_orbit_spawns_one_orthographic_camera_with_the_reviewed_projection() {
    let mut app = camera_app(&repo_assets());
    let entity = camera_entity(&mut app);

    assert!(
        app.world().get::<Camera3d>(entity).is_some(),
        "the game camera must be a real 3D camera"
    );
    let projection = app
        .world()
        .get::<Projection>(entity)
        .expect("the camera carries a projection");
    let Projection::Orthographic(orthographic) = projection else {
        panic!("the reviewed camera must be orthographic, got {projection:?}");
    };
    assert!(
        matches!(
            orthographic.scaling_mode,
            ScalingMode::Fixed { width, height }
                if width == ORTHOGRAPHIC_WIDTH && height == ORTHOGRAPHIC_HEIGHT
        ),
        "the projection must stay a fixed 26 m by 14.625 m rectangle, got {:?}",
        orthographic.scaling_mode
    );
    // `camera_system` recomputes `area` from the live window, so this proves
    // the fixed rectangle survived the real 1280x720 target.
    assert_eq!(
        orthographic.area,
        Rect::new(
            -ORTHOGRAPHIC_WIDTH * 0.5,
            -ORTHOGRAPHIC_HEIGHT * 0.5,
            ORTHOGRAPHIC_WIDTH * 0.5,
            ORTHOGRAPHIC_HEIGHT * 0.5,
        )
    );
    assert_eq!(orthographic.scale, 1.0);
    assert_eq!(
        viewport_size(&mut app),
        Vec2::new(DEFAULT_WINDOW_WIDTH as f32, DEFAULT_WINDOW_HEIGHT as f32)
    );

    // The authored hall is unlit, so the camera alone must make it visible.
    assert_eq!(
        app.world_mut()
            .query::<&PointLight>()
            .iter(app.world())
            .count()
            + app
                .world_mut()
                .query::<&DirectionalLight>()
                .iter(app.world())
                .count()
            + app
                .world_mut()
                .query::<&SpotLight>()
                .iter(app.world())
                .count(),
        0,
        "the cel-shift hall must not need a light"
    );
    assert!(
        app.world_mut()
            .query::<&HallProp>()
            .iter(app.world())
            .count()
            > 0
    );
}

#[test]
fn camera_orbit_projects_the_authored_metre_scale_onto_the_real_viewport() {
    let mut app = camera_app(&repo_assets());
    let size = viewport_size(&mut app);
    let basis = view_basis(&app);
    let focus = camera_ground_target(&mut app);

    let centre = viewport_of(&mut app, focus);
    assert!(
        (centre - size * 0.5).abs().max_element() < 0.5,
        "the followed ground point {focus:?} should project to the viewport centre, got {centre:?}"
    );

    // 26 m across the screen must be exactly 1280 logical pixels, and the
    // fixed elevation must foreshorten the ground depth by sin(57 degrees).
    let right_edge = viewport_of(&mut app, focus + basis.right() * ORTHOGRAPHIC_WIDTH * 0.5);
    assert!(
        (right_edge.x - size.x).abs() < 0.5 && (right_edge.y - size.y * 0.5).abs() < 0.5,
        "13 m of screen right should land on the right edge, got {right_edge:?}"
    );
    let far_edge = viewport_of(
        &mut app,
        focus
            + basis.forward() * ORTHOGRAPHIC_HEIGHT * 0.5
                / CAMERA_ELEVATION_DEGREES.to_radians().sin(),
    );
    assert!(
        (far_edge.y).abs() < 0.5 && (far_edge.x - size.x * 0.5).abs() < 0.5,
        "the ground depth should land on the top edge, got {far_edge:?}"
    );

    // Every authored prop centre is inside the room, and the camera holds the
    // whole ground footprint inside it.
    for corner in ground_quadrilateral(orbit(&app).yaw_radians(), focus) {
        assert!(
            corner.x.abs() <= ROOM_SIZE.x * 0.5 + 1.0e-3
                && corner.y.abs() <= ROOM_SIZE.y * 0.5 + 1.0e-3,
            "the initial view already leaks past the room at {corner:?}"
        );
    }
}

#[test]
fn camera_orbit_real_q_and_e_keys_walk_every_heading_in_both_directions() {
    let mut app = camera_app(&repo_assets());
    assert_eq!(orbit(&app).heading(), CameraHeading::NorthEast);

    for expected in [
        CameraHeading::SouthEast,
        CameraHeading::SouthWest,
        CameraHeading::NorthWest,
        CameraHeading::NorthEast,
    ] {
        assert_eq!(settle_orbit(&mut app, KeyCode::KeyE), expected);
        assert_heading(&mut app, expected);
    }

    for expected in [
        CameraHeading::NorthWest,
        CameraHeading::SouthWest,
        CameraHeading::SouthEast,
        CameraHeading::NorthEast,
    ] {
        assert_eq!(settle_orbit(&mut app, KeyCode::KeyQ), expected);
        assert_heading(&mut app, expected);
    }
}

/// Asserts the resource, the published basis, and the real camera entity all
/// agree that the settled heading has been reached.
fn assert_heading(app: &mut App, expected: CameraHeading) {
    let orbit = orbit(app);
    assert!(
        (orbit.yaw_degrees() - expected.yaw_degrees()).abs() < 1.0e-2
            || (orbit.yaw_degrees() - expected.yaw_degrees()).abs() > 359.99,
        "{expected:?} should settle at {} degrees, got {}",
        expected.yaw_degrees(),
        orbit.yaw_degrees()
    );
    assert_eq!(
        view_basis(app),
        ViewBasis::from_yaw_radians(orbit.yaw_radians()),
        "movement must read the settled basis"
    );

    let target = camera_ground_target(app);
    let (transform, _) = camera_placement(app);
    let offset = transform.translation - Vec3::new(target.x, 0.0, target.y);
    let quadrant = match expected {
        CameraHeading::NorthEast => Vec2::new(1.0, 1.0),
        CameraHeading::SouthEast => Vec2::new(1.0, -1.0),
        CameraHeading::SouthWest => Vec2::new(-1.0, -1.0),
        CameraHeading::NorthWest => Vec2::new(-1.0, 1.0),
    };
    assert_eq!(
        Vec2::new(offset.x.signum(), offset.z.signum()),
        quadrant,
        "{expected:?} must put the real camera in its own compass quadrant, offset {offset:?}"
    );
    assert!(
        (offset.length() - CAMERA_DISTANCE).abs() < 1.0e-2,
        "the zoom must not change with the heading"
    );
    let elevation = (-transform.forward().y).asin().to_degrees();
    assert!(
        (elevation - CAMERA_ELEVATION_DEGREES).abs() < 1.0e-2,
        "{expected:?} elevation drifted to {elevation}"
    );
    assert!(
        transform.right().y.abs() < 1.0e-5,
        "{expected:?} introduced roll"
    );
}

#[test]
fn camera_orbit_cancels_opposing_keys_and_retargets_mid_tween_in_the_real_app() {
    let mut app = camera_app(&repo_assets());

    // Half a quarter turn in, both keys on one frame must change nothing.
    tap(&mut app, &[KeyCode::KeyE]);
    pump(&mut app, QUARTER_TURN_FRAMES / 2 - 1);
    let midpoint = orbit(&app);
    assert!(
        (midpoint.yaw_degrees() - 90.0).abs() < 1.0e-2,
        "the tween midpoint should be 90 degrees, got {}",
        midpoint.yaw_degrees()
    );
    tap(&mut app, &[KeyCode::KeyQ, KeyCode::KeyE]);
    let after = orbit(&app);
    assert_eq!(
        after.heading(),
        CameraHeading::SouthEast,
        "opposing keys on one frame must not retarget"
    );
    assert!(
        (after.duration_seconds() - CAMERA_ORBIT_DURATION_SECONDS).abs() < 1.0e-6,
        "opposing keys on one frame must not re-time the turn, got {}",
        after.duration_seconds()
    );
    pump(&mut app, QUARTER_TURN_FRAMES / 2 - 1);
    assert_heading(&mut app, CameraHeading::SouthEast);

    // A reversal mid-tween starts at the interpolated yaw and keeps the rate,
    // so it needs exactly the frames the remaining angle costs.
    tap(&mut app, &[KeyCode::KeyE]);
    pump(&mut app, QUARTER_TURN_FRAMES / 2 - 1);
    tap(&mut app, &[KeyCode::KeyQ]);
    let reversed = orbit(&app);
    assert_eq!(reversed.heading(), CameraHeading::SouthEast);
    assert!(
        (reversed.duration_seconds() - CAMERA_ORBIT_DURATION_SECONDS * 0.5).abs() < 1.0e-6,
        "a 45 degree reversal must take 0.15 s, got {}",
        reversed.duration_seconds()
    );
    pump(&mut app, QUARTER_TURN_FRAMES / 2 - 1);
    assert_heading(&mut app, CameraHeading::SouthEast);

    // A second turn queued mid-tween takes the longer, still constant-rate path.
    tap(&mut app, &[KeyCode::KeyE]);
    pump(&mut app, QUARTER_TURN_FRAMES / 2 - 1);
    tap(&mut app, &[KeyCode::KeyE]);
    let queued = orbit(&app);
    assert_eq!(queued.heading(), CameraHeading::NorthWest);
    assert!(
        (queued.duration_seconds() - CAMERA_ORBIT_DURATION_SECONDS * 1.5).abs() < 1.0e-6,
        "a 135 degree retarget must take 0.45 s, got {}",
        queued.duration_seconds()
    );
    pump(&mut app, QUARTER_TURN_FRAMES * 3 / 2 - 1);
    assert_heading(&mut app, CameraHeading::NorthWest);
}

/// Records the basis every consumer set saw, in the frame it saw it.
#[derive(Resource, Default)]
struct BasisTrace(Vec<(f32, f32, f32)>);

fn trace_basis_before_movement(
    orbit: Res<CameraOrbit>,
    basis: Res<ViewBasis>,
    mut trace: ResMut<BasisTrace>,
) {
    trace
        .0
        .push((orbit.yaw_degrees(), basis.yaw_degrees(), basis.forward().x));
}

#[test]
fn camera_orbit_publishes_the_interpolated_basis_before_movement_every_frame() {
    let mut app = camera_app(&repo_assets());
    app.init_resource::<BasisTrace>().add_systems(
        Update,
        trace_basis_before_movement.in_set(CellShiftSet::MovePlayer),
    );

    tap(&mut app, &[KeyCode::KeyE]);
    pump(&mut app, QUARTER_TURN_FRAMES - 1);

    let trace = app.world().resource::<BasisTrace>().0.clone();
    assert_eq!(
        trace.len(),
        QUARTER_TURN_FRAMES,
        "the probe must see every frame of the turn"
    );
    for (index, (orbit_yaw, basis_yaw, forward_x)) in trace.iter().enumerate() {
        assert!(
            (orbit_yaw - basis_yaw).abs() < 1.0e-4,
            "frame {index} let movement read a stale basis: orbit {orbit_yaw}, basis {basis_yaw}"
        );
        assert!(
            (forward_x + basis_yaw.to_radians().sin()).abs() < 1.0e-4,
            "frame {index} basis forward disagrees with its own yaw"
        );
    }
    let yaws = trace.iter().map(|entry| entry.1).collect::<Vec<_>>();
    assert!(
        yaws[0] > 45.0 && yaws[0] < 46.0,
        "the first frame of the turn should have barely moved, got {}",
        yaws[0]
    );
    assert!(
        yaws.windows(2).all(|pair| pair[1] > pair[0]),
        "the basis must advance every single frame, got {yaws:?}"
    );
    assert!(
        (yaws[QUARTER_TURN_FRAMES - 1] - 135.0).abs() < 1.0e-2,
        "the last frame should be settled, got {}",
        yaws[QUARTER_TURN_FRAMES - 1]
    );
}

#[test]
fn camera_orbit_moves_the_technician_along_the_live_mid_tween_basis() {
    let mut app = walking_hall(&repo_assets());
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));

    tap(&mut app, &[KeyCode::KeyE]);
    hold(&mut app, &[KeyCode::ArrowUp]);

    for frame in 0..(QUARTER_TURN_FRAMES - 1) {
        let before = player_position(&mut app);
        app.update();
        let basis = view_basis(&app);
        let accepted = player_position(&mut app) - before;
        assert!(
            accepted.length() > 1.0e-4,
            "frame {frame} of the orbit stopped the technician"
        );
        assert!(
            (accepted.normalize() - basis.forward()).abs().max_element() < 1.0e-3,
            "frame {frame} moved along {:?} instead of the live basis {:?}",
            accepted.normalize(),
            basis.forward()
        );
    }
    assert_eq!(orbit(&app).heading(), CameraHeading::SouthEast);
}

/// Ground positions the camera must frame: the room centre, the authored
/// player spawn, every aisle end, and the corners of the legal rectangle.
fn framing_samples(yaw_radians: f32) -> Vec<Vec2> {
    let mut samples = vec![Vec2::ZERO, SceneBlueprint::v0().player_spawn];
    for center_x in AISLE_CENTER_X {
        samples.push(Vec2::new(center_x, AISLE_Z_MIN));
        samples.push(Vec2::new(center_x, AISLE_Z_MAX));
        samples.push(Vec2::new(center_x, 0.0));
    }
    let (min, max) = camera_target_bounds(ROOM_SIZE, yaw_radians)
        .expect("the authored room must always have a legal rectangle");
    samples.extend([min, max, Vec2::new(min.x, max.y), Vec2::new(max.x, min.y)]);
    samples
}

/// Drives the camera to `yaw` by settling the nearest heading and then running
/// a real turn for the requested number of frames.
fn orbit_to(app: &mut App, heading: CameraHeading, frames: usize) {
    app.world_mut()
        .insert_resource(CameraOrbit::settled(heading));
    if frames > 0 {
        tap(app, &[KeyCode::KeyE]);
        pump(app, frames - 1);
    }
    app.update();
}

#[test]
fn camera_orbit_clamps_the_follow_target_and_keeps_the_technician_framed() {
    let mut app = walking_hall(&repo_assets());
    let half = ROOM_SIZE * 0.5;

    for heading in CameraHeading::ALL {
        // A settled heading, then the exact midpoint of the turn that leaves it.
        for frames in [0, QUARTER_TURN_FRAMES / 2, QUARTER_TURN_FRAMES / 4] {
            orbit_to(&mut app, heading, frames);
            let yaw = orbit(&app).yaw_radians();
            let bounds = camera_target_bounds(ROOM_SIZE, yaw);
            let (min, max) = bounds.unwrap_or_else(|| {
                panic!("yaw {} has no legal target rectangle", yaw.to_degrees())
            });
            assert!(
                max.min_element() > 0.0,
                "yaw {} collapsed the legal rectangle to {min:?}..{max:?}",
                yaw.to_degrees()
            );

            for sample in framing_samples(yaw) {
                place_player(&mut app, sample);
                app.update();
                let yaw = orbit(&app).yaw_radians();
                let target = camera_ground_target(&mut app);
                let expected = clamp_follow_target(sample, ROOM_SIZE, yaw);
                assert!(
                    (target - expected).abs().max_element() < 1.0e-3,
                    "yaw {} following {sample:?} settled on {target:?}, expected {expected:?}",
                    yaw.to_degrees()
                );

                for corner in ground_quadrilateral(yaw, target) {
                    assert!(
                        corner.x.abs() <= half.x + 1.0e-3 && corner.y.abs() <= half.y + 1.0e-3,
                        "yaw {} following {sample:?} pushed a ground corner to {corner:?}",
                        yaw.to_degrees()
                    );
                }

                let margin = framing_margin(&mut app, sample);
                assert!(
                    margin >= FRAMING_MARGIN_PIXELS,
                    "yaw {} framed {sample:?} with only {margin} px of margin",
                    yaw.to_degrees()
                );
            }
        }
    }
}

#[test]
fn camera_orbit_holds_the_room_edge_instead_of_leaking_past_it() {
    let mut app = walking_hall(&repo_assets());
    let reachable = ROOM_SIZE * 0.5 - Vec2::splat(PLAYER_RADIUS);
    let half = ROOM_SIZE * 0.5;

    // Every room corner, at every heading and at the tween midpoints between
    // them. The clamp is a hard containment guarantee: the camera stops at the
    // room edge rather than following the technician out of it.
    for heading in CameraHeading::ALL {
        for frames in [0, QUARTER_TURN_FRAMES / 2] {
            orbit_to(&mut app, heading, frames);
            for corner in [
                reachable,
                -reachable,
                Vec2::new(reachable.x, -reachable.y),
                Vec2::new(-reachable.x, reachable.y),
            ] {
                place_player(&mut app, corner);
                app.update();
                let yaw = orbit(&app).yaw_radians();
                let (_, max) = camera_target_bounds(ROOM_SIZE, yaw).expect("legal rectangle");
                let target = camera_ground_target(&mut app);

                assert!(
                    (target.abs() - max).abs().max_element() < 1.0e-3,
                    "yaw {} at corner {corner:?} should sit on the legal edge {max:?}, got {target:?}",
                    yaw.to_degrees()
                );
                for ground in ground_quadrilateral(yaw, target) {
                    assert!(
                        ground.x.abs() <= half.x + 1.0e-3 && ground.y.abs() <= half.y + 1.0e-3,
                        "yaw {} at corner {corner:?} leaked to {ground:?}",
                        yaw.to_degrees()
                    );
                }
            }
        }
    }
}

#[test]
fn camera_orbit_room_corner_framing_is_impossible_under_the_fixed_contract() {
    // Recorded plan defect. Task 5 asks for both a camera that never shows
    // anything outside the room and a technician framed at every room corner.
    // At a diagonal heading the two cannot hold together for any room size.
    //
    // At yaw 45 the whole diagonal footprint separates the legal target from
    // the corner, so the technician sits
    // `ORTHOGRAPHIC_WIDTH / 2 - PLAYER_RADIUS * sqrt(2)` metres beyond the far
    // ground edge. The room size cancels out of that expression: framing a
    // corner would need `ORTHOGRAPHIC_WIDTH <= 2 * PLAYER_RADIUS * sqrt(2)`,
    // which is 0.99 m rather than the reviewed 26 m.
    let mut app = walking_hall(&repo_assets());
    let corner = ROOM_SIZE * 0.5 - Vec2::splat(PLAYER_RADIUS);
    orbit_to(&mut app, CameraHeading::NorthEast, 0);
    place_player(&mut app, corner);
    app.update();

    let yaw = orbit(&app).yaw_radians();
    let basis = ViewBasis::from_yaw_radians(yaw);
    let target = camera_ground_target(&mut app);
    let behind = (corner - target).dot(basis.forward());
    let half_depth = ground_half_depth();
    let overshoot = -behind - half_depth;

    let closed_form = ORTHOGRAPHIC_WIDTH * 0.5 - PLAYER_RADIUS * std::f32::consts::SQRT_2;
    assert!(
        (overshoot - closed_form).abs() < 1.0e-3 && (overshoot - 12.505).abs() < 1.0e-2,
        "the corner technician should fall 12.505 m past the far edge, got {overshoot}"
    );
    let off_screen_pixels = overshoot * CAMERA_ELEVATION_DEGREES.to_radians().sin()
        / ORTHOGRAPHIC_HEIGHT
        * DEFAULT_WINDOW_HEIGHT as f32;
    assert!(
        (off_screen_pixels - 516.3).abs() < 1.0,
        "the corner technician should be 516 px off screen, got {off_screen_pixels}"
    );
    assert!(
        framing_margin(&mut app, corner) < 0.0,
        "the recorded defect requires the corner technician to be off screen"
    );

    // The algebra, not the authored room, is what makes this unreachable.
    let widest_framable_view = 2.0 * PLAYER_RADIUS * std::f32::consts::SQRT_2;
    assert!(
        (widest_framable_view - 0.9899).abs() < 1.0e-3,
        "the widest corner-framing view should be 0.99 m, got {widest_framable_view}"
    );
    assert!(
        ORTHOGRAPHIC_WIDTH > widest_framable_view,
        "the reviewed 26 m view is far wider than any corner-framing view"
    );
    for room in [40.0_f32, 60.0, 120.0] {
        let size = Vec2::splat(room);
        let reachable = size * 0.5 - Vec2::splat(PLAYER_RADIUS);
        let target = clamp_follow_target(reachable, size, yaw);
        let behind = -(reachable - target).dot(basis.forward());
        assert!(
            behind > half_depth,
            "a {room} m room still cannot frame its corner: {behind} m of {half_depth} m"
        );
    }
}
