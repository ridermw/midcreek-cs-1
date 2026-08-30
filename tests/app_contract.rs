use std::{
    collections::BTreeSet,
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
    ui::{ComputedNode, UiGlobalTransform},
    window::WindowResized,
};
use midcreek_cs_1::{
    CellShiftPlugin, CellShiftSet,
    assetgen::{
        ASSET_MODULES, ASSET_NAMES, Axis, ChannelSource, ClipSource, KeySource, ModuleSource,
        PrimitiveSource, RigSource, TECHNICIAN_BONES, generate_glb, load_source,
    },
    assets::{
        AssetLoadReport, AssetLoadState, GENERATED_ASSET_DIRECTORY, GeneratedAssets, RenderAssets,
        generated_modules, module_for,
    },
    camera::{
        CAMERA_DISTANCE, CameraHeading, CameraOrbit, CellShiftCamera, active_coverage,
        camera_target_bounds, clamp_follow_target, coverage_holds_room, ground_half_depth,
        ground_quadrilateral,
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
        RENDER_APRON_DROP, RENDER_COVERAGE_SIZE, REPAIR_DURATION_SECONDS, REPAIR_INTERACTION_RANGE,
        RESOLVED_DISPLAY_SECONDS, ROOM_SIZE, SIGNATURE_YELLOW, SKY_BOUNCE_BLUE, SceneBlueprint,
        SceneValidationError, TEAL_ACCENT, VERIFICATION_WINDOW_HEIGHT, VERIFICATION_WINDOW_WIDTH,
        VisualSpec, WALKABLE_CELL_SIZE, WALL_HEIGHT, WALL_THICKNESS, WORKER_BOOTS, WORKER_HARD_HAT,
        WORKER_HI_VIS, WORKER_SKIN, WORKER_SLATE, WORKER_TROUSERS,
    },
    hud::{
        BADGE_HEIGHT, BADGE_WIDTH, BadgeKind, BadgeVisibility, CONTROLS_PANEL_HEIGHT,
        ControlHintCap, ControlHintCapLabel, ControlsPanel, HUD_MARGIN, HudControl, HudError,
        HudReport, HudRoot, HudStatus, HudStatusChip, HudStatusLabel, LEADER_WIDTH,
        QUEUE_CHIP_SIZE, QUEUE_PROGRESS_HEIGHT, QueueHeaderLabel, QueueRowLabel, QueueRowNode,
        QueueRowProgress, QueueRowSeverityChip, QueueRowStateChip, RackBadgeLabel, RackBadgeNode,
        RackLeaderLine, TicketQueuePanel, severity_role, state_role,
    },
    operations::{
        FAULT_INTERVAL, FAULT_SCHEDULER_SEED, FaultScheduler, InteractionOutcome, LastInteraction,
        MovementLock, OperationsClock, RACK_ASSET_KIND, RACK_COOLDOWN, REPAIR_DURATION, REPAIR_KEY,
        RESOLVED_DISPLAY, RackOperations, RackRoster, RackState, ScheduleBlock, Ticket, TicketId,
        TicketQueue, TicketSeverity,
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
    assert_eq!(scene.room.coverage, RENDER_COVERAGE_SIZE);
    assert_eq!(scene.player_spawn, Vec2::new(-6.0, -11.0));
    assert_eq!(
        scene
            .visuals
            .iter()
            .map(|visual| visual.id.as_str())
            .collect::<Vec<_>>(),
        [
            "render-apron",
            "floor",
            "floor-grid",
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
            "floor-marking-hazard-north",
            "floor-marking-hazard-south",
            "floor-marking-hazard-west",
            "floor-marking-hazard-east",
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
    assert_eq!(scene.count_of(AssetKind::RenderApron), 1);
    assert_eq!(scene.count_of(AssetKind::Floor), 1);
    assert_eq!(scene.count_of(AssetKind::FloorGrid), 1);
    assert_eq!(scene.count_of(AssetKind::Wall), 4);
    assert_eq!(scene.count_of(AssetKind::RackRow), 4);
    assert_eq!(scene.count_of(AssetKind::CoolingUnit), 4);
    assert_eq!(scene.count_of(AssetKind::OverheadTray), 3);
    assert_eq!(scene.count_of(AssetKind::HoseDrop), 3);
    assert_eq!(scene.count_of(AssetKind::UtilityCart), 1);
    assert_eq!(scene.count_of(AssetKind::StepStool), 1);
    assert_eq!(scene.count_of(AssetKind::FloorMarking), 12);
    assert_eq!(scene.visuals.len(), 35);
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
    assert_eq!(AssetKind::ALL.len(), 11);
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
        Some((PrimitiveShape::Cuboid, PaletteRole::RackWhite))
    );
    assert_eq!(
        AssetKind::RenderApron.primitive(),
        Some((PrimitiveShape::Quad, PaletteRole::RackShadow))
    );
    assert_eq!(
        AssetKind::FloorGrid.primitive(),
        None,
        "the raised access floor is a generated merged mesh"
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
fn design_validator_reports_empty_camera_coverage_intervals() {
    let errors = validation_errors(|scene| {
        scene.room.coverage = Vec2::splat(30.0);
    });

    for yaw_degrees in [45, 135, 225, 315] {
        assert!(
            errors.contains(&SceneValidationError::EmptyCameraCoverageInterval { yaw_degrees }),
            "coverage narrower than the footprint must be reported at yaw {yaw_degrees}"
        );
    }
}

#[test]
fn design_validator_checks_mid_orbit_camera_coverage_intervals() {
    let errors = validation_errors(|scene| {
        scene.room.coverage = Vec2::new(25.0, 72.0);
    });

    assert!(errors.contains(&SceneValidationError::EmptyCameraCoverageInterval { yaw_degrees: 0 }));
    assert!(
        errors.contains(&SceneValidationError::EmptyCameraCoverageInterval { yaw_degrees: 180 })
    );
}

#[test]
fn design_validator_rejects_coverage_that_cannot_follow_the_whole_walkable_room() {
    // 60 m of coverage still holds the footprint at every yaw, so it passes the
    // old non-empty test, but it cannot follow a technician standing against a
    // wall of the 40 m room. That is the property the rule now enforces.
    let errors = validation_errors(|scene| {
        scene.room.coverage = Vec2::splat(60.0);
    });

    for yaw_degrees in [0, 45, 90, 135, 180, 225, 270, 315] {
        assert!(
            camera_target_bounds(Vec2::splat(60.0), (yaw_degrees as f32).to_radians()).is_some(),
            "60 m of coverage still has a non-empty legal rectangle at yaw {yaw_degrees}"
        );
        assert!(
            errors.contains(&SceneValidationError::RoomOutsideCameraCoverage { yaw_degrees }),
            "yaw {yaw_degrees} must report the room falling outside the coverage"
        );
    }
    assert!(
        !errors.iter().any(|error| matches!(
            error,
            SceneValidationError::EmptyCameraCoverageInterval { .. }
        )),
        "60 m of coverage is not empty, it is merely too small to follow the room"
    );
}

#[test]
fn design_validator_rejects_a_room_that_outgrows_the_authored_coverage() {
    let errors = validation_errors(|scene| {
        scene.room.size = Vec2::splat(60.0);
    });

    assert!(
        errors.iter().any(|error| matches!(
            error,
            SceneValidationError::RoomOutsideCameraCoverage { .. }
        )),
        "a room grown past the 72 m apron must be reported, got {errors:?}"
    );
}

#[test]
fn design_validator_requires_a_visual_only_apron_over_the_rendered_area() {
    let missing = validation_errors(|scene| {
        scene
            .visuals
            .retain(|visual| visual.asset != AssetKind::RenderApron);
    });
    assert_eq!(missing, [SceneValidationError::MissingRenderApron]);

    let too_small = validation_errors(|scene| {
        for visual in &mut scene.visuals {
            if visual.asset == AssetKind::RenderApron {
                visual.transform.scale = Vec3::new(ROOM_SIZE.x, 1.0, ROOM_SIZE.y);
            }
        }
    });
    assert_eq!(
        too_small,
        [SceneValidationError::RenderApronDoesNotCoverRenderedArea]
    );

    let coplanar = validation_errors(|scene| {
        for visual in &mut scene.visuals {
            if visual.asset == AssetKind::RenderApron {
                visual.transform.translation.y = 0.0;
            }
        }
    });
    assert_eq!(
        coplanar,
        [SceneValidationError::RenderApronDoesNotCoverRenderedArea],
        "an apron coplanar with the floor would z-fight, so it is not a legal apron"
    );

    let collidable = validation_errors(|scene| {
        for visual in &mut scene.visuals {
            if visual.asset == AssetKind::RenderApron {
                visual.collision_required = true;
            }
        }
        scene.colliders.push(ColliderSpec {
            id: prop("render-apron"),
            center: Vec2::ZERO,
            half_extents: RENDER_COVERAGE_SIZE * 0.5,
        });
    });
    assert!(
        collidable.contains(&SceneValidationError::RenderApronHasCollider(prop(
            "render-apron"
        ))),
        "the apron is background, not a second room: {collidable:?}"
    );
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
        assert_eq!(source.modules.len(), 3);
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
    assert_eq!(primitives.len(), 18);

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
    assert_eq!(modules.len(), 17);

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

/// One captured rig part: its declared name, its live transform, and its rest.
type PartSample = (String, Transform, Transform);

/// Every clip change observed, with the part transforms as they stood *after*
/// the transition ran and *before* the animation player posed the new clip.
#[derive(Resource, Default)]
struct TransitionProbe(Vec<(PlayerClip, Vec<PartSample>)>);

fn capture_transition_probe(
    state: Res<PlayerAnimationState>,
    parts: Res<PlayerParts>,
    transforms: Query<&Transform>,
    mut probe: ResMut<TransitionProbe>,
) {
    if !state.is_changed() {
        return;
    }
    let captured = parts
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
    probe.0.push((state.current(), captured));
}

/// Installs [`capture_transition_probe`] immediately after the animation
/// transition system, inside the same set.
fn watch_clip_transitions(app: &mut App) {
    app.init_resource::<TransitionProbe>().add_systems(
        Update,
        capture_transition_probe
            .in_set(CellShiftSet::UpdateAnimation)
            .after(update_player_animation),
    );
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
    viewport_of_world(app, Vec3::new(ground.x, 0.0, ground.y))
}

/// Viewport position of any world point, through the real Bevy projection.
fn viewport_of_world(app: &mut App, world: Vec3) -> Vec2 {
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
        .world_to_viewport(&global, world)
        .unwrap_or_else(|error| panic!("{world:?} did not project: {error:?}"))
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
    world_framing_margin(app, Vec3::new(ground.x, 0.0, ground.y))
}

/// Smallest distance from any projected world point to any viewport edge.
/// Negative when the point is off screen.
fn world_framing_margin(app: &mut App, world: Vec3) -> f32 {
    let size = viewport_size(app);
    let point = viewport_of_world(app, world);
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
    // whole ground footprint inside the rendered-coverage apron.
    let coverage = RENDER_COVERAGE_SIZE * 0.5;
    for corner in ground_quadrilateral(orbit(&app).yaw_radians(), focus) {
        assert!(
            corner.x.abs() <= coverage.x + 1.0e-3 && corner.y.abs() <= coverage.y + 1.0e-3,
            "the initial view already leaks past the rendered apron at {corner:?}"
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

/// Ground positions the camera must frame. The technician can stand anywhere in
/// the walkable room, so this is the room centre, the authored player spawn,
/// every aisle end, and every room corner as close as a technician of
/// [`PLAYER_RADIUS`] can actually stand to it. Movement clamps the technician
/// off the wall itself, so the wall corner is not a legal standing position.
fn framing_samples() -> Vec<Vec2> {
    let mut samples = vec![Vec2::ZERO, SceneBlueprint::v0().player_spawn];
    for center_x in AISLE_CENTER_X {
        samples.push(Vec2::new(center_x, AISLE_Z_MIN));
        samples.push(Vec2::new(center_x, AISLE_Z_MAX));
        samples.push(Vec2::new(center_x, 0.0));
    }
    samples.extend(reachable_room_corners());
    samples
}

/// The four corners of a rectangle given its half extents, in a stable order.
fn room_corners(half: Vec2) -> [Vec2; 4] {
    [
        half,
        -half,
        Vec2::new(half.x, -half.y),
        Vec2::new(-half.x, half.y),
    ]
}

/// The closest a technician can stand to each corner of the walkable room.
fn reachable_room_corners() -> [Vec2; 4] {
    room_corners(ROOM_SIZE * 0.5 - Vec2::splat(PLAYER_RADIUS))
}

// ---------------------------------------------------------------------------
// The technician's real spatial envelope
// ---------------------------------------------------------------------------

/// The space the generated technician actually occupies, relative to its ground
/// origin, in metres. Framing the ground origin says nothing about whether the
/// body is on screen, so the framing gate is measured against this instead.
#[derive(Clone, Copy, Debug)]
struct TechnicianEnvelope {
    /// Largest horizontal distance from the ground origin to any skinned corner
    /// of any authored shape. Taken as a radius so it holds at every facing.
    radius: f32,
    /// Lowest skinned corner.
    min_y: f32,
    /// Highest skinned corner, which is what a head-height framing gate needs.
    max_y: f32,
}

impl TechnicianEnvelope {
    /// The eight corners of the axis-aligned box containing the whole envelope
    /// around `ground`. The projection is orthographic, hence affine, so the
    /// extreme screen point of a convex body is a vertex of any box holding it.
    fn corners(self, ground: Vec2) -> [Vec3; 8] {
        let mut corners = [Vec3::ZERO; 8];
        let mut index = 0;
        for x in [-self.radius, self.radius] {
            for y in [self.min_y, self.max_y] {
                for z in [-self.radius, self.radius] {
                    corners[index] = Vec3::new(ground.x + x, y, ground.y + z);
                    index += 1;
                }
            }
        }
        corners
    }
}

fn triple(value: [f64; 3]) -> Vec3 {
    Vec3::new(value[0] as f32, value[1] as f32, value[2] as f32)
}

/// The exact quaternion `assetgen::push_animations` writes for an extrinsic XYZ
/// Euler keyframe, restated here so the test does not inherit the generator's
/// arithmetic by calling it.
fn euler_degrees_to_quat(euler: [f64; 3]) -> Quat {
    let (sin_x, cos_x) = (euler[0].to_radians() * 0.5).sin_cos();
    let (sin_y, cos_y) = (euler[1].to_radians() * 0.5).sin_cos();
    let (sin_z, cos_z) = (euler[2].to_radians() * 0.5).sin_cos();
    Quat::from_xyzw(
        (sin_x * cos_y * cos_z + cos_x * sin_y * sin_z) as f32,
        (cos_x * sin_y * cos_z - sin_x * cos_y * sin_z) as f32,
        (cos_x * cos_y * sin_z + sin_x * sin_y * cos_z) as f32,
        (cos_x * cos_y * cos_z - sin_x * sin_y * sin_z) as f32,
    )
    .normalize()
}

/// The keyframe pair straddling `time`, and the interpolant between them.
/// glTF samplers clamp outside the key range, which is what the ends return.
fn key_span(keys: &[KeySource], time: f64) -> (usize, usize, f32) {
    assert!(!keys.is_empty(), "every track carries keyframes");
    if time <= keys[0].time {
        return (0, 0, 0.0);
    }
    match keys.iter().position(|key| key.time >= time) {
        Some(hi) if hi > 0 => {
            let lo = hi - 1;
            let span = keys[hi].time - keys[lo].time;
            let fraction = if span > 0.0 {
                (time - keys[lo].time) / span
            } else {
                0.0
            };
            (lo, hi, fraction as f32)
        }
        _ => (keys.len() - 1, keys.len() - 1, 0.0),
    }
}

/// Local translation and rotation of every bone in the authored rest pose.
/// Rest rotations are identity by construction.
fn rest_pose(rig: &RigSource) -> Vec<(Vec3, Quat)> {
    rig.bones
        .iter()
        .map(|bone| (triple(bone.translation), Quat::IDENTITY))
        .collect()
}

/// Local translation and rotation of every bone at `time` of `clip`.
/// `Translation` keys are offsets from the rest translation, exactly as
/// `assetgen::push_animations` bakes them.
fn clip_pose(rig: &RigSource, clip: &ClipSource, time: f64) -> Vec<(Vec3, Quat)> {
    let mut pose = rest_pose(rig);
    for track in &clip.tracks {
        let index = bone_index(rig, &track.bone);
        let (lo, hi, fraction) = key_span(&track.keys, time);
        match track.channel {
            ChannelSource::Translation => {
                let offset =
                    triple(track.keys[lo].value).lerp(triple(track.keys[hi].value), fraction);
                pose[index].0 = triple(rig.bones[index].translation) + offset;
            }
            ChannelSource::Rotation => {
                let from = euler_degrees_to_quat(track.keys[lo].value);
                let to = euler_degrees_to_quat(track.keys[hi].value);
                pose[index].1 = from.slerp(to, fraction);
            }
        }
    }
    pose
}

fn bone_index(rig: &RigSource, name: &str) -> usize {
    rig.bones
        .iter()
        .position(|bone| bone.name == name)
        .unwrap_or_else(|| panic!("{name} is not a declared bone"))
}

/// Rest-pose world origin of every bone, which is what the inverse bind
/// matrices carry.
fn rest_bone_origins(rig: &RigSource) -> Vec<Vec3> {
    let mut origins = Vec::with_capacity(rig.bones.len());
    for bone in &rig.bones {
        let local = triple(bone.translation);
        let origin = match &bone.parent {
            None => local,
            Some(parent) => origins[bone_index(rig, parent)] + local,
        };
        origins.push(origin);
    }
    origins
}

/// Widens `envelope` by every rigidly skinned shape corner of `pose`.
fn accumulate_pose(
    module: &ModuleSource,
    rig: &RigSource,
    rest: &[Vec3],
    pose: &[(Vec3, Quat)],
    envelope: &mut TechnicianEnvelope,
) {
    let mut globals: Vec<Mat4> = Vec::with_capacity(rig.bones.len());
    for (index, bone) in rig.bones.iter().enumerate() {
        let local = Mat4::from_rotation_translation(pose[index].1, pose[index].0);
        let global = match &bone.parent {
            None => local,
            Some(parent) => globals[bone_index(rig, parent)] * local,
        };
        globals.push(global);
    }

    for shape in &module.shapes {
        assert!(
            shape.repeat.is_empty(),
            "{} repeats; the envelope would miss its instances",
            shape.name
        );
        let bone = shape
            .bone
            .as_ref()
            .unwrap_or_else(|| panic!("{} is not skinned to a bone", shape.name));
        let index = bone_index(rig, bone);
        // Rigid skinning: the inverse bind matrix is the rest origin, so a rest
        // shape corner rides its bone from the rest pose to the animated one.
        let skin = globals[index] * Mat4::from_translation(-rest[index]);

        let (center, half) = match shape.primitive {
            PrimitiveSource::Box {
                center,
                half_extents,
            } => (triple(center), triple(half_extents)),
            PrimitiveSource::Cylinder {
                center,
                radius,
                half_height,
                axis,
                ..
            } => {
                let radius = radius as f32;
                let half_height = half_height as f32;
                let half = match axis {
                    Axis::X => Vec3::new(half_height, radius, radius),
                    Axis::Y => Vec3::new(radius, half_height, radius),
                    Axis::Z => Vec3::new(radius, radius, half_height),
                };
                (triple(center), half)
            }
        };
        // The cel outline is an inverted hull expanded by this much, so it is
        // part of what the camera actually has to frame.
        let half = half + Vec3::splat(shape.outline.unwrap_or(0.0) as f32);

        for x in [-1.0_f32, 1.0] {
            for y in [-1.0_f32, 1.0] {
                for z in [-1.0_f32, 1.0] {
                    let point = skin.transform_point3(center + half * Vec3::new(x, y, z));
                    envelope.radius = envelope.radius.max(point.x.hypot(point.z));
                    envelope.min_y = envelope.min_y.min(point.y);
                    envelope.max_y = envelope.max_y.max(point.y);
                }
            }
        }
    }
}

/// The union of the generated rest pose and every pose of every generated clip,
/// so the framing gate is measured against the widest the technician ever gets.
fn technician_envelope() -> TechnicianEnvelope {
    let source = load_source(&repo_root(), "technician").expect("the technician source parses");
    let module = source
        .modules
        .iter()
        .find(|module| module.name == "technician")
        .expect("the technician module exists");
    let rig = module.rig.as_ref().expect("the technician is rigged");
    assert_eq!(rig.bones.len(), TECHNICIAN_BONES.len());
    assert_eq!(rig.clips.len(), PlayerClip::ALL.len());

    let rest = rest_bone_origins(rig);
    let mut envelope = TechnicianEnvelope {
        radius: 0.0,
        min_y: f32::MAX,
        max_y: f32::MIN,
    };
    accumulate_pose(module, rig, &rest, &rest_pose(rig), &mut envelope);

    for clip in &rig.clips {
        let mut times = clip
            .tracks
            .iter()
            .flat_map(|track| track.keys.iter().map(|key| key.time))
            .collect::<Vec<_>>();
        // Linear samplers put the extremes on the keyframes, but sampling
        // between them proves no interpolated pose escapes the envelope.
        times.extend((0..=240).map(|step| clip.duration * f64::from(step) / 240.0));
        for time in times {
            accumulate_pose(
                module,
                rig,
                &rest,
                &clip_pose(rig, clip, time),
                &mut envelope,
            );
        }
    }
    envelope
}

/// Drives the camera to an exact tween sample of the turn that leaves
/// `heading` in the direction `key` names.
///
/// `frames` is the number of fixed steps of the tween that have elapsed when
/// this returns, so `elapsed == frames * FIXED_STEP` exactly and
/// `frames == QUARTER_TURN_FRAMES / 2` is the true halfway point. [`tap`] runs
/// the first of those frames itself -- the press is read in `ReadInput` and the
/// tween advances in `UpdateOrbitIntent` of that same frame -- so the pump is
/// one frame shorter and there is no trailing `app.update()` to over-count.
fn orbit_towards(app: &mut App, heading: CameraHeading, key: KeyCode, frames: usize) {
    app.world_mut()
        .insert_resource(CameraOrbit::settled(heading));
    if frames == 0 {
        app.update();
        return;
    }
    tap(app, &[key]);
    pump(app, frames - 1);
}

/// Drives the camera clockwise off `heading` for exactly `frames` tween steps.
fn orbit_to(app: &mut App, heading: CameraHeading, frames: usize) {
    orbit_towards(app, heading, KeyCode::KeyE, frames);
}

/// Raw, un-eased progress through the current turn.
fn tween_fraction(orbit: CameraOrbit) -> f32 {
    let duration = orbit.duration_seconds();
    assert!(duration > 0.0, "a settled orbit has no tween fraction");
    (duration - orbit.remaining_seconds()) / duration
}

/// Yaw of the real camera entity, recovered from its transform.
fn camera_yaw_degrees(app: &mut App) -> f32 {
    let target = camera_ground_target(app);
    let (transform, _) = camera_placement(app);
    let offset = transform.translation - Vec3::new(target.x, 0.0, target.y);
    offset.x.atan2(offset.z).to_degrees().rem_euclid(360.0)
}

/// Signed shortest difference between two yaws, in degrees.
fn yaw_difference_degrees(actual: f32, expected: f32) -> f32 {
    (actual - expected + 540.0).rem_euclid(360.0) - 180.0
}

/// Stops the virtual clock so a batch of samples is evaluated at one immutable
/// orbit state. `app.update()` otherwise advances the tween between samples,
/// which lets a requested mid-tween yaw settle part way through a loop.
fn freeze_time(app: &mut App) {
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
}

/// Restores the fixed test step after [`freeze_time`].
fn resume_time(app: &mut App) {
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
        FIXED_STEP,
    )));
}

#[test]
fn camera_orbit_clamps_the_follow_target_and_keeps_the_technician_framed() {
    let mut app = walking_hall(&repo_assets());
    let envelope = technician_envelope();
    let coverage = RENDER_COVERAGE_SIZE * 0.5;
    let half_room = ROOM_SIZE * 0.5;

    for heading in CameraHeading::ALL {
        // A settled heading, then the exact midpoint of the turn that leaves it.
        for frames in [0, QUARTER_TURN_FRAMES / 2, QUARTER_TURN_FRAMES / 4] {
            orbit_to(&mut app, heading, frames);
            // Every sample below runs a real frame, and a real frame would
            // otherwise advance the tween and settle it part way through the
            // batch. Freeze the clock so the whole batch is one orbit state.
            freeze_time(&mut app);
            let frozen = orbit(&app);
            let yaw = frozen.yaw_radians();
            let bounds = camera_target_bounds(RENDER_COVERAGE_SIZE, yaw);
            let (min, max) = bounds.unwrap_or_else(|| {
                panic!("yaw {} has no legal target rectangle", yaw.to_degrees())
            });
            assert!(
                max.x >= half_room.x && max.y >= half_room.y,
                "yaw {} shrank the legal rectangle to {min:?}..{max:?}, which no longer holds the {half_room:?} room",
                yaw.to_degrees()
            );

            for sample in framing_samples() {
                place_player(&mut app, sample);
                app.update();
                assert_eq!(
                    orbit(&app),
                    frozen,
                    "the orbit drifted mid-batch: every sample of this batch must be measured at yaw {}",
                    yaw.to_degrees()
                );
                assert_eq!(
                    orbit(&app).yaw_radians().to_bits(),
                    yaw.to_bits(),
                    "the sampled yaw must be bit-identical across the whole batch"
                );
                let target = camera_ground_target(&mut app);
                let expected = clamp_follow_target(sample, RENDER_COVERAGE_SIZE, yaw);
                assert!(
                    (target - expected).abs().max_element() < 1.0e-3,
                    "yaw {} following {sample:?} settled on {target:?}, expected {expected:?}",
                    yaw.to_degrees()
                );
                assert!(
                    (target - sample).abs().max_element() < 1.0e-3,
                    "yaw {} must follow the legal position {sample:?} exactly, got {target:?}",
                    yaw.to_degrees()
                );

                for corner in ground_quadrilateral(yaw, target) {
                    assert!(
                        corner.x.abs() <= coverage.x + 1.0e-3
                            && corner.y.abs() <= coverage.y + 1.0e-3,
                        "yaw {} following {sample:?} pushed a ground corner to {corner:?}, outside the rendered apron",
                        yaw.to_degrees()
                    );
                }

                // Not the ground origin: the whole body, hard hat included.
                for point in envelope.corners(sample) {
                    let margin = world_framing_margin(&mut app, point);
                    assert!(
                        margin >= FRAMING_MARGIN_PIXELS,
                        "yaw {} framed the technician envelope point {point:?} at {sample:?} with only {margin} px of margin",
                        yaw.to_degrees()
                    );
                }
            }
            resume_time(&mut app);
        }
    }
}

#[test]
fn camera_orbit_frames_every_room_corner_with_the_reviewed_margin() {
    let mut app = walking_hall(&repo_assets());
    let envelope = technician_envelope();
    let size = viewport_size(&mut app);
    let centre_margin = size.x.min(size.y) * 0.5;

    // Requirement 7, positively: every corner of the walkable room, at every
    // settled heading and at two tween samples of the turn that leaves it,
    // framed with at least 32 logical pixels of viewport margin. Movement keeps
    // the technician a radius off the wall, so the followed position is the
    // reachable corner and the wall corner itself is checked as a framed point.
    for (corner, wall) in reachable_room_corners()
        .into_iter()
        .zip(room_corners(ROOM_SIZE * 0.5))
    {
        for heading in CameraHeading::ALL {
            for frames in [0, QUARTER_TURN_FRAMES / 2, QUARTER_TURN_FRAMES / 4] {
                place_player(&mut app, corner);
                orbit_to(&mut app, heading, frames);
                let yaw = orbit(&app).yaw_radians();
                let settled = CameraHeading::from_yaw_degrees(yaw.to_degrees());
                if frames == 0 {
                    assert_eq!(
                        settled,
                        Some(heading),
                        "frames 0 must be the settled {heading:?}"
                    );
                } else {
                    assert_eq!(
                        settled,
                        None,
                        "frames {frames} must sample a yaw between headings, got {} degrees",
                        yaw.to_degrees()
                    );
                }

                // The framing contract is the whole body at both corners, not a
                // ground point: the hard hat, the boots, and the swung wrench
                // all have to stay 32 px inside the viewport.
                for point in [corner, wall]
                    .into_iter()
                    .flat_map(|ground| envelope.corners(ground))
                {
                    let margin = world_framing_margin(&mut app, point);
                    assert!(
                        margin >= FRAMING_MARGIN_PIXELS,
                        "yaw {} framed room corner envelope point {point:?} with only {margin} px of margin",
                        yaw.to_degrees()
                    );
                }

                // The apron makes the corner reachable by the camera itself, so
                // the technician is not merely on screen: it is centred, with
                // the full half-viewport of margin a followed point gets.
                let projected = viewport_of(&mut app, corner);
                assert!(
                    (projected - size * 0.5).abs().max_element() < 0.5,
                    "yaw {} should centre room corner {corner:?}, projected to {projected:?}",
                    yaw.to_degrees()
                );
                let margin = framing_margin(&mut app, corner);
                assert!(
                    (margin - centre_margin).abs() < 1.0,
                    "a centred corner should keep the full {centre_margin} px of margin, got {margin}"
                );
            }
        }
    }
}

#[test]
fn camera_framing_margin_is_calibrated_against_the_real_pixel_to_world_scale() {
    let mut app = camera_app(&repo_assets());
    let size = viewport_size(&mut app);

    // One independent calibration of the whole pixel-to-world chain. If the
    // zoom, the viewport, the elevation, the margin constant, or the helper's
    // edge arithmetic drifts, the measured margin stops being 32 px, because
    // these offsets are derived from the reviewed numbers and not from the
    // projection they are checked against.
    let pixels_per_metre = size.x / ORTHOGRAPHIC_WIDTH;
    assert!(
        (size.y / ORTHOGRAPHIC_HEIGHT - pixels_per_metre).abs() < 1.0e-3,
        "the reviewed rectangle must map to square pixels, got {} by {}",
        pixels_per_metre,
        size.y / ORTHOGRAPHIC_HEIGHT
    );
    assert!(
        (pixels_per_metre - 49.230_77).abs() < 1.0e-3,
        "1280 px over 26 m is 49.23077 px/m, got {pixels_per_metre}"
    );

    // Screen horizontal is world horizontal; screen vertical is foreshortened
    // by the fixed elevation, which is why the two offsets differ.
    let across = (size.x * 0.5 - FRAMING_MARGIN_PIXELS) / pixels_per_metre;
    let along = (size.y * 0.5 - FRAMING_MARGIN_PIXELS)
        / (pixels_per_metre * CAMERA_ELEVATION_DEGREES.to_radians().sin());
    assert!(
        (across - 12.35).abs() < 1.0e-3,
        "608 px of half viewport is 12.35 m across, got {across}"
    );
    assert!(
        (along - 7.944_1).abs() < 1.0e-3,
        "328 px of half viewport is 7.9441 m along the ground, got {along}"
    );

    for heading in CameraHeading::ALL {
        for frames in [0, QUARTER_TURN_FRAMES / 2] {
            orbit_to(&mut app, heading, frames);
            freeze_time(&mut app);
            let yaw = orbit(&app).yaw_radians();
            let basis = ViewBasis::from_yaw_radians(yaw);
            let target = camera_ground_target(&mut app);

            for (label, offset) in [
                ("right", basis.right() * across),
                ("left", -basis.right() * across),
                ("up", basis.forward() * along),
                ("down", -basis.forward() * along),
            ] {
                let point = target + offset;
                let margin = framing_margin(&mut app, point);
                assert!(
                    (margin - FRAMING_MARGIN_PIXELS).abs() < 0.05,
                    "yaw {} calibration point {label} must project exactly {FRAMING_MARGIN_PIXELS} px inside the edge, got {margin}",
                    yaw.to_degrees()
                );
            }

            // A hair further out and the same point is outside the gate, so the
            // calibration is a knife edge rather than a comfortable pass.
            let outside = target + basis.right() * (across + 1.0 / pixels_per_metre);
            let margin = framing_margin(&mut app, outside);
            assert!(
                margin < FRAMING_MARGIN_PIXELS,
                "one metre-pixel past the calibration point must fail the gate, got {margin}"
            );
            resume_time(&mut app);
        }
    }
}

#[test]
fn camera_frames_the_generated_technician_envelope_it_measures() {
    // The envelope the framing gates use is derived from the generated source,
    // so it has to agree with the rig the running app actually loaded, and it
    // has to be a body rather than a point.
    let envelope = technician_envelope();
    let source = load_source(&repo_root(), "technician").expect("the technician source parses");
    let rig = source.modules[0]
        .rig
        .as_ref()
        .expect("the technician is rigged");

    let mut app = walking_hall(&repo_assets());
    let transform = technician_transform(&mut app);
    assert_eq!(
        transform.scale,
        Vec3::ONE,
        "a scaled technician would invalidate the measured envelope"
    );
    for (name, live, rest) in part_transforms(&mut app) {
        let Some(bone) = rig.bones.iter().find(|bone| bone.name == name) else {
            continue;
        };
        assert!(
            (rest.translation - triple(bone.translation))
                .abs()
                .max_element()
                < 1.0e-5,
            "{name} loaded at rest {:?}, but the source authors {:?}",
            rest.translation,
            triple(bone.translation)
        );
        assert!(live.translation.is_finite());
    }

    // Rest alone is not the widest pose: Repair swings the wrench arm back.
    let rest_only = {
        let module = &source.modules[0];
        let rest = rest_bone_origins(rig);
        let mut only = TechnicianEnvelope {
            radius: 0.0,
            min_y: f32::MAX,
            max_y: f32::MIN,
        };
        accumulate_pose(module, rig, &rest, &rest_pose(rig), &mut only);
        only
    };
    assert!(
        envelope.radius > rest_only.radius + 0.1,
        "the animated envelope must be wider than the rest pose, {} vs {}",
        envelope.radius,
        rest_only.radius
    );
    // Pinned numerically, so dropping a clip, an outline hull, a bone rotation,
    // or the height axis changes a measured number rather than passing quietly.
    for (label, measured, expected) in [
        ("rest radius", rest_only.radius, 0.311_004_9_f32),
        ("rest floor", rest_only.min_y, -0.012),
        ("rest crown", rest_only.max_y, 1.944),
        ("animated radius", envelope.radius, 0.799_812_1),
        ("animated floor", envelope.min_y, -0.042),
        ("animated crown", envelope.max_y, 1.970_412_4),
    ] {
        assert!(
            (measured - expected).abs() < 1.0e-4,
            "the {label} of the generated technician is {expected} m, measured {measured}"
        );
    }
    assert!(
        envelope.max_y > 1.9,
        "the envelope must include height, not just a ground footprint"
    );
}

#[test]
fn camera_orbit_tween_samples_land_on_the_exact_requested_fraction() {
    let mut app = camera_app(&repo_assets());

    // The halfway sample really is halfway, in every direction the orbit can
    // turn, including both wraparounds through zero. `smoothstep(0.5) == 0.5`,
    // so the eased yaw at the midpoint is the plain arithmetic midpoint.
    let cases = [
        (
            CameraHeading::NorthEast,
            KeyCode::KeyE,
            CameraHeading::SouthEast,
            90.0_f32,
        ),
        (
            CameraHeading::SouthEast,
            KeyCode::KeyQ,
            CameraHeading::NorthEast,
            90.0,
        ),
        (
            CameraHeading::SouthWest,
            KeyCode::KeyE,
            CameraHeading::NorthWest,
            270.0,
        ),
        // 315 -> 405 wraps through zero going clockwise.
        (
            CameraHeading::NorthWest,
            KeyCode::KeyE,
            CameraHeading::NorthEast,
            0.0,
        ),
        // 45 -> -45 wraps through zero going counter-clockwise.
        (
            CameraHeading::NorthEast,
            KeyCode::KeyQ,
            CameraHeading::NorthWest,
            0.0,
        ),
    ];

    for (start, key, target, halfway) in cases {
        orbit_towards(&mut app, start, key, QUARTER_TURN_FRAMES / 2);
        let sampled = orbit(&app);

        assert_eq!(sampled.heading(), target, "{start:?} + {key:?}");
        assert!(
            !sampled.is_settled(),
            "{start:?} + {key:?} settled before the midpoint"
        );
        let fraction = tween_fraction(sampled);
        assert!(
            (fraction - 0.5).abs() < 1.0e-4,
            "{start:?} + {key:?} sampled elapsed/duration {fraction}, not 0.5"
        );
        assert!(
            (sampled.progress() - 0.5).abs() < 1.0e-4,
            "smoothstep(0.5) must be 0.5, got {}",
            sampled.progress()
        );
        let drift = yaw_difference_degrees(sampled.yaw_degrees(), halfway);
        assert!(
            drift.abs() < 1.0e-2,
            "{start:?} + {key:?} should sit at {halfway} degrees, got {} ({drift} off)",
            sampled.yaw_degrees()
        );
        // The real camera entity, not just the resource that drives it.
        let camera_drift = yaw_difference_degrees(camera_yaw_degrees(&mut app), halfway);
        assert!(
            camera_drift.abs() < 1.0e-2,
            "{start:?} + {key:?} left the camera entity at {} degrees, not {halfway}",
            camera_yaw_degrees(&mut app)
        );

        // One frame more is 10/18 of the turn, which is the 0.556 the helper
        // used to advertise as its midpoint.
        pump(&mut app, 1);
        let late = tween_fraction(orbit(&app));
        assert!(
            (late - 10.0 / 18.0).abs() < 1.0e-3,
            "one frame past the midpoint should be 0.5556, got {late}"
        );
        assert!(
            (late - 0.5).abs() > 1.0e-2,
            "an off-by-one frame must be visible in the sampled fraction"
        );
    }

    // Every requested sample is exactly `frames / QUARTER_TURN_FRAMES` of the
    // turn, and a whole quarter turn settles exactly.
    for frames in [1_usize, 4, 6, 9, 12, 17] {
        orbit_towards(&mut app, CameraHeading::NorthEast, KeyCode::KeyE, frames);
        let sampled = tween_fraction(orbit(&app));
        let expected = frames as f32 / QUARTER_TURN_FRAMES as f32;
        assert!(
            (sampled - expected).abs() < 1.0e-4,
            "{frames} of {QUARTER_TURN_FRAMES} frames should be {expected} of the turn, got {sampled}"
        );
    }
    orbit_towards(
        &mut app,
        CameraHeading::NorthEast,
        KeyCode::KeyE,
        QUARTER_TURN_FRAMES,
    );
    assert!(
        orbit(&app).is_settled(),
        "{QUARTER_TURN_FRAMES} frames is exactly one settled quarter turn"
    );
    assert_eq!(orbit(&app).heading(), CameraHeading::SouthEast);
}

/// Ground position the test harness pins the technician to, after movement has
/// run and before the camera follows it.
#[derive(Resource, Clone, Copy, Debug)]
struct ForcedPosition(Vec2);

/// Places the technician wherever the test asks, between `MovePlayer` and
/// `FollowCamera`. `move_player` clamps to the walkable room every frame, so
/// this is the only way to hand the follow clamp a position outside it -- which
/// is the only kind of position the follow clamp ever actually engages for.
fn force_position(
    forced: Res<ForcedPosition>,
    mut technicians: Query<&mut Transform, With<Technician>>,
) {
    if let Ok(mut transform) = technicians.single_mut() {
        transform.translation.x = forced.0.x;
        transform.translation.z = forced.0.y;
    }
}

/// A blueprint that is identical to the authored hall except for its rendered
/// coverage, apron included, so it validates exactly as the authored one does.
fn blueprint_with_coverage(coverage: Vec2) -> SceneBlueprint {
    let mut blueprint = SceneBlueprint::v0();
    blueprint.room.coverage = coverage;
    let apron = blueprint
        .visuals
        .iter_mut()
        .find(|visual| visual.id.as_str() == "render-apron")
        .expect("the authored blueprint carries the apron");
    apron.transform.scale = Vec3::new(coverage.x, 1.0, coverage.y);
    assert_eq!(
        blueprint.validate(),
        Vec::<SceneValidationError>::new(),
        "the override must be a valid hall, not a broken one"
    );
    blueprint
}

#[test]
fn camera_follow_clamps_against_the_active_blueprint_coverage_not_the_constant() {
    let mut app = walking_hall(&repo_assets());
    app.insert_resource(ForcedPosition(Vec2::ZERO)).add_systems(
        Update,
        force_position
            .after(CellShiftSet::MovePlayer)
            .before(CellShiftSet::FollowCamera),
    );

    // Three halls: the authored one the camera falls back to when no blueprint
    // resource exists, and two valid overrides whose coverage is neither the
    // constant nor each other.
    let overrides = [Vec2::new(76.0, 76.0), Vec2::new(88.0, 80.0)];
    for coverage in overrides {
        assert_ne!(coverage, RENDER_COVERAGE_SIZE);
        assert!(coverage_holds_room(ROOM_SIZE, coverage, 0.0));
    }

    let probes = [
        Vec2::new(30.0, 30.0),
        Vec2::new(-30.0, 25.0),
        Vec2::new(21.0, -60.0),
        Vec2::new(19.65, 19.65),
    ];

    for active in [None, Some(overrides[0]), Some(overrides[1])] {
        match active {
            None => {
                app.world_mut().remove_resource::<HallBlueprint>();
            }
            Some(coverage) => {
                let blueprint = HallBlueprint(blueprint_with_coverage(coverage));
                assert_eq!(active_coverage(Some(&blueprint)), coverage);
                app.insert_resource(blueprint);
            }
        }
        let coverage = active.unwrap_or(RENDER_COVERAGE_SIZE);

        for heading in CameraHeading::ALL {
            for frames in [0, QUARTER_TURN_FRAMES / 2] {
                orbit_to(&mut app, heading, frames);
                freeze_time(&mut app);
                let frozen = orbit(&app);
                let yaw = frozen.yaw_radians();

                for probe in probes {
                    app.insert_resource(ForcedPosition(probe));
                    app.update();
                    assert_eq!(orbit(&app), frozen, "the orbit drifted mid-batch");
                    assert_eq!(
                        player_position(&mut app),
                        probe,
                        "the harness must hand the follow clamp the probe itself"
                    );

                    let target = camera_ground_target(&mut app);
                    let expected = clamp_follow_target(probe, coverage, yaw);
                    assert!(
                        (target - expected).abs().max_element() < 1.0e-3,
                        "yaw {} with {coverage:?} of coverage followed {probe:?} to {target:?}, expected {expected:?}",
                        yaw.to_degrees()
                    );

                    // And it is genuinely a different answer from the constant,
                    // so the assertion above cannot pass by coincidence.
                    let constant = clamp_follow_target(probe, RENDER_COVERAGE_SIZE, yaw);
                    if coverage != RENDER_COVERAGE_SIZE && probe.abs().max_element() > 25.0 {
                        assert!(
                            (target - constant).abs().max_element() > 1.0,
                            "yaw {} followed {probe:?} to {target:?}, which is what the {RENDER_COVERAGE_SIZE:?} constant would have produced",
                            yaw.to_degrees()
                        );
                    }
                }
                resume_time(&mut app);
            }
        }
    }

    // A legal standing position is followed exactly whatever the coverage is,
    // because every valid coverage holds the whole walkable room.
    app.insert_resource(ForcedPosition(Vec2::new(19.65, 19.65)));
    app.update();
    assert!(
        (camera_ground_target(&mut app) - Vec2::new(19.65, 19.65))
            .abs()
            .max_element()
            < 1.0e-3
    );
}

#[test]
fn camera_orbit_holds_the_rendered_apron_instead_of_leaking_past_it() {
    let mut app = walking_hall(&repo_assets());
    let coverage = RENDER_COVERAGE_SIZE * 0.5;
    let half_room = ROOM_SIZE * 0.5;
    let mut overhung_the_room = false;

    // Containment is now measured against the rendered coverage the apron
    // covers, not the walkable room. The camera is free to overhang the room --
    // it must, to centre a corner -- but never the apron.
    for heading in CameraHeading::ALL {
        for frames in [0, QUARTER_TURN_FRAMES / 2, QUARTER_TURN_FRAMES / 4] {
            for corner in room_corners(ROOM_SIZE * 0.5 - Vec2::splat(PLAYER_RADIUS)) {
                place_player(&mut app, corner);
                orbit_to(&mut app, heading, frames);
                let yaw = orbit(&app).yaw_radians();
                let target = camera_ground_target(&mut app);

                assert!(
                    (target - corner).abs().max_element() < 1.0e-3,
                    "yaw {} at corner {corner:?} should follow it exactly, got {target:?}",
                    yaw.to_degrees()
                );
                for ground in ground_quadrilateral(yaw, target) {
                    assert!(
                        ground.x.abs() <= coverage.x + 1.0e-3
                            && ground.y.abs() <= coverage.y + 1.0e-3,
                        "yaw {} at corner {corner:?} leaked past the apron to {ground:?}",
                        yaw.to_degrees()
                    );
                    overhung_the_room |=
                        ground.x.abs() > half_room.x || ground.y.abs() > half_room.y;
                }
            }
        }
    }

    assert!(
        overhung_the_room,
        "the apron is load bearing: a cornered technician must put ground outside the 40 m room on screen"
    );
    assert!(
        coverage_holds_room(ROOM_SIZE, RENDER_COVERAGE_SIZE, 0.0),
        "the authored apron must hold the whole walkable room"
    );
}

#[test]
fn camera_orbit_renders_the_apron_over_every_position_the_camera_can_reach() {
    // The apron is the thing that makes the relaxed containment rule sound: the
    // whole ground quadrilateral, at every legal player position and every yaw,
    // must land on authored apron geometry rather than on the clear colour.
    let scene = SceneBlueprint::v0();
    let apron = scene
        .visual("render-apron")
        .expect("the authored blueprint must carry the rendered-coverage apron");
    let apron_half = Vec2::new(apron.transform.scale.x, apron.transform.scale.z) * 0.5;
    assert!(apron_half.x >= RENDER_COVERAGE_SIZE.x * 0.5);
    assert!(apron_half.y >= RENDER_COVERAGE_SIZE.y * 0.5);
    assert!(scene.collider("render-apron").is_none());
    assert!(!apron.collision_required);
    assert_eq!(
        apron.transform.translation,
        Vec3::new(0.0, -RENDER_APRON_DROP, 0.0)
    );

    let reachable = ROOM_SIZE * 0.5;
    // The apron size is not arbitrary. Following a technician standing at a
    // room corner overhangs the room by up to `hypot(13, 8.71916)` m, so the
    // rendered coverage has to be at least `2 * (20 + 15.6532)` m.
    let widest = (ORTHOGRAPHIC_WIDTH * 0.5).hypot(ground_half_depth());
    assert!(
        (widest - 15.653_2).abs() < 1.0e-3,
        "the widest camera overhang should be 15.6532 m, got {widest}"
    );
    let required = 2.0 * (ROOM_SIZE.x * 0.5 + widest);
    assert!(
        (required - 71.306_5).abs() < 1.0e-2,
        "the rendered coverage must be at least 71.3065 m, got {required}"
    );
    assert!(
        RENDER_COVERAGE_SIZE.x >= required && RENDER_COVERAGE_SIZE.y >= required,
        "the authored {RENDER_COVERAGE_SIZE:?} apron must cover {required} m"
    );

    for step in 0..720 {
        let yaw = (step as f32 * 0.5).to_radians();
        for player in framing_samples()
            .into_iter()
            .chain(room_corners(reachable))
            .chain([Vec2::new(0.0, reachable.y), Vec2::new(reachable.x, 0.0)])
        {
            let target = clamp_follow_target(player, RENDER_COVERAGE_SIZE, yaw);
            assert_eq!(
                target,
                player,
                "yaw {} must follow the legal position {player:?}",
                yaw.to_degrees()
            );
            for ground in ground_quadrilateral(yaw, target) {
                assert!(
                    ground.x.abs() <= apron_half.x + 1.0e-3
                        && ground.y.abs() <= apron_half.y + 1.0e-3,
                    "yaw {} at {player:?} put ground {ground:?} outside the authored apron",
                    yaw.to_degrees()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Operations, ticket, and repair contracts
// ---------------------------------------------------------------------------

/// The pinned `(rack, severity)` prefix of the reviewed seed, derived
/// independently from ChaCha8 word order: rack is `next_u32() % 4`, severity is
/// `next_u32() % 2` with zero meaning Critical.
const SEEDED_FAULTS: [(usize, TicketSeverity); 12] = [
    (2, TicketSeverity::Critical),
    (1, TicketSeverity::Critical),
    (3, TicketSeverity::Critical),
    (1, TicketSeverity::Warning),
    (2, TicketSeverity::Critical),
    (0, TicketSeverity::Critical),
    (3, TicketSeverity::Critical),
    (1, TicketSeverity::Warning),
    (1, TicketSeverity::Warning),
    (3, TicketSeverity::Critical),
    (1, TicketSeverity::Warning),
    (1, TicketSeverity::Critical),
];

/// Fixed-step frames in each documented interval.
const FAULT_FRAMES: usize = 240;
const REPAIR_FRAMES: usize = 180;
const RESOLVED_FRAMES: usize = 120;
const COOLDOWN_FRAMES: usize = 480;

fn ticket_queue(app: &App) -> TicketQueue {
    app.world().resource::<TicketQueue>().clone()
}

fn scheduler(app: &App) -> FaultScheduler {
    app.world().resource::<FaultScheduler>().clone()
}

fn movement_lock(app: &App) -> MovementLock {
    *app.world().resource::<MovementLock>()
}

fn last_interaction(app: &App) -> LastInteraction {
    *app.world().resource::<LastInteraction>()
}

fn operations_tick(app: &App) -> u64 {
    app.world().resource::<OperationsClock>().tick()
}

fn roster(app: &App) -> RackRoster {
    app.world().resource::<RackRoster>().clone()
}

fn rack_ops(app: &App, rack: usize) -> RackOperations {
    let entry = roster(app)
        .get(rack)
        .cloned()
        .unwrap_or_else(|| panic!("rack {rack} must be on the roster"));
    app.world()
        .get::<RackOperations>(entry.entity)
        .unwrap_or_else(|| panic!("rack {rack} must carry operational state"))
        .clone()
}

fn rack_states(app: &App) -> Vec<RackState> {
    (0..roster(app).len())
        .map(|rack| rack_ops(app, rack).state())
        .collect()
}

/// A standing position beside one rack, just inside the repair range and clear
/// of every authored collider.
fn repair_spot(app: &App, rack: usize) -> Vec2 {
    let entry = roster(app)
        .get(rack)
        .cloned()
        .unwrap_or_else(|| panic!("rack {rack} must be on the roster"));
    Vec2::new(
        entry.center.x + entry.half_extents.x + PLAYER_RADIUS + 0.2,
        0.0,
    )
}

/// Presses and releases the real Space key across exactly one frame.
fn press_space(app: &mut App) {
    tap(app, &[REPAIR_KEY]);
}

/// Boots the walking hall, then resets the operations model to a pristine
/// origin.
///
/// The hall settles its assets and its rig against the wall clock, so the
/// scheduler would otherwise start each test part way through an interval. The
/// reset is the same fixed-seed origin the verification harness will use, and
/// nothing else about the running app changes.
fn operations_hall(assets: &Path) -> App {
    let mut app = walking_hall(assets);
    let roster = roster(&app);
    assert_eq!(roster.len(), 4, "the authored hall has four rack rows");
    for entry in roster.all() {
        *app.world_mut()
            .get_mut::<RackOperations>(entry.entity)
            .expect("every authored rack carries operational state") =
            RackOperations::new(entry.rack, entry.id.clone());
    }
    app.world_mut()
        .insert_resource(FaultScheduler::new(roster.len()));
    app.world_mut().insert_resource(TicketQueue::default());
    app.world_mut().insert_resource(OperationsClock::default());
    app.world_mut().insert_resource(MovementLock::default());
    app.world_mut().insert_resource(LastInteraction::default());
    hold(&mut app, &[]);
    app
}

#[test]
fn operations_attaches_state_to_every_authored_rack_by_prop_id() {
    let mut app = walking_hall(&repo_assets());
    let roster = roster(&app);
    let blueprint = SceneBlueprint::v0();

    let authored = blueprint
        .visuals
        .iter()
        .filter(|visual| visual.asset == RACK_ASSET_KIND)
        .map(|visual| visual.id.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        roster
            .all()
            .iter()
            .map(|entry| entry.id.as_str().to_owned())
            .collect::<Vec<_>>(),
        authored,
        "the roster is the authored rack list, in stable identifier order"
    );

    for (rack, entry) in roster.all().iter().enumerate() {
        assert_eq!(entry.rack, rack, "rack indices are stable and dense");

        // The joined collider really is the blueprint's, by stable PropId.
        let collider = blueprint
            .collider(entry.id.as_str())
            .expect("every rack row authors a collider");
        assert_eq!(entry.center, collider.center);
        assert_eq!(entry.half_extents, collider.half_extents);

        // The state hangs on the spawned HallProp itself.
        let prop = app
            .world()
            .get::<HallProp>(entry.entity)
            .expect("the roster points at the authored prop entity");
        assert_eq!(prop.id, entry.id);
        assert_eq!(prop.asset, RACK_ASSET_KIND);

        let state = rack_ops(&app, rack);
        assert_eq!(state.id, entry.id);
        assert_eq!(state.rack, rack);
    }

    // Nothing that is not a rack carries operational state.
    let attached = app
        .world_mut()
        .query::<&RackOperations>()
        .iter(app.world())
        .count();
    assert_eq!(attached, roster.len());
    assert_eq!(attached, 4);
    assert_eq!(scheduler(&app).racks(), 4);
    assert_eq!(scheduler(&app).rng().seed(), FAULT_SCHEDULER_SEED);
}

#[test]
fn operations_scheduler_opens_the_seeded_queue_on_the_exact_interval_ticks() {
    let mut app = operations_hall(&repo_assets());
    assert!(ticket_queue(&app).is_empty());

    pump(&mut app, FAULT_FRAMES - 1);
    assert_eq!(operations_tick(&app), 239);
    assert!(
        ticket_queue(&app).is_empty(),
        "one tick short of four seconds must not fault"
    );
    assert_eq!(scheduler(&app).rng().draws(), 0);

    for (index, (rack, severity)) in SEEDED_FAULTS.iter().copied().take(3).enumerate() {
        if index > 0 {
            pump(&mut app, FAULT_FRAMES - 1);
            assert_eq!(ticket_queue(&app).len(), index);
        }
        app.update();

        let queue = ticket_queue(&app);
        assert_eq!(queue.len(), index + 1, "fault {index} must be open");
        let ticket = queue
            .for_rack(rack)
            .unwrap_or_else(|| panic!("fault {index} belongs to rack {rack}"));
        assert_eq!(ticket.id, TicketId::new(index as u64 + 1));
        assert_eq!(ticket.severity, severity);
        assert_eq!(ticket.created_tick, (index as u64 + 1) * 240);
        assert_eq!(ticket.rack_id.as_str(), format!("rack-row-{:02}", rack + 1));
        assert_eq!(rack_ops(&app, rack).state(), RackState::Faulted);
        assert_eq!(rack_ops(&app, rack).ticket(), Some(ticket.id));
    }

    // Three simultaneous tickets, in global priority order, and every other
    // rack still healthy.
    let queue = ticket_queue(&app);
    assert!(queue.is_at_capacity());
    assert_eq!(
        queue
            .ordered()
            .iter()
            .map(|ticket| (ticket.rack, ticket.severity, ticket.created_tick))
            .collect::<Vec<_>>(),
        vec![
            (2, TicketSeverity::Critical, 240),
            (1, TicketSeverity::Critical, 480),
            (3, TicketSeverity::Critical, 720),
        ],
        "equal severities sort by creation tick"
    );
    assert_eq!(
        rack_states(&app),
        vec![
            RackState::Healthy,
            RackState::Faulted,
            RackState::Faulted,
            RackState::Faulted
        ]
    );
    assert_eq!(scheduler(&app).rng().draws(), 6, "two words per emission");

    // The fourth opportunity matures against a full queue: it pauses, reports
    // why, and never touches the seeded stream.
    pump(&mut app, FAULT_FRAMES);
    assert_eq!(operations_tick(&app), 960);
    assert_eq!(ticket_queue(&app).len(), 3);
    let paused = scheduler(&app);
    assert!(paused.is_armed());
    assert_eq!(
        paused.blocked(),
        Some(ScheduleBlock::AtCapacity { active: 3 })
    );
    assert_eq!(paused.rng().draws(), 6);
    assert_eq!(paused.capacity_pauses(), 1);
    assert_eq!(paused.emitted(), 3);

    pump(&mut app, FAULT_FRAMES * 3);
    let still_paused = scheduler(&app);
    assert_eq!(still_paused.rng().draws(), 6, "a full queue never rerolls");
    assert_eq!(still_paused.capacity_pauses(), 1);
    assert_eq!(ticket_queue(&app).len(), 3);
}

#[test]
fn operations_out_of_range_space_is_rejected_and_stays_observable() {
    let mut app = operations_hall(&repo_assets());
    pump(&mut app, FAULT_FRAMES * 3);
    assert_eq!(ticket_queue(&app).len(), 3);

    // The middle of the centre aisle is 2.2 m from both faulted inner rack
    // faces, which is outside the 1.5 m repair range.
    place_player(&mut app, Vec2::new(0.0, 0.0));
    hold(&mut app, &[]);
    app.update();
    let before = player_position(&mut app);

    press_space(&mut app);

    let last = last_interaction(&app);
    assert_eq!(last.presses, 1);
    assert_eq!(last.rejected, 1);
    assert_eq!(last.started, 0, "a rejection is never recorded as a start");
    match last.outcome {
        InteractionOutcome::OutOfRange {
            nearest_rack,
            nearest_distance,
        } => {
            assert_eq!(nearest_rack, Some(1));
            assert!(
                (nearest_distance - 2.2).abs() < 1.0e-5,
                "the nearest faulted rack face is 2.2 m away, got {nearest_distance}"
            );
            assert!(nearest_distance > REPAIR_INTERACTION_RANGE);
        }
        other => panic!("an out-of-range press must be reported, got {other:?}"),
    }

    // Nothing at all changed: no repair, no lock, no ticket movement.
    assert!(!movement_lock(&app).is_locked());
    assert_eq!(ticket_queue(&app).len(), 3);
    assert!(
        rack_states(&app)
            .iter()
            .all(|state| *state != RackState::Repairing)
    );
    assert_eq!(
        app.world().resource::<PlayerAnimationState>().current(),
        PlayerClip::Idle
    );
    assert_eq!(player_position(&mut app), before);

    // A press with no open ticket at all is a different, still explicit,
    // rejection.
    let mut empty = operations_hall(&repo_assets());
    press_space(&mut empty);
    let last = last_interaction(&empty);
    assert_eq!(last.outcome, InteractionOutcome::NoOpenTickets);
    assert_eq!(last.rejected, 1);
    assert_eq!(last.started, 0);
}

#[test]
fn operations_in_range_space_starts_the_repair_and_locks_movement_in_one_frame() {
    let mut app = operations_hall(&repo_assets());
    pump(&mut app, FAULT_FRAMES * 3);
    let queue = ticket_queue(&app);
    assert_eq!(queue.len(), 3);
    let target = queue.ordered()[0].clone();
    assert_eq!(target.rack, 2);
    assert_eq!(target.id, TicketId::new(1));

    // Walk there with the real arrow keys, never by writing a transform.
    let spot = repair_spot(&app, target.rack);
    let mut walked = 0usize;
    for waypoint in [Vec2::new(spot.x, -11.0), spot] {
        loop {
            let position = player_position(&mut app);
            if position.distance(waypoint) <= 0.1 {
                break;
            }
            let keys = keys_towards(&view_basis(&app), waypoint - position);
            hold(&mut app, keys);
            app.update();
            walked += 1;
            assert!(walked < 3_000, "the walk to rack {} stalled", target.rack);
        }
    }
    assert!(
        walked > 200,
        "the approach must be a real walk, got {walked}"
    );

    // Still holding the arrows, so the lock has to be what stops the walk.
    let approach = keys_towards(&view_basis(&app), Vec2::new(0.0, 1.0));
    hold(&mut app, approach);
    app.update();
    let before = player_position(&mut app);
    assert_eq!(rack_ops(&app, target.rack).state(), RackState::Faulted);

    press_space(&mut app);

    let last = last_interaction(&app);
    assert_eq!(
        last.outcome,
        InteractionOutcome::Started {
            ticket: target.id,
            rack: target.rack
        }
    );
    assert_eq!(last.started, 1);
    assert_eq!(last.rejected, 0);

    // The very frame the repair starts: locked, posed, and standing still.
    let rack = rack_ops(&app, target.rack);
    assert_eq!(rack.state(), RackState::Repairing);
    assert_eq!(rack.elapsed(), Duration::ZERO);
    assert_eq!(rack.remaining(), Some(REPAIR_DURATION));
    assert!(rack.state().shows_wrench_badge(), "Task 7 reads this state");
    assert!(!rack.state().shows_fault_badge());
    assert_eq!(movement_lock(&app).ticket(), Some(target.id));
    assert_eq!(player_position(&mut app), before);
    assert_eq!(
        app.world().resource::<PlayerMotion>().accepted(),
        Vec2::ZERO
    );
    assert_eq!(
        app.world().resource::<PlayerAnimationState>().current(),
        PlayerClip::Repair
    );
    let animations = app.world().resource::<PlayerAnimations>().clone();
    let playing = app
        .world()
        .get::<AnimationPlayer>(animations.player)
        .expect("the technician exposes an AnimationPlayer")
        .playing_animations()
        .map(|(index, _)| *index)
        .collect::<Vec<_>>();
    assert_eq!(playing, vec![animations.node(PlayerClip::Repair)]);

    // Movement stays locked for the whole repair, arrows held throughout.
    hold(&mut app, approach);
    pump(&mut app, 60);
    assert_eq!(player_position(&mut app), before);
    assert!(movement_lock(&app).is_locked());

    // The camera is still live: a real Q press still orbits.
    let heading_before = orbit(&app).heading();
    tap(&mut app, &[KeyCode::KeyQ]);
    assert!(
        !orbit(&app).is_settled(),
        "the camera must still take input"
    );
    pump(&mut app, QUARTER_TURN_FRAMES - 1);
    assert_ne!(orbit(&app).heading(), heading_before);
    assert_eq!(player_position(&mut app), before);

    // A second press during the repair is rejected, not silently ignored.
    press_space(&mut app);
    assert_eq!(
        last_interaction(&app).outcome,
        InteractionOutcome::AlreadyRepairing { ticket: target.id }
    );
    assert_eq!(last_interaction(&app).started, 1);
}

#[test]
fn operations_repair_resolves_removes_the_ticket_and_cools_the_rack_down() {
    let mut app = operations_hall(&repo_assets());
    pump(&mut app, FAULT_FRAMES * 3);
    let target = ticket_queue(&app).ordered()[0].clone();
    let spot = repair_spot(&app, target.rack);
    place_player(&mut app, spot);
    hold(&mut app, &[]);
    app.update();

    press_space(&mut app);
    assert_eq!(rack_ops(&app, target.rack).state(), RackState::Repairing);
    assert!(movement_lock(&app).is_locked());

    // One fixed tick short of three seconds, then exactly on it.
    pump(&mut app, REPAIR_FRAMES - 1);
    assert_eq!(rack_ops(&app, target.rack).state(), RackState::Repairing);
    assert!(movement_lock(&app).is_locked());
    app.update();
    let resolved = rack_ops(&app, target.rack);
    assert_eq!(resolved.state(), RackState::Resolved);
    assert!(resolved.state().shows_healthy_badge());
    assert_eq!(resolved.ticket(), Some(target.id));
    assert!(
        !movement_lock(&app).is_locked(),
        "the lock is released the moment the repair completes"
    );
    assert_eq!(
        ticket_queue(&app).len(),
        3,
        "the ticket stays active while the healthy indicator shows"
    );

    // Movement really works again.
    let before = player_position(&mut app);
    drive(&mut app, &[KeyCode::ArrowUp], 5);
    assert_ne!(player_position(&mut app), before);
    hold(&mut app, &[]);

    // One tick short of the resolved display, then exactly on it.
    pump(&mut app, RESOLVED_FRAMES - 6);
    assert_eq!(rack_ops(&app, target.rack).state(), RackState::Resolved);
    assert_eq!(ticket_queue(&app).len(), 3);
    app.update();

    let cooling = rack_ops(&app, target.rack);
    assert_eq!(cooling.state(), RackState::Cooldown);
    assert_eq!(cooling.ticket(), None);
    // Each dwell carries its remainder, so the cooldown starts 100 ns in: the
    // 60 ns the repair boundary overshot plus the 40 ns the display added.
    assert_eq!(cooling.elapsed(), Duration::from_nanos(100));
    assert_eq!(
        cooling.remaining(),
        Some(RACK_COOLDOWN - Duration::from_nanos(100))
    );
    let queue = ticket_queue(&app);
    assert_eq!(queue.len(), 2, "the resolved ticket leaves the queue");
    assert_eq!(queue.get(target.id), None);
    assert!(!queue.is_at_capacity());

    // Capacity reopened, so the paused opportunity fires on this very frame.
    // The seeded stream's next entry is rack 1, which is still faulted, so the
    // drawn candidate is held and the duplicate is named rather than skipped.
    let after = scheduler(&app);
    assert_eq!(
        after.rng().draws(),
        8,
        "the paused opportunity drew exactly now"
    );
    assert_eq!(
        after.blocked(),
        Some(ScheduleBlock::DuplicateRack {
            rack: SEEDED_FAULTS[3].0,
            existing: TicketId::new(2)
        })
    );
    assert_eq!(after.duplicate_pauses(), 1);
    assert_eq!(after.emitted(), 3);

    // The repaired rack finishes its cooldown exactly on the boundary and only
    // then becomes eligible again.
    pump(&mut app, COOLDOWN_FRAMES - 1);
    assert_eq!(rack_ops(&app, target.rack).state(), RackState::Cooldown);
    app.update();
    let healthy = rack_ops(&app, target.rack);
    assert_eq!(healthy.state(), RackState::Healthy);
    assert!(healthy.state().is_eligible_for_fault());
    assert_eq!(healthy.elapsed(), Duration::ZERO);
    assert_eq!(
        ticket_queue(&app).len(),
        2,
        "the held candidate still belongs to a rack that is faulted"
    );
    assert_eq!(scheduler(&app).rng().draws(), 8, "waiting never rerolls");
}

/// The bone names one authored technician clip actually animates.
fn animated_bones(clip_name: &str) -> BTreeSet<String> {
    let source = load_source(&repo_root(), "technician").expect("the technician source parses");
    let rig = source
        .modules
        .iter()
        .find(|module| module.name == "technician")
        .expect("the technician module exists")
        .rig
        .as_ref()
        .expect("the technician is rigged");
    rig.clips
        .iter()
        .find(|clip| clip.name == clip_name)
        .unwrap_or_else(|| panic!("the technician declares a {clip_name} clip"))
        .tracks
        .iter()
        .map(|track| track.bone.clone())
        .collect()
}

#[test]
fn operations_leaving_the_repair_clip_restores_every_rest_transform_first() {
    // Repair poses `bone-head`, `bone-arm-lower-right`, and `bone-tool`, none
    // of which Walk animates. If the Repair -> Walk transition skips the rest
    // restore, those bones keep the repair pose forever.
    let repair_bones = animated_bones("Repair");
    let walk_bones = animated_bones("Walk");
    let repair_only = repair_bones
        .difference(&walk_bones)
        .cloned()
        .collect::<BTreeSet<_>>();
    assert!(
        repair_only.contains("bone-head")
            && repair_only.contains("bone-arm-lower-right")
            && repair_only.contains("bone-tool"),
        "the head, right forearm, and tool are posed by Repair alone, got {repair_only:?}"
    );

    let mut app = operations_hall(&repo_assets());
    watch_clip_transitions(&mut app);
    pump(&mut app, FAULT_FRAMES * 3);
    let target = ticket_queue(&app).ordered()[0].clone();
    let spot = repair_spot(&app, target.rack);
    place_player(&mut app, spot);
    hold(&mut app, &[]);
    app.update();

    press_space(&mut app);
    assert_eq!(rack_ops(&app, target.rack).state(), RackState::Repairing);
    assert_eq!(
        app.world().resource::<PlayerAnimationState>().current(),
        PlayerClip::Repair
    );

    // Sample the repair at a genuinely non-rest pose: run the clip until every
    // repair-only bone has been moved off its rest transform.
    let mut sampled = 0usize;
    let repair_pose = loop {
        let posed = part_transforms(&mut app)
            .into_iter()
            .filter(|(name, current, rest)| repair_only.contains(name) && current != rest)
            .count();
        if posed == repair_only.len() {
            break part_transforms(&mut app);
        }
        app.update();
        sampled += 1;
        assert!(
            sampled < REPAIR_FRAMES - 30,
            "the Repair clip never posed every repair-only bone"
        );
    };
    for (name, current, rest) in &repair_pose {
        if repair_only.contains(name) {
            assert_ne!(current, rest, "{name} must be off its rest pose to matter");
        }
    }

    // Hold the arrows down for the rest of the repair, so the frame the lock
    // releases is a direct Repair -> Walk transition with no Idle in between.
    let approach = keys_towards(&view_basis(&app), Vec2::new(0.0, 1.0));
    hold(&mut app, approach);
    app.world_mut().resource_mut::<TransitionProbe>().0.clear();
    let mut waited = 0usize;
    while movement_lock(&app).is_locked() {
        app.update();
        waited += 1;
        assert!(
            waited <= REPAIR_FRAMES,
            "the repair never released the lock"
        );
    }

    let before = player_position(&mut app);
    assert_eq!(
        app.world().resource::<PlayerAnimationState>().current(),
        PlayerClip::Walk,
        "released with the arrows still held, the technician walks straight off"
    );
    let transitions = app.world().resource::<TransitionProbe>().0.clone();
    assert_eq!(
        transitions
            .iter()
            .map(|(clip, _)| *clip)
            .collect::<Vec<_>>(),
        vec![PlayerClip::Walk],
        "exactly one transition ran, and it went straight from Repair to Walk"
    );
    let (_, captured) = &transitions[0];
    assert_eq!(
        captured.len(),
        required_player_parts().len(),
        "the transition must visit every discovered part"
    );
    for (name, restored, rest) in captured {
        assert_eq!(
            restored, rest,
            "{name} must be restored to its rest pose before Walk plays"
        );
    }

    // And the pose really is gone from the running world: Walk never writes
    // these bones, so anything stale would persist.
    for (name, current, rest) in part_transforms(&mut app) {
        if repair_only.contains(&name) {
            assert_eq!(current, rest, "{name} still holds the repair pose");
        }
    }
    pump(&mut app, 60);
    assert_ne!(player_position(&mut app), before, "the technician walked");
    assert_eq!(
        app.world().resource::<PlayerAnimationState>().current(),
        PlayerClip::Walk
    );
    for (name, current, rest) in part_transforms(&mut app) {
        if repair_only.contains(&name) {
            assert_eq!(
                current, rest,
                "{name} regained the repair pose while walking"
            );
        }
    }

    // Walking really does pose the rig, so the assertions above are not vacuous.
    assert!(
        part_transforms(&mut app)
            .into_iter()
            .any(|(name, current, rest)| walk_bones.contains(&name) && current != rest),
        "the Walk clip must actually pose the bones it owns"
    );
}

#[test]
fn operations_space_counts_one_edge_per_press_not_a_held_key() {
    let mut app = operations_hall(&repo_assets());
    pump(&mut app, FAULT_FRAMES * 3);
    assert_eq!(ticket_queue(&app).len(), 3);

    // Out of range in the centre aisle, so every counted press is a rejection
    // and 60 Hz spam would be unmissable.
    place_player(&mut app, Vec2::ZERO);
    hold(&mut app, &[]);
    app.update();

    key_message(&mut app, REPAIR_KEY, ButtonState::Pressed);
    pump(&mut app, 150);
    assert!(
        app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(REPAIR_KEY),
        "the key really was held down for every one of those frames"
    );
    assert!(
        !app.world()
            .resource::<ButtonInput<KeyCode>>()
            .just_pressed(REPAIR_KEY),
        "only the first frame of a hold is an edge"
    );
    let held = last_interaction(&app);
    assert_eq!(held.presses, 1, "holding Space is one press, not 150");
    assert_eq!(held.rejected, 1, "a held key must not spam rejections");
    assert_eq!(held.started, 0);
    assert!(matches!(
        held.outcome,
        InteractionOutcome::OutOfRange { .. }
    ));
    let rejected_tick = held.tick;

    // Releasing is not itself an interaction.
    key_message(&mut app, REPAIR_KEY, ButtonState::Released);
    pump(&mut app, 30);
    assert_eq!(last_interaction(&app).presses, 1);
    assert_eq!(last_interaction(&app).rejected, 1);
    assert_eq!(last_interaction(&app).tick, rejected_tick);

    // A second real press is a second edge, so the key is not latched either.
    key_message(&mut app, REPAIR_KEY, ButtonState::Pressed);
    pump(&mut app, 60);
    let again = last_interaction(&app);
    assert_eq!(again.presses, 2, "a fresh press is a fresh edge");
    assert_eq!(again.rejected, 2);
    assert!(again.tick > rejected_tick);
    key_message(&mut app, REPAIR_KEY, ButtonState::Released);
    app.update();

    // In range, a key held across the whole repair and its tail starts exactly
    // one repair and never reports a single `AlreadyRepairing`.
    let target = ticket_queue(&app).ordered()[0].clone();
    let spot = repair_spot(&app, target.rack);
    place_player(&mut app, spot);
    hold(&mut app, &[]);
    app.update();
    let before = last_interaction(&app);

    key_message(&mut app, REPAIR_KEY, ButtonState::Pressed);
    app.update();
    assert_eq!(
        last_interaction(&app).outcome,
        InteractionOutcome::Started {
            ticket: target.id,
            rack: target.rack
        }
    );
    pump(&mut app, REPAIR_FRAMES + RESOLVED_FRAMES);
    assert!(
        app.world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(REPAIR_KEY),
        "the key was still held all the way through the tail"
    );
    let after = last_interaction(&app);
    assert_eq!(after.presses, before.presses + 1, "one edge, one press");
    assert_eq!(after.started, 1);
    assert_eq!(
        after.rejected, before.rejected,
        "a held key must never re-enter the interaction while repairing"
    );
    assert_eq!(
        after.outcome,
        InteractionOutcome::Started {
            ticket: target.id,
            rack: target.rack
        },
        "no later frame overwrote the one real outcome"
    );
    assert_eq!(ticket_queue(&app).get(target.id), None);
    assert!(!movement_lock(&app).is_locked());
}

/// One thing the seeded journey observed happening.
#[derive(Clone, Debug, PartialEq)]
enum JourneyEvent {
    Opened {
        ticket: Ticket,
        tick: u64,
    },
    Removed {
        id: TicketId,
        rack: usize,
        tick: u64,
    },
}

/// Runs one frame and records every queue change it caused.
fn journey_frame(app: &mut App, active: &mut Vec<Ticket>, log: &mut Vec<JourneyEvent>) {
    app.update();
    let queue = ticket_queue(app);
    let tick = operations_tick(app);
    for ticket in queue.ordered() {
        if !active.iter().any(|held| held.id == ticket.id) {
            active.push(ticket.clone());
            log.push(JourneyEvent::Opened {
                ticket: ticket.clone(),
                tick,
            });
        }
    }
    active.retain(|held| {
        let kept = queue.get(held.id).is_some();
        if !kept {
            log.push(JourneyEvent::Removed {
                id: held.id,
                rack: held.rack,
                tick,
            });
        }
        kept
    });
}

#[test]
fn recurring_ticket_journey() {
    let mut app = operations_hall(&repo_assets());
    let mut active: Vec<Ticket> = Vec::new();
    let mut log: Vec<JourneyEvent> = Vec::new();
    let mut frames = 0usize;
    let budget = 20_000usize;

    // Three simultaneous tickets from the seeded stream, on the exact ticks.
    while active.len() < MAX_ACTIVE_TICKETS {
        journey_frame(&mut app, &mut active, &mut log);
        frames += 1;
        assert!(frames < budget, "the queue never filled");
    }
    assert_eq!(frames, FAULT_FRAMES * 3);
    assert!(ticket_queue(&app).is_at_capacity());

    // Repair the highest-priority ticket over and over until two separate racks
    // have faulted, been repaired, cooled down, and faulted again.
    let mut repaired: BTreeSet<usize> = BTreeSet::new();
    let mut recurrences: Vec<(usize, u64, u64)> = Vec::new();
    let mut removed_at: Vec<(usize, u64)> = Vec::new();
    let mut simultaneous = 0usize;
    let mut repairs = 0usize;

    while recurrences.len() < 2 {
        simultaneous = simultaneous.max(ticket_queue(&app).len());

        let Some(target) = ticket_queue(&app).ordered().first().cloned() else {
            journey_frame(&mut app, &mut active, &mut log);
            frames += 1;
            assert!(frames < budget, "the journey stalled with an empty queue");
            continue;
        };

        // This journey is about recurrence over thousands of frames, so it
        // places the technician at the repair spot rather than walking there.
        // The real arrow-key approach and the out-of-range rejection each have
        // their own dedicated contract; only the `Space` press is real here.
        let spot = repair_spot(&app, target.rack);
        place_player(&mut app, spot);
        hold(&mut app, &[]);
        journey_frame(&mut app, &mut active, &mut log);
        frames += 1;

        press_space(&mut app);
        frames += 1;
        let queue = ticket_queue(&app);
        for ticket in queue.ordered() {
            if !active.iter().any(|held| held.id == ticket.id) {
                active.push(ticket.clone());
            }
        }
        assert_eq!(
            last_interaction(&app).outcome,
            InteractionOutcome::Started {
                ticket: target.id,
                rack: target.rack
            },
            "the journey must start the repair it walked to"
        );
        assert!(movement_lock(&app).is_locked());
        assert_eq!(
            app.world().resource::<PlayerAnimationState>().current(),
            PlayerClip::Repair
        );
        repairs += 1;

        // Ride the documented tail out: repairing, resolved, ticket removed.
        let opened_before = log.len();
        while ticket_queue(&app).get(target.id).is_some() {
            journey_frame(&mut app, &mut active, &mut log);
            frames += 1;
            assert!(frames < budget, "ticket {} never resolved", target.id);
        }
        assert!(
            !movement_lock(&app).is_locked(),
            "the lock never survives the repair"
        );
        assert_eq!(rack_ops(&app, target.rack).state(), RackState::Cooldown);
        removed_at.push((target.rack, operations_tick(&app)));
        repaired.insert(target.rack);
        assert!(log.len() > opened_before);

        // Any fault that now opens on an already repaired rack is a recurrence.
        for event in &log {
            if let JourneyEvent::Opened { ticket, tick } = event
                && repaired.contains(&ticket.rack)
                && let Some((_, removed)) = removed_at
                    .iter()
                    .rfind(|(rack, removed)| *rack == ticket.rack && *removed < *tick)
                && !recurrences
                    .iter()
                    .any(|(rack, _, at)| *rack == ticket.rack && at == tick)
            {
                recurrences.push((ticket.rack, *removed, *tick));
            }
        }

        assert!(frames < budget, "the journey ran out of frames");
        assert!(repairs < 40, "the journey repaired far more than planned");
    }

    // The seeded sequence is exactly the pinned one, whatever the repairs did.
    let opened = log
        .iter()
        .filter_map(|event| match event {
            JourneyEvent::Opened { ticket, .. } => Some(ticket.clone()),
            JourneyEvent::Removed { .. } => None,
        })
        .collect::<Vec<_>>();
    assert!(opened.len() >= 5, "got {} tickets", opened.len());
    assert!(repairs >= 3, "the journey completed only {repairs} repairs");
    assert_eq!(
        opened
            .iter()
            .map(|ticket| (ticket.rack, ticket.severity))
            .collect::<Vec<_>>(),
        SEEDED_FAULTS[..opened.len()].to_vec(),
        "repair timing must never perturb the seeded rack and severity sequence"
    );
    assert_eq!(
        opened
            .iter()
            .map(|ticket| ticket.id.value())
            .collect::<Vec<_>>(),
        (1..=opened.len() as u64).collect::<Vec<_>>(),
        "ticket identifiers are stable and monotonic"
    );
    assert_eq!(
        scheduler(&app).rng().draws(),
        2 * scheduler(&app).emitted()
            + if scheduler(&app).pending().is_some() {
                2
            } else {
                0
            },
        "exactly two words are drawn per candidate, and only for candidates"
    );

    // Multiple simultaneous tickets, and never more than the reviewed maximum.
    assert_eq!(simultaneous, MAX_ACTIVE_TICKETS);
    for event in &log {
        if let JourneyEvent::Opened { ticket, .. } = event {
            assert!(ticket.rack < 4);
        }
    }

    // At least two full recurrence cycles, each honouring the whole cooldown.
    assert!(
        recurrences.len() >= 2,
        "the journey must show at least two recurrence cycles, got {recurrences:?}"
    );
    for (rack, removed, reopened) in &recurrences {
        assert!(
            reopened - removed >= COOLDOWN_FRAMES as u64,
            "rack {rack} re-faulted {} ticks after its ticket was removed, \
             which is inside the {COOLDOWN_FRAMES}-tick cooldown",
            reopened - removed
        );
    }
    assert!(
        recurrences
            .iter()
            .any(|(_, removed, reopened)| reopened - removed == COOLDOWN_FRAMES as u64),
        "a candidate held on a cooling rack must fire the instant the cooldown \
         ends, got {recurrences:?}"
    );
    assert!(
        recurrences
            .iter()
            .map(|(rack, _, _)| *rack)
            .collect::<BTreeSet<_>>()
            .len()
            >= 2,
        "two separate racks must complete a recurrence cycle, got {recurrences:?}"
    );

    // Every rack that ever faulted is joined to an authored rack row, and the
    // final state is internally consistent.
    let queue = ticket_queue(&app);
    for (rack, state) in rack_states(&app).into_iter().enumerate() {
        assert_eq!(
            state.holds_ticket(),
            queue.contains_rack(rack),
            "rack {rack} state {state:?} disagrees with the queue"
        );
    }
    assert!(app.world().resource::<PlayerRigReport>().is_healthy());
    assert_eq!(FAULT_INTERVAL, Duration::from_secs(4));
    assert_eq!(RESOLVED_DISPLAY, Duration::from_secs(2));
}

// ---------------------------------------------------------------------------
// Operations HUD contracts
// ---------------------------------------------------------------------------

fn hud_report(app: &App) -> HudReport {
    app.world().resource::<HudReport>().clone()
}

/// The one entity carrying a marker component.
fn hud_single<T: Component>(app: &mut App) -> Entity {
    let entities = app
        .world_mut()
        .query_filtered::<Entity, With<T>>()
        .iter(app.world())
        .collect::<Vec<_>>();
    assert_eq!(
        entities.len(),
        1,
        "exactly one {} must exist",
        std::any::type_name::<T>()
    );
    entities[0]
}

/// The real laid-out rectangle of one UI node, in logical px.
fn ui_rect(app: &App, entity: Entity) -> Rect {
    let node = *app
        .world()
        .get::<ComputedNode>(entity)
        .expect("a HUD node carries a ComputedNode");
    let transform = *app
        .world()
        .get::<UiGlobalTransform>(entity)
        .expect("a HUD node carries a UiGlobalTransform");
    let scale = node.inverse_scale_factor;
    let center = transform.translation * scale;
    let half = node.size * 0.5 * scale;
    Rect::from_corners(center - half, center + half)
}

fn ui_displayed(app: &App, entity: Entity) -> bool {
    app.world()
        .get::<Node>(entity)
        .expect("a HUD node carries a Node")
        .display
        != Display::None
}

fn ui_text(app: &App, entity: Entity) -> String {
    app.world()
        .get::<Text>(entity)
        .expect("a HUD label carries Text")
        .0
        .clone()
}

fn ui_background(app: &App, entity: Entity) -> Srgba {
    app.world()
        .get::<BackgroundColor>(entity)
        .expect("a HUD node carries a BackgroundColor")
        .0
        .to_srgba()
}

fn ui_text_color(app: &App, entity: Entity) -> Srgba {
    app.world()
        .get::<TextColor>(entity)
        .expect("a HUD label carries a TextColor")
        .0
        .to_srgba()
}

/// The real authored corner radius of one UI node.
fn ui_corner_radius(app: &App, entity: Entity) -> BorderRadius {
    app.world()
        .get::<Node>(entity)
        .expect("a HUD node carries a Node")
        .border_radius
}

/// The real authored width of one UI node.
fn ui_width(app: &App, entity: Entity) -> Val {
    app.world()
        .get::<Node>(entity)
        .expect("a HUD node carries a Node")
        .width
}

/// Every queue row entity, by slot.
fn queue_rows_by_slot(app: &mut App) -> Vec<(usize, Entity)> {
    let mut rows = app
        .world_mut()
        .query::<(Entity, &QueueRowNode)>()
        .iter(app.world())
        .map(|(entity, row)| (row.slot, entity))
        .collect::<Vec<_>>();
    rows.sort_by_key(|(slot, _)| *slot);
    rows
}

fn queue_row_labels(app: &mut App) -> Vec<(usize, Entity)> {
    let mut labels = app
        .world_mut()
        .query::<(Entity, &QueueRowLabel)>()
        .iter(app.world())
        .map(|(entity, label)| (label.slot, entity))
        .collect::<Vec<_>>();
    labels.sort_by_key(|(slot, _)| *slot);
    labels
}

fn queue_row_severity_chips(app: &mut App) -> Vec<(usize, Entity)> {
    let mut chips = app
        .world_mut()
        .query::<(Entity, &QueueRowSeverityChip)>()
        .iter(app.world())
        .map(|(entity, chip)| (chip.slot, entity))
        .collect::<Vec<_>>();
    chips.sort_by_key(|(slot, _)| *slot);
    chips
}

fn queue_row_state_chips(app: &mut App) -> Vec<(usize, Entity)> {
    let mut chips = app
        .world_mut()
        .query::<(Entity, &QueueRowStateChip)>()
        .iter(app.world())
        .map(|(entity, chip)| (chip.slot, entity))
        .collect::<Vec<_>>();
    chips.sort_by_key(|(slot, _)| *slot);
    chips
}

fn rack_badge_nodes(app: &mut App) -> Vec<(usize, Entity)> {
    let mut badges = app
        .world_mut()
        .query::<(Entity, &RackBadgeNode)>()
        .iter(app.world())
        .map(|(entity, badge)| (badge.rack, entity))
        .collect::<Vec<_>>();
    badges.sort_by_key(|(rack, _)| *rack);
    badges
}

fn queue_row_progress_bars(app: &mut App) -> Vec<(usize, Entity)> {
    let mut bars = app
        .world_mut()
        .query::<(Entity, &QueueRowProgress)>()
        .iter(app.world())
        .map(|(entity, bar)| (bar.slot, entity))
        .collect::<Vec<_>>();
    bars.sort_by_key(|(slot, _)| *slot);
    bars
}

fn rack_badge_labels(app: &mut App) -> Vec<(usize, Entity)> {
    let mut labels = app
        .world_mut()
        .query::<(Entity, &RackBadgeLabel)>()
        .iter(app.world())
        .map(|(entity, label)| (label.rack, entity))
        .collect::<Vec<_>>();
    labels.sort_by_key(|(rack, _)| *rack);
    labels
}

/// The one badge node belonging to one rack.
fn badge_node(app: &mut App, rack: usize) -> Entity {
    rack_badge_nodes(app)
        .into_iter()
        .find(|(index, _)| *index == rack)
        .unwrap_or_else(|| panic!("rack {rack} must have a badge node"))
        .1
}

/// The one badge glyph belonging to one rack.
fn badge_label_node(app: &mut App, rack: usize) -> Entity {
    rack_badge_labels(app)
        .into_iter()
        .find(|(index, _)| *index == rack)
        .unwrap_or_else(|| panic!("rack {rack} must have a badge glyph"))
        .1
}

/// The one leader line belonging to one rack.
fn leader_node(app: &mut App, rack: usize) -> Entity {
    rack_leader_lines(app)
        .into_iter()
        .find(|(index, _)| *index == rack)
        .unwrap_or_else(|| panic!("rack {rack} must have a leader line"))
        .1
}

fn rack_leader_lines(app: &mut App) -> Vec<(usize, Entity)> {
    let mut leaders = app
        .world_mut()
        .query::<(Entity, &RackLeaderLine)>()
        .iter(app.world())
        .map(|(entity, leader)| (leader.rack, entity))
        .collect::<Vec<_>>();
    leaders.sort_by_key(|(rack, _)| *rack);
    leaders
}

fn control_caps(app: &mut App) -> Vec<(HudControl, Entity)> {
    app.world_mut()
        .query::<(Entity, &ControlHintCap)>()
        .iter(app.world())
        .map(|(entity, cap)| (cap.control, entity))
        .collect()
}

/// Resizes the primary window exactly the way the windowing backend does, so
/// the camera and the UI layout both see the change.
fn resize_window(app: &mut App, width: f32, height: f32) {
    let window = app
        .world_mut()
        .query_filtered::<Entity, With<Window>>()
        .iter(app.world())
        .next()
        .expect("the app has a primary window");
    app.world_mut()
        .get_mut::<Window>(window)
        .expect("the primary window carries a Window")
        .resolution
        .set(width, height);
    app.world_mut().write_message(WindowResized {
        window,
        width,
        height,
    });
    pump(app, 3);
}

/// The central half-width, half-height rectangle the fixed panels must never
/// cover.
fn play_rectangle(viewport: Vec2) -> Rect {
    Rect::from_corners(viewport * 0.25, viewport * 0.75)
}

fn hud_app(assets: &Path) -> App {
    let mut app = operations_hall(assets);
    app.update();
    assert!(
        hud_report(&app).is_healthy(),
        "the HUD started with errors: {:?}",
        hud_report(&app).errors
    );
    app
}

/// Fills the queue with the first three seeded tickets.
fn fill_queue(app: &mut App) {
    pump(app, FAULT_FRAMES * 3);
    assert_eq!(ticket_queue(app).len(), MAX_ACTIVE_TICKETS);
}

/// Stands the technician in the middle of the hall, where the camera frames
/// every rack row's badge anchor at the initial heading.
fn center_player(app: &mut App) {
    place_player(app, Vec2::ZERO);
    hold(app, &[]);
    pump(app, 4);
}

/// Whether one viewport point is inside the viewport rectangle.
fn on_screen(point: Vec2, viewport: Vec2) -> bool {
    (0.0..=viewport.x).contains(&point.x) && (0.0..=viewport.y).contains(&point.y)
}

#[test]
fn operations_hud_shows_the_healthy_state_with_no_ticket_and_no_badge() {
    let mut app = hud_app(&repo_assets());
    let report = hud_report(&app);

    assert!(ticket_queue(&app).is_empty());
    assert!(report.rows.is_empty(), "{:?}", report.rows);
    assert_eq!(report.status, HudStatus::AllHealthy);
    assert!(!report.movement_locked);
    assert_eq!(report.badges.len(), roster(&app).len());
    for badge in &report.badges {
        assert_eq!(badge.kind, None, "rack {} shows a badge", badge.rack);
        assert_eq!(badge.visibility, BadgeVisibility::NoTicket);
    }
    assert!(report.shown_badges().is_empty());

    for (slot, entity) in queue_rows_by_slot(&mut app) {
        assert!(!ui_displayed(&app, entity), "row {slot} is visible");
    }
    for (rack, entity) in rack_badge_nodes(&mut app) {
        assert!(!ui_displayed(&app, entity), "badge {rack} is visible");
    }
    for (rack, entity) in rack_leader_lines(&mut app) {
        assert!(!ui_displayed(&app, entity), "leader {rack} is visible");
    }

    let header = hud_single::<QueueHeaderLabel>(&mut app);
    assert_eq!(ui_text(&app, header), "OPS QUEUE 0/3");
    let status = hud_single::<HudStatusLabel>(&mut app);
    assert_eq!(ui_text(&app, status), "All racks healthy");
    let chip = hud_single::<HudStatusChip>(&mut app);
    assert_eq!(ui_background(&app, chip), HEALTHY_GREEN);
}

#[test]
fn operations_hud_lists_one_then_three_tickets_in_queue_priority_order() {
    let mut app = hud_app(&repo_assets());

    // One ticket.
    pump(&mut app, FAULT_FRAMES);
    let first = ticket_queue(&app).ordered()[0].clone();
    let report = hud_report(&app);
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].ticket, first.id);
    assert_eq!(report.rows[0].rack, first.rack);
    assert_eq!(report.status, HudStatus::TicketsOpen);
    let rows = queue_rows_by_slot(&mut app);
    assert!(ui_displayed(&app, rows[0].1));
    assert!(!ui_displayed(&app, rows[1].1));
    assert!(!ui_displayed(&app, rows[2].1));

    // Three tickets, in the queue's own global priority order.
    pump(&mut app, FAULT_FRAMES * 2);
    let queue = ticket_queue(&app);
    assert_eq!(queue.len(), MAX_ACTIVE_TICKETS);
    let report = hud_report(&app);
    assert_eq!(
        report
            .rows
            .iter()
            .map(|row| (row.slot, row.ticket, row.rack, row.severity))
            .collect::<Vec<_>>(),
        queue
            .ordered()
            .iter()
            .enumerate()
            .map(|(slot, ticket)| (slot, ticket.id, ticket.rack, ticket.severity))
            .collect::<Vec<_>>(),
        "the HUD renders the live queue order, never its own"
    );

    let header = hud_single::<QueueHeaderLabel>(&mut app);
    assert_eq!(ui_text(&app, header), "OPS QUEUE 3/3");

    // The rendered rows carry the real labels, colours, and vertical order.
    let labels = queue_row_labels(&mut app);
    let severity_chips = queue_row_severity_chips(&mut app);
    let state_chips = queue_row_state_chips(&mut app);
    let row_nodes = queue_rows_by_slot(&mut app);
    let mut previous_bottom = f32::NEG_INFINITY;
    for row in &report.rows {
        let ticket = queue
            .get(row.ticket)
            .expect("every rendered row names a live ticket");
        assert_eq!(
            ui_text(&app, labels[row.slot].1),
            format!(
                "{} R{:02} {}",
                ticket.id,
                ticket.rack + 1,
                ticket.severity.label()
            )
        );
        assert_eq!(
            ui_background(&app, severity_chips[row.slot].1),
            severity_role(ticket.severity).color(),
            "row {} shows the wrong severity colour",
            row.slot
        );
        assert_eq!(
            ui_background(&app, state_chips[row.slot].1),
            state_role(row.state).color()
        );
        let rect = ui_rect(&app, row_nodes[row.slot].1);
        assert!(
            rect.min.y > previous_bottom,
            "row {} is not below row {}",
            row.slot,
            row.slot.saturating_sub(1)
        );
        previous_bottom = rect.min.y;
    }

    // Every faulted rack the camera can see carries the red badge, and no
    // other rack does.
    center_player(&mut app);
    let report = hud_report(&app);
    let viewport = viewport_size(&mut app);
    let mut expected = queue
        .ordered()
        .iter()
        .map(|ticket| (ticket.rack, BadgeKind::Fault))
        .collect::<Vec<_>>();
    expected.sort();
    for (rack, _) in &expected {
        let anchor = report
            .badge(*rack)
            .expect("a faulted rack has a badge")
            .anchor;
        assert!(
            anchor.is_some_and(|anchor| on_screen(anchor, viewport)),
            "rack {rack} must be framed from the middle of the hall, got {anchor:?}"
        );
    }
    let mut shown_sorted = report.shown_badges();
    shown_sorted.sort();
    assert_eq!(shown_sorted, expected, "got {shown_sorted:?}");
    for (rack, entity) in rack_badge_nodes(&mut app) {
        let visible = expected.iter().any(|(faulted, _)| *faulted == rack);
        assert_eq!(ui_displayed(&app, entity), visible, "badge {rack}");
        if visible {
            assert_eq!(ui_background(&app, entity), FAULT_RED);
        }
    }
}

#[test]
fn operations_hud_follows_the_repair_resolve_and_removal_states() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);
    let target = ticket_queue(&app).ordered()[0].clone();

    let spot = repair_spot(&app, target.rack);
    place_player(&mut app, spot);
    hold(&mut app, &[]);
    app.update();
    press_space(&mut app);

    // Repairing: blue badge, blue row state, locked controls.
    assert_eq!(rack_ops(&app, target.rack).state(), RackState::Repairing);
    let report = hud_report(&app);
    assert_eq!(report.status, HudStatus::Repairing);
    assert!(report.movement_locked);
    assert_eq!(
        report
            .badge(target.rack)
            .expect("the rack has a badge")
            .kind,
        Some(BadgeKind::Repairing)
    );
    let repairing_row = report
        .rows
        .iter()
        .find(|row| row.ticket == target.id)
        .expect("the repairing ticket is still queued");
    assert_eq!(repairing_row.state, RackState::Repairing);
    let badge = rack_badge_nodes(&mut app)
        .into_iter()
        .find(|(rack, _)| *rack == target.rack)
        .expect("the repairing rack has a badge node")
        .1;
    assert_eq!(ui_background(&app, badge), WORKER_HARD_HAT);
    for (control, entity) in control_caps(&mut app) {
        assert_eq!(
            ui_background(&app, entity),
            control.cap_role(true).color(),
            "{control:?} does not show the locked repair"
        );
    }

    // Halfway through the repair the progress bar is halfway across.
    pump(&mut app, REPAIR_FRAMES / 2);
    let report = hud_report(&app);
    let row = report
        .rows
        .iter()
        .find(|row| row.ticket == target.id)
        .expect("still queued");
    assert!(
        (row.progress - 0.5).abs() < 0.05,
        "the repair bar reads {}",
        row.progress
    );

    // Resolved: healthy green badge while the ticket is still queued.
    pump(&mut app, REPAIR_FRAMES / 2 + 1);
    assert_eq!(rack_ops(&app, target.rack).state(), RackState::Resolved);
    let report = hud_report(&app);
    assert!(!report.movement_locked);
    assert_eq!(
        report.badge(target.rack).expect("badge").kind,
        Some(BadgeKind::Resolved)
    );
    assert_eq!(ui_background(&app, badge), HEALTHY_GREEN);
    assert!(
        report.rows.iter().any(|row| row.ticket == target.id),
        "the resolved ticket is still in the queue and still on the HUD"
    );
    for (control, entity) in control_caps(&mut app) {
        assert_eq!(ui_background(&app, entity), control.cap_role(false).color());
    }

    // Removed: the row disappears and the badge goes out with it.
    pump(&mut app, RESOLVED_FRAMES + 1);
    assert_eq!(ticket_queue(&app).get(target.id), None);
    assert_eq!(rack_ops(&app, target.rack).state(), RackState::Cooldown);
    let report = hud_report(&app);
    assert!(
        !report.rows.iter().any(|row| row.ticket == target.id),
        "a removed ticket must leave the HUD"
    );
    assert_eq!(report.badge(target.rack).expect("badge").kind, None);
    assert_eq!(
        report.badge(target.rack).expect("badge").visibility,
        BadgeVisibility::NoTicket
    );
    assert!(!ui_displayed(&app, badge));
    assert_eq!(report.rows.len(), ticket_queue(&app).len());
    assert!(hud_report(&app).is_healthy());
}

#[test]
fn operations_hud_status_reports_a_real_out_of_range_rejection() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);

    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));
    hold(&mut app, &[]);
    app.update();
    press_space(&mut app);

    assert!(matches!(
        last_interaction(&app).outcome,
        InteractionOutcome::OutOfRange { .. }
    ));
    let report = hud_report(&app);
    assert_eq!(report.status, HudStatus::MoveCloser);
    let status = hud_single::<HudStatusLabel>(&mut app);
    assert_eq!(ui_text(&app, status), "Move closer");
    let chip = hud_single::<HudStatusChip>(&mut app);
    assert_eq!(ui_background(&app, chip), SIGNATURE_YELLOW);
    assert!(!report.movement_locked, "a rejection never locks movement");
}

/// Opens one real fault on a rack that has none, exactly the way the scheduler
/// does: the rack's own `RackOperations` takes the fault and the live
/// `TicketQueue` takes the ticket. There is no second model to seed.
fn open_fault(app: &mut App, rack: usize, severity: TicketSeverity, id: u64) -> Ticket {
    let entry = roster(app)
        .get(rack)
        .cloned()
        .unwrap_or_else(|| panic!("rack {rack} must be on the roster"));
    let ticket = Ticket {
        id: TicketId::new(id),
        rack,
        rack_id: entry.id.clone(),
        severity,
        created_tick: operations_tick(app),
    };
    assert!(
        app.world_mut()
            .get_mut::<RackOperations>(entry.entity)
            .expect("the rack carries operational state")
            .open_fault(ticket.id),
        "rack {rack} must be eligible for a fault"
    );
    app.world_mut()
        .resource_mut::<TicketQueue>()
        .insert(ticket.clone())
        .expect("the live queue accepts the fault");
    app.update();
    ticket
}

#[test]
fn operations_hud_clears_the_move_closer_prompt_when_that_rack_changes_or_you_walk_in() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);

    // Stand in an aisle, out of range of every faulted rack, and press Space.
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));
    hold(&mut app, &[]);
    app.update();
    press_space(&mut app);
    let InteractionOutcome::OutOfRange {
        nearest_rack: Some(rejected),
        ..
    } = last_interaction(&app).outcome
    else {
        panic!("the press must be rejected against one named rack");
    };
    assert_eq!(hud_report(&app).status, HudStatus::MoveCloser);
    let status_label = hud_single::<HudStatusLabel>(&mut app);
    assert_eq!(ui_text(&app, status_label), "Move closer");

    // Standing still with the rejection unchanged keeps the prompt up.
    pump(&mut app, 3);
    assert_eq!(hud_report(&app).status, HudStatus::MoveCloser);

    // Walking into range of that rack clears it, with no new press at all and
    // with every ticket still open.
    let spot = repair_spot(&app, rejected);
    place_player(&mut app, spot);
    hold(&mut app, &[]);
    pump(&mut app, 2);
    assert!(matches!(
        last_interaction(&app).outcome,
        InteractionOutcome::OutOfRange { .. }
    ));
    assert!(!ticket_queue(&app).is_empty());
    assert_eq!(
        hud_report(&app).status,
        HudStatus::TicketsOpen,
        "the prompt that told you to move closer must go once you have"
    );
    assert_eq!(ui_text(&app, status_label), "Tickets waiting");

    // Walk back out: the same standing rejection becomes true again, because
    // it is derived from live state rather than remembered.
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));
    hold(&mut app, &[]);
    pump(&mut app, 2);
    assert_eq!(hud_report(&app).status, HudStatus::MoveCloser);

    // Now repair that exact rack. The rejection was about it, so it must not
    // survive the repair even though the other two tickets are still open.
    // A rejection can only ever be superseded by another real press, so the
    // recorded outcome is captured here and replayed verbatim afterwards: it
    // is the same value the real input system produced a moment ago, put back
    // against later live state.
    let recorded = last_interaction(&app);
    let spot = repair_spot(&app, rejected);
    place_player(&mut app, spot);
    hold(&mut app, &[]);
    app.update();
    press_space(&mut app);
    assert_eq!(rack_ops(&app, rejected).state(), RackState::Repairing);
    assert_eq!(hud_report(&app).status, HudStatus::Repairing);

    // Let the repair finish, step back out of range so distance is no longer
    // what clears the prompt, and replay the recorded rejection.
    pump(&mut app, REPAIR_FRAMES + 1);
    place_player(&mut app, Vec2::new(AISLE_CENTER_X[1], 0.0));
    hold(&mut app, &[]);
    app.world_mut().insert_resource(recorded);
    pump(&mut app, 2);
    assert_eq!(rack_ops(&app, rejected).state(), RackState::Resolved);
    assert_eq!(last_interaction(&app), recorded, "no new press happened");
    assert!(
        ticket_queue(&app).len() >= 2,
        "the other tickets are still open, got {}",
        ticket_queue(&app).len()
    );
    assert_eq!(
        hud_report(&app).status,
        HudStatus::TicketsOpen,
        "a rejection about a resolved rack must not survive on other racks' tickets"
    );

    // And once that rack's ticket leaves the queue entirely, still nothing.
    pump(&mut app, RESOLVED_FRAMES + 1);
    assert_eq!(ticket_queue(&app).for_rack(rejected), None);
    assert_eq!(last_interaction(&app), recorded);
    assert!(!ticket_queue(&app).is_empty(), "other tickets remain open");
    assert_eq!(hud_report(&app).status, HudStatus::TicketsOpen);
    assert_eq!(ui_text(&app, status_label), "Tickets waiting");
}

