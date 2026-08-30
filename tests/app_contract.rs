use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use bevy::{
    asset::{AssetPlugin as BevyAssetPlugin, RecursiveDependencyLoadState},
    color::palettes::css::BLACK,
    prelude::*,
    render::{
        RenderPlugin,
        settings::{RenderCreation, WgpuSettings},
    },
};
use midcreek_cs_1::{
    CellShiftPlugin, CellShiftSet,
    assetgen::{ASSET_MODULES, ASSET_NAMES, generate_glb, load_source},
    assets::{
        AssetLoadReport, AssetLoadState, GENERATED_ASSET_DIRECTORY, GeneratedAssets, RenderAssets,
        generated_modules, module_for,
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