#[test]
fn operations_hud_shapes_the_severity_chip_by_severity_at_the_node_level() {
    let mut app = hud_app(&repo_assets());
    pump(&mut app, FAULT_FRAMES);
    let critical = ticket_queue(&app).ordered()[0].clone();
    assert_eq!(critical.severity, TicketSeverity::Critical);

    let free = (0..roster(&app).len())
        .find(|rack| !ticket_queue(&app).contains_rack(*rack))
        .expect("a rack without a ticket");
    let warning = open_fault(&mut app, free, TicketSeverity::Warning, 9_001);

    let report = hud_report(&app);
    let slot_of = |ticket: TicketId| {
        report
            .rows
            .iter()
            .find(|row| row.ticket == ticket)
            .unwrap_or_else(|| panic!("{ticket} must be on the HUD"))
            .slot
    };
    let chips = queue_row_severity_chips(&mut app);
    let critical_chip = chips[slot_of(critical.id)].1;
    let warning_chip = chips[slot_of(warning.id)].1;

    // Shape carries the severity at the real node, not only the colour.
    assert_eq!(
        ui_corner_radius(&app, critical_chip),
        BorderRadius::all(Val::Px(0.0)),
        "a Critical chip is a sharp square"
    );
    assert_eq!(
        ui_corner_radius(&app, warning_chip),
        BorderRadius::all(Val::Px(QUEUE_CHIP_SIZE * 0.5)),
        "a Warning chip is a circle"
    );
    assert_ne!(
        ui_corner_radius(&app, critical_chip),
        ui_corner_radius(&app, warning_chip),
        "the two severities must be told apart with the colour turned off"
    );
    assert_eq!(ui_background(&app, critical_chip), FAULT_RED);
    assert_eq!(ui_background(&app, warning_chip), SIGNATURE_YELLOW);
    for chip in [critical_chip, warning_chip] {
        let rect = ui_rect(&app, chip);
        assert_eq!(rect.size(), Vec2::splat(QUEUE_CHIP_SIZE));
    }
}

#[test]
fn operations_hud_draws_every_badge_shape_and_glyph_at_the_node_level() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);
    let target = ticket_queue(&app).ordered()[0].clone();
    let spot = repair_spot(&app, target.rack);
    place_player(&mut app, spot);
    hold(&mut app, &[]);
    app.update();

    let badge = badge_node(&mut app, target.rack);
    let glyph = badge_label_node(&mut app, target.rack);
    let leader = leader_node(&mut app, target.rack);

    // Every state's shape and glyph, read off the real nodes.
    let mut seen = Vec::new();
    let mut check = |app: &mut App, state: RackState, kind: BadgeKind| {
        assert_eq!(rack_ops(app, target.rack).state(), state);
        assert_eq!(
            hud_report(app)
                .badge(target.rack)
                .expect("badge")
                .visibility,
            BadgeVisibility::Shown,
            "the {state:?} badge must really be drawn to be checked"
        );
        assert!(ui_displayed(app, badge));
        assert_eq!(
            ui_corner_radius(app, badge),
            BorderRadius::all(Val::Px(kind.corner_radius())),
            "the {kind:?} badge has the wrong corner radius"
        );
        assert_eq!(
            ui_text(app, glyph),
            kind.label(),
            "the {kind:?} badge has the wrong glyph"
        );
        assert_eq!(ui_background(app, badge), kind.role().color());
        assert_eq!(ui_text_color(app, glyph), kind.text_role().color());
        let content = app
            .world()
            .get::<ComputedNode>(glyph)
            .expect("the glyph carries a ComputedNode")
            .content_size;
        assert!(
            content.x > 0.0 && content.y > 0.0,
            "the {kind:?} glyph rendered nothing"
        );
        seen.push((kind, ui_corner_radius(app, badge), ui_text(app, glyph)));
    };

    check(&mut app, RackState::Faulted, BadgeKind::Fault);
    press_space(&mut app);
    check(&mut app, RackState::Repairing, BadgeKind::Repairing);

    // Halfway through the repair, the progress bar node is halfway across.
    pump(&mut app, REPAIR_FRAMES / 2);
    let report = hud_report(&app);
    let row = report
        .rows
        .iter()
        .find(|row| row.ticket == target.id)
        .expect("the repairing ticket is still queued");
    let bar = queue_row_progress_bars(&mut app)[row.slot].1;
    assert_eq!(
        ui_width(&app, bar),
        Val::Percent(row.progress * 100.0),
        "the bar node width must be the live dwell progress"
    );
    assert!(
        (row.progress - 0.5).abs() < 0.05,
        "the sample must be mid repair, got {}",
        row.progress
    );
    let row_entity = queue_rows_by_slot(&mut app)[row.slot].1;
    let row_rect = ui_rect(&app, row_entity);
    let bar_rect = ui_rect(&app, bar);
    assert!(
        (bar_rect.width() - row_rect.width() * row.progress).abs() < 1.0,
        "the laid-out bar is {} px across a {} px row at progress {}",
        bar_rect.width(),
        row_rect.width(),
        row.progress
    );
    assert!((bar_rect.height() - QUEUE_PROGRESS_HEIGHT).abs() < 0.5);

    pump(&mut app, REPAIR_FRAMES / 2 + 1);
    check(&mut app, RackState::Resolved, BadgeKind::Resolved);

    // All three shapes and all three glyphs really differed at the node.
    assert_eq!(seen.len(), 3);
    for (left, right) in [(0, 1), (0, 2), (1, 2)] {
        assert_ne!(seen[left].1, seen[right].1, "two badges shared a shape");
        assert_ne!(seen[left].2, seen[right].2, "two badges shared a glyph");
    }

    // Hidden states: once the ticket goes, the badge, its glyph, its leader,
    // and its row are all switched off rather than left stale.
    pump(&mut app, RESOLVED_FRAMES + 1);
    assert_eq!(ticket_queue(&app).get(target.id), None);
    let report = hud_report(&app);
    assert_eq!(report.badge(target.rack).expect("badge").kind, None);
    assert_eq!(
        report.badge(target.rack).expect("badge").visibility,
        BadgeVisibility::NoTicket
    );
    assert!(!ui_displayed(&app, badge), "the badge stayed up");
    assert!(!ui_displayed(&app, leader), "the leader stayed up");
    for (slot, entity) in queue_rows_by_slot(&mut app) {
        assert_eq!(
            ui_displayed(&app, entity),
            report.rows.iter().any(|row| row.slot == slot),
            "row {slot} display disagrees with the live queue"
        );
    }
    assert!(
        hud_report(&app).is_healthy(),
        "{:?}",
        hud_report(&app).errors
    );
}

#[test]
fn operations_hud_reports_a_camera_with_no_viewport_instead_of_guessing_one() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);
    center_player(&mut app);
    assert!(!hud_report(&app).shown_badges().is_empty());

    // The real state a camera is in before `camera_system` has ever sized it:
    // it exists, it is the game camera, and it has no viewport at all.
    let camera = camera_entity(&mut app);
    app.world_mut()
        .get_mut::<Camera>(camera)
        .expect("the game camera carries a Camera")
        .computed
        .target_info = None;
    app.update();

    let report = hud_report(&app);
    assert_eq!(
        report.errors,
        vec![HudError::NoViewport],
        "a camera with no viewport is its own failure, not a missing camera"
    );
    assert!(!report.errors.contains(&HudError::NoCamera));
    assert_eq!(report.viewport, Vec2::ZERO);
    for badge in &report.badges {
        if badge.kind.is_some() {
            assert_eq!(badge.visibility, BadgeVisibility::NoViewport);
            assert_eq!(badge.anchor, None);
            assert_eq!(badge.center, None);
        }
    }
    for (rack, entity) in rack_badge_nodes(&mut app) {
        assert!(!ui_displayed(&app, entity), "badge {rack} survived");
    }
    assert_eq!(
        report.rows.len(),
        MAX_ACTIVE_TICKETS,
        "tickets are not badges"
    );
}

#[test]
fn operations_hud_reports_a_rack_with_no_badge_node_exactly_once() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);
    center_player(&mut app);

    // A rack whose badge is really being drawn right now, so the failure lands
    // on the write path rather than the hide path.
    let drawn = hud_report(&app)
        .shown_badges()
        .first()
        .expect("at least one badge is drawn from the middle of the hall")
        .0;
    let badge = badge_node(&mut app, drawn);
    let leader = leader_node(&mut app, drawn);
    app.world_mut().entity_mut(badge).remove::<RackBadgeNode>();
    app.update();

    let report = hud_report(&app);
    assert_eq!(
        report
            .errors
            .iter()
            .filter(|error| **error == HudError::MissingBadgeNode { rack: drawn })
            .count(),
        1,
        "a missing badge node is one failure, not one per write and one per \
         hide: got {:?}",
        report.errors
    );
    assert_eq!(
        report.errors,
        vec![HudError::MissingBadgeNode { rack: drawn }],
        "nothing else failed"
    );
    let hud_badge = report
        .badge(drawn)
        .expect("the rack is still on the roster");
    assert_eq!(
        hud_badge.visibility,
        BadgeVisibility::MissingBadgeNode,
        "a missing node is not a missing rack"
    );
    assert_eq!(hud_badge.kind, Some(BadgeKind::Fault));
    assert_eq!(hud_badge.center, None);
    assert!(hud_badge.anchor.is_some(), "the projection itself worked");
    assert!(
        !ui_displayed(&app, leader),
        "a badge that cannot be drawn must not leave its leader line pointing at nothing"
    );

    // Every other rack is unaffected, and the queue still reads live.
    for badge in &report.badges {
        if badge.rack != drawn {
            assert_ne!(badge.visibility, BadgeVisibility::MissingBadgeNode);
        }
    }
    assert_eq!(report.rows.len(), MAX_ACTIVE_TICKETS);
}

#[test]
fn operations_hud_refuses_a_parented_camera_instead_of_projecting_a_local_transform() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);
    center_player(&mut app);
    assert!(!hud_report(&app).shown_badges().is_empty());

    // The HUD substitutes the camera's own `Transform` for its `GlobalTransform`
    // to avoid a frame of propagation lag. That is only sound while the camera
    // has no parent, so giving it one must be refused outright rather than
    // projected through a local transform pretending to be a global one.
    let parent = app
        .world_mut()
        .spawn((
            Name::new("camera-rig-parent"),
            Transform::from_xyz(37.0, -11.0, 23.0),
        ))
        .id();
    let camera = camera_entity(&mut app);
    app.world_mut().entity_mut(camera).insert(ChildOf(parent));
    app.update();

    // The camera really is parented, and its global transform really has moved
    // away from its local one, which is exactly the silent error being refused.
    assert_eq!(
        app.world().get::<ChildOf>(camera).map(ChildOf::parent),
        Some(parent)
    );
    let local = *app
        .world()
        .get::<Transform>(camera)
        .expect("the camera carries a Transform");
    let global = *app
        .world()
        .get::<GlobalTransform>(camera)
        .expect("the camera carries a GlobalTransform");
    assert!(
        global.translation().distance(local.translation) > 1.0,
        "the parent must really displace the camera, got {global:?} against {local:?}"
    );

    let report = hud_report(&app);
    assert!(
        report.errors.contains(&HudError::NoCamera),
        "a parented camera is an unusable camera, got {:?}",
        report.errors
    );
    assert_eq!(report.viewport, Vec2::ZERO);
    for badge in &report.badges {
        if badge.kind.is_some() {
            assert_eq!(badge.visibility, BadgeVisibility::NoCamera);
            assert_eq!(badge.anchor, None, "nothing was projected");
            assert_eq!(badge.center, None);
        }
    }
    for (rack, entity) in rack_badge_nodes(&mut app) {
        assert!(!ui_displayed(&app, entity), "badge {rack} survived");
    }
    for (rack, entity) in rack_leader_lines(&mut app) {
        assert!(!ui_displayed(&app, entity), "leader {rack} survived");
    }

    // Unparenting restores the whole badge pass.
    app.world_mut().entity_mut(camera).remove::<ChildOf>();
    app.update();
    let report = hud_report(&app);
    assert!(report.is_healthy(), "{:?}", report.errors);
    assert!(!report.shown_badges().is_empty());
}

/// Checks every badge against the real projection of its own anchor and
/// returns how many drawn badges it verified.
fn assert_badges_match_the_real_projection(app: &mut App) -> usize {
    let viewport = viewport_size(app);
    let report = hud_report(app);
    assert_eq!(report.viewport, viewport);
    assert!(report.is_healthy(), "{:?}", report.errors);

    let mut checked = 0usize;
    for badge in &report.badges {
        let projected = viewport_of_world(app, badge.anchor_world);
        let visible = on_screen(projected, viewport);
        let Some(kind) = badge.kind else {
            assert_eq!(badge.visibility, BadgeVisibility::NoTicket);
            continue;
        };
        assert_eq!(kind, BadgeKind::Fault);
        assert_eq!(
            badge.anchor.expect("a projected badge records its anchor"),
            projected,
            "rack {} anchored somewhere other than the real projection",
            badge.rack
        );
        assert_eq!(
            badge.visibility == BadgeVisibility::Shown,
            visible,
            "rack {} visibility disagrees with its projected anchor {projected:?}",
            badge.rack
        );
        if !visible {
            continue;
        }
        checked += 1;

        let entity = rack_badge_nodes(app)
            .into_iter()
            .find(|(rack, _)| *rack == badge.rack)
            .expect("a shown badge has a node")
            .1;
        assert!(ui_displayed(app, entity));
        let rect = ui_rect(app, entity);
        assert_eq!(
            rect.size(),
            Vec2::new(BADGE_WIDTH, BADGE_HEIGHT),
            "badges are fixed size in screen space"
        );
        assert!(
            rect.min.x >= 0.0
                && rect.min.y >= 0.0
                && rect.max.x <= viewport.x
                && rect.max.y <= viewport.y,
            "rack {} badge left the viewport: {rect:?}",
            badge.rack
        );
        let center = badge.center.expect("a shown badge records its centre");
        assert!(
            rect.center().distance(center) < 0.75,
            "rack {} badge laid out at {:?}, report says {center:?}",
            badge.rack,
            rect.center()
        );

        // The leader line really ends on the projected rack anchor.
        let leader = rack_leader_lines(app)
            .into_iter()
            .find(|(rack, _)| *rack == badge.rack)
            .expect("a shown badge has a leader")
            .1;
        assert!(ui_displayed(app, leader), "rack {} leader", badge.rack);
        let node = *app
            .world()
            .get::<ComputedNode>(leader)
            .expect("the leader carries a ComputedNode");
        let transform = *app
            .world()
            .get::<UiGlobalTransform>(leader)
            .expect("the leader carries a UiGlobalTransform");
        let scale = node.inverse_scale_factor;
        let tip = transform.transform_point2(Vec2::new(0.0, node.size.y * 0.5)) * scale;
        assert!(
            tip.distance(projected) < 1.5,
            "rack {} leader ends at {tip:?} instead of the anchor {projected:?}",
            badge.rack
        );
        assert!(
            (node.size.x * scale - LEADER_WIDTH).abs() < 0.75,
            "leader lines stay thin, got {}",
            node.size.x * scale
        );
    }
    checked
}

#[test]
fn operations_hud_badges_track_the_projected_rack_at_every_heading_and_mid_tween() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);
    center_player(&mut app);

    let mut samples = 0usize;
    let mut headings = Vec::new();
    for _ in 0..4 {
        assert!(orbit(&app).is_settled(), "each sweep step starts settled");
        headings.push(orbit(&app).heading());
        samples += assert_badges_match_the_real_projection(&mut app);

        // A genuine mid-tween frame, where the camera is between headings.
        tap(&mut app, &[KeyCode::KeyE]);
        pump(&mut app, QUARTER_TURN_FRAMES / 2);
        assert!(!orbit(&app).is_settled(), "this sample must be mid-tween");
        samples += assert_badges_match_the_real_projection(&mut app);

        pump(&mut app, QUARTER_TURN_FRAMES);
        assert!(orbit(&app).is_settled(), "the quarter turn must complete");
    }
    assert_eq!(
        headings,
        vec![
            CameraHeading::NorthEast,
            CameraHeading::SouthEast,
            CameraHeading::SouthWest,
            CameraHeading::NorthWest,
        ],
        "the sweep must visit every heading"
    );
    assert!(
        samples >= 16,
        "the sweep must have checked real badges, got {samples}"
    );
}

#[test]
fn operations_hud_clamps_an_edge_badge_and_still_points_its_leader_at_the_rack() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);
    let viewport = viewport_size(&mut app);
    let edge = BADGE_WIDTH * 0.5 + 6.0;

    // Walk the technician until a faulted rack's anchor sits close enough to a
    // viewport edge that the fixed badge box has to be clamped sideways.
    let mut found = None;
    'search: for step in 0..80 {
        let along = -18.0 + step as f32 * 0.45;
        for lateral in [-18.0f32, -9.0, 0.0, 9.0, 18.0] {
            place_player(&mut app, Vec2::new(lateral, along));
            hold(&mut app, &[]);
            pump(&mut app, 2);
            let report = hud_report(&app);
            for badge in &report.badges {
                if badge.visibility != BadgeVisibility::Shown {
                    continue;
                }
                let anchor = badge.anchor.expect("a shown badge records its anchor");
                if anchor.x < edge || anchor.x > viewport.x - edge {
                    found = Some(badge.rack);
                    break 'search;
                }
            }
        }
    }
    let rack = found.expect("no player position pushed a badge against a side edge");

    let report = hud_report(&app);
    let badge = *report.badge(rack).expect("badge");
    let anchor = badge.anchor.expect("anchor");
    let center = badge.center.expect("centre");
    assert_ne!(
        center.x, anchor.x,
        "an edge badge must be clamped sideways, so its leader is not vertical"
    );

    let entity = rack_badge_nodes(&mut app)
        .into_iter()
        .find(|(index, _)| *index == rack)
        .expect("badge node")
        .1;
    let rect = ui_rect(&app, entity);
    assert_eq!(rect.size(), Vec2::new(BADGE_WIDTH, BADGE_HEIGHT));
    assert!(
        rect.min.x >= 0.0
            && rect.min.y >= 0.0
            && rect.max.x <= viewport.x
            && rect.max.y <= viewport.y,
        "the clamped badge {rect:?} still left the viewport {viewport:?}"
    );

    let leader = rack_leader_lines(&mut app)
        .into_iter()
        .find(|(index, _)| *index == rack)
        .expect("leader")
        .1;
    assert!(ui_displayed(&app, leader));
    let node = *app
        .world()
        .get::<ComputedNode>(leader)
        .expect("ComputedNode");
    let transform = *app
        .world()
        .get::<UiGlobalTransform>(leader)
        .expect("UiGlobalTransform");
    let scale = node.inverse_scale_factor;
    let tip = transform.transform_point2(Vec2::new(0.0, node.size.y * 0.5)) * scale;
    assert!(
        tip.distance(anchor) < 1.5,
        "the tilted leader ends at {tip:?} instead of the anchor {anchor:?}"
    );
    assert!(hud_report(&app).is_healthy());
}

#[test]
fn operations_hud_hides_a_badge_whose_rack_leaves_the_viewport() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);

    // Walk the camera to the far corner so the west rack rows leave the view.
    place_player(&mut app, Vec2::new(18.0, 18.0));
    hold(&mut app, &[]);
    pump(&mut app, 4);

    let viewport = viewport_size(&mut app);
    let report = hud_report(&app);
    let mut hidden = 0usize;
    for badge in &report.badges {
        let Some(_) = badge.kind else { continue };
        let projected = viewport_of_world(&mut app, badge.anchor_world);
        let on_screen =
            (0.0..=viewport.x).contains(&projected.x) && (0.0..=viewport.y).contains(&projected.y);
        let entity = rack_badge_nodes(&mut app)
            .into_iter()
            .find(|(rack, _)| *rack == badge.rack)
            .expect("badge node")
            .1;
        if on_screen {
            assert_eq!(badge.visibility, BadgeVisibility::Shown);
            assert!(ui_displayed(&app, entity));
        } else {
            hidden += 1;
            assert_eq!(
                badge.visibility,
                BadgeVisibility::OffScreen,
                "rack {} projected to {projected:?}",
                badge.rack
            );
            assert!(!ui_displayed(&app, entity), "rack {} badge", badge.rack);
            let leader = rack_leader_lines(&mut app)
                .into_iter()
                .find(|(rack, _)| *rack == badge.rack)
                .expect("leader")
                .1;
            assert!(!ui_displayed(&app, leader));
        }
    }
    assert!(
        hidden > 0,
        "the corner sample must push at least one faulted rack off screen"
    );
    assert!(report.is_healthy(), "an off-screen rack is not a failure");
}

#[test]
fn operations_hud_panels_stay_on_screen_and_clear_of_the_play_rectangle() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);

    for (width, height) in [
        (DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT),
        (VERIFICATION_WINDOW_WIDTH, VERIFICATION_WINDOW_HEIGHT),
    ] {
        resize_window(&mut app, width as f32, height as f32);
        let viewport = viewport_size(&mut app);
        assert_eq!(viewport, Vec2::new(width as f32, height as f32));
        assert_eq!(hud_report(&app).viewport, viewport);

        let play = play_rectangle(viewport);
        let queue_panel = hud_single::<TicketQueuePanel>(&mut app);
        let controls_panel = hud_single::<ControlsPanel>(&mut app);
        let queue = ui_rect(&app, queue_panel);
        let controls = ui_rect(&app, controls_panel);
        for (name, rect) in [("queue", queue), ("controls", controls)] {
            assert!(
                rect.min.x >= HUD_MARGIN - 0.5
                    && rect.min.y >= HUD_MARGIN - 0.5
                    && rect.max.x <= viewport.x - HUD_MARGIN + 0.5
                    && rect.max.y <= viewport.y - HUD_MARGIN + 0.5,
                "the {name} panel {rect:?} broke the {HUD_MARGIN} px margin at {viewport:?}"
            );
            assert!(rect.width() > 0.0 && rect.height() > 0.0, "{name} is empty");
            assert!(
                rect.intersect(play).is_empty(),
                "the {name} panel {rect:?} covers the play rectangle {play:?}"
            );
        }
        assert!(
            queue.max.x <= play.min.x,
            "the queue stack {queue:?} must stay left of the play rectangle {play:?}"
        );
        assert!(
            controls.min.y >= play.max.y,
            "the control strip {controls:?} must stay below the play rectangle {play:?}"
        );
        assert_eq!(queue.min, Vec2::splat(HUD_MARGIN), "at {viewport:?}");
        assert!(
            (controls.max - (viewport - Vec2::splat(HUD_MARGIN)))
                .abs()
                .max_element()
                < 0.5,
            "the control strip must stay pinned to the bottom-right, got {controls:?}"
        );

        // The rows are still ordered, still on screen, and still inside their
        // panel.
        let report = hud_report(&app);
        assert_eq!(report.rows.len(), MAX_ACTIVE_TICKETS);
        let mut previous = f32::NEG_INFINITY;
        for (slot, entity) in queue_rows_by_slot(&mut app) {
            assert!(ui_displayed(&app, entity), "row {slot} vanished");
            let rect = ui_rect(&app, entity);
            assert!(rect.min.y > previous, "row {slot} is out of order");
            previous = rect.min.y;
            assert!(
                queue.contains(rect.min) && queue.contains(rect.max),
                "row {slot} {rect:?} escaped its panel {queue:?}"
            );
            assert!(rect.intersect(play).is_empty());
        }

        // Every visible badge is still fully on screen at this size.
        for badge in &report.badges {
            if badge.visibility != BadgeVisibility::Shown {
                continue;
            }
            let entity = rack_badge_nodes(&mut app)
                .into_iter()
                .find(|(rack, _)| *rack == badge.rack)
                .expect("badge node")
                .1;
            let rect = ui_rect(&app, entity);
            assert_eq!(rect.size(), Vec2::new(BADGE_WIDTH, BADGE_HEIGHT));
            assert!(
                rect.min.x >= 0.0
                    && rect.min.y >= 0.0
                    && rect.max.x <= viewport.x
                    && rect.max.y <= viewport.y,
                "rack {} badge {rect:?} left the {viewport:?} viewport",
                badge.rack
            );
        }
    }
}

#[test]
fn operations_hud_reports_a_rack_that_lost_its_operational_state() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);
    let faulted = ticket_queue(&app).ordered()[0].rack;
    assert_eq!(
        hud_report(&app).badge(faulted).expect("badge").kind,
        Some(BadgeKind::Fault)
    );

    let entity = roster(&app).get(faulted).expect("rack entry").entity;
    app.world_mut()
        .entity_mut(entity)
        .remove::<RackOperations>();
    app.update();

    let report = hud_report(&app);
    assert!(
        report
            .errors
            .contains(&HudError::MissingRackState { rack: faulted }),
        "a rack the HUD cannot read must be reported, got {:?}",
        report.errors
    );
    let badge = report.badge(faulted).expect("badge");
    assert_eq!(badge.visibility, BadgeVisibility::MissingRack);
    assert_eq!(badge.kind, None);
    let node = rack_badge_nodes(&mut app)
        .into_iter()
        .find(|(rack, _)| *rack == faulted)
        .expect("badge node")
        .1;
    assert!(!ui_displayed(&app, node));
    assert!(!report.is_healthy());
}

#[test]
fn operations_hud_reports_a_missing_camera_instead_of_drawing_stale_badges() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);
    assert!(!hud_report(&app).shown_badges().is_empty());

    // Removing the marker is the fault the HUD must survive: the game camera
    // it projects through is gone. Despawning the whole entity would tear the
    // render world down instead, which is a different failure.
    let camera = camera_entity(&mut app);
    app.world_mut()
        .entity_mut(camera)
        .remove::<CellShiftCamera>();
    app.update();

    let report = hud_report(&app);
    assert!(
        report.errors.contains(&HudError::NoCamera),
        "got {:?}",
        report.errors
    );
    assert_eq!(report.viewport, Vec2::ZERO);
    for badge in &report.badges {
        if badge.kind.is_some() {
            assert_eq!(badge.visibility, BadgeVisibility::NoCamera);
            assert_eq!(badge.anchor, None);
            assert_eq!(badge.center, None);
        }
    }
    for (rack, entity) in rack_badge_nodes(&mut app) {
        assert!(!ui_displayed(&app, entity), "badge {rack} survived");
    }
    // The queue stack still reads the live queue: losing the camera hides
    // badges, not tickets.
    assert_eq!(report.rows.len(), MAX_ACTIVE_TICKETS);
}

#[test]
fn operations_hud_draws_only_typed_palette_colors() {
    let mut app = hud_app(&repo_assets());
    fill_queue(&mut app);
    let spot = repair_spot(&app, ticket_queue(&app).ordered()[0].rack);
    place_player(&mut app, spot);
    hold(&mut app, &[]);
    app.update();
    press_space(&mut app);
    pump(&mut app, 2);

    let root = hud_single::<HudRoot>(&mut app);
    let mut stack = vec![(root, true)];
    let mut seen = 0usize;
    let mut text_nodes = 0usize;
    while let Some((entity, inherited)) = stack.pop() {
        let visible = inherited && ui_displayed(&app, entity);
        if let Some(children) = app.world().get::<Children>(entity) {
            stack.extend(children.iter().map(|child| (child, visible)));
        }
        let mut colors: Vec<Srgba> = Vec::new();
        if let Some(background) = app.world().get::<BackgroundColor>(entity) {
            colors.push(background.0.to_srgba());
        }
        if let Some(border) = app.world().get::<BorderColor>(entity) {
            colors.extend(
                [border.top, border.right, border.bottom, border.left]
                    .map(|color| color.to_srgba()),
            );
        }
        if let Some(text) = app.world().get::<TextColor>(entity) {
            colors.push(text.0.to_srgba());
        }
        for color in colors {
            if color.alpha == 0.0 {
                continue;
            }
            seen += 1;
            assert!(
                PaletteRole::ALL
                    .iter()
                    .any(|role| role.color().to_vec3() == color.to_vec3()),
                "the HUD drew {color:?}, which is not a typed PaletteRole colour"
            );
        }
        // Every label the HUD renders must actually produce glyphs.
        if app.world().get::<Text>(entity).is_some() {
            let text = ui_text(&app, entity);
            let node = app
                .world()
                .get::<ComputedNode>(entity)
                .expect("a text node carries a ComputedNode");
            if !text.is_empty() && visible {
                text_nodes += 1;
                assert!(
                    node.content_size.x > 0.0 && node.content_size.y > 0.0,
                    "{text:?} rendered no glyphs at all"
                );
            }
        }
    }
    assert!(seen > 20, "the walk only found {seen} colours");
    assert!(text_nodes >= 12, "the walk only found {text_nodes} labels");
}

#[test]
fn operations_hud_control_strip_names_every_reviewed_key() {
    let mut app = hud_app(&repo_assets());
    let caps = control_caps(&mut app);
    assert_eq!(caps.len(), HudControl::ALL.len());

    let mut labelled = app
        .world_mut()
        .query::<(&ControlHintCapLabel, &Text)>()
        .iter(app.world())
        .map(|(label, text)| (label.control, text.0.clone()))
        .collect::<Vec<_>>();
    labelled.sort_by_key(|(control, _)| format!("{control:?}"));
    let mut expected = HudControl::ALL
        .map(|control| (control, control.keys().to_owned()))
        .to_vec();
    expected.sort_by_key(|(control, _)| format!("{control:?}"));
    assert_eq!(labelled, expected);

    // The strip is one compact row pinned to the bottom-right corner.
    let panel_entity = hud_single::<ControlsPanel>(&mut app);
    let panel = ui_rect(&app, panel_entity);
    assert!(
        panel.height() <= CONTROLS_PANEL_HEIGHT + 0.5,
        "the control strip grew to {}",
        panel.height()
    );
    for (control, entity) in caps {
        let cap = ui_rect(&app, entity);
        assert!(
            panel.contains(cap.min) && panel.contains(cap.max),
            "{control:?} escaped the strip"
        );
        assert_eq!(ui_background(&app, entity), control.cap_role(false).color());
    }
}
