//! The autonomous verification contract.
//!
//! This file owns three layers of proof:
//!
//! 1. pure contracts over the verification output directory, the stage machine,
//!    the canonical semantic report, and the image analyzers;
//! 2. generated negative fixtures proving each analyzer family rejects the
//!    frame it is supposed to reject;
//! 3. one end-to-end run of the real compiled game under `--verify-output`,
//!    whose 14 real frames must pass every mandatory metric.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bevy::color::ColorToPacked;
use image::RgbImage;
use midcreek_cs_1::{
    camera::{CEL_SHIFT_DEBAND_DITHER, CEL_SHIFT_TONEMAPPING},
    design::{
        AssetKind, CHARACTER_SHEET_REFERENCE_PATH, CHARACTER_SHEET_SHA256, FLOOR_LIGHT,
        KEY_ART_REFERENCE_PATH, KEY_ART_SHA256, PaletteRole, SceneBlueprint,
    },
    player::required_player_parts,
    verification::{
        APP_WATCHDOG, ARTIFACT_NAMES, BadgeFacts, BlueprintFacts, CLIP_DIFFERENCE_RANGE,
        CameraRenderFacts, EQUIPMENT_REGION_MIN_PIXELS, EQUIPMENT_ROLE_MIN, EquipmentCategory,
        EquipmentFacts, FrameFacts, FrameMetrics, FrameName, GameplayFacts, HudRowFacts,
        OUTSIDE_CROP_MAX, PixelRect, REPORT_FILE_NAME, RectFacts, SENTINEL_CLEAR, SENTINEL_MAX,
        StageError, StageMachine, VERIFICATION_MSAA, VerificationFault, VerificationReport,
        VerificationRequest, VerificationStage, VerifyOutput, VerifyOutputError, WORKER_REGION,
        axis_aligned_fixture, badge_region, black_fixture, blank_hud_fixture, canonical_f64,
        canonical_float, canonical_json, clip_difference, equipment_region, evaluate_frame,
        flood_bytes, frame_regions, gradient_noise_fixture, hud_region, magenta_border_fixture,
        missing_badge_fixture, missing_worker_fixture, outside_crop_change,
        parse_verification_args, reference_metrics, semantic_hash, synthetic_badges,
        synthetic_frame, synthetic_hud_panel, synthetic_worker_crop,
    },
};
use sha2::{Digest, Sha256};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A temporary directory this test owns outright and removes on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "midcreek-render-contract-{}-{label}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the test owns its own temporary directory");
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Output directory safety
// ---------------------------------------------------------------------------

#[test]
fn verify_output_accepts_an_existing_writable_directory() {
    let temp = TempDir::new("accepts");
    let output = VerifyOutput::prepare(temp.path()).expect("an owned empty directory is legal");

    assert_eq!(output.root(), temp.path());
    assert!(
        temp.path().is_dir(),
        "preparing an output directory must never remove it"
    );
}

#[test]
fn verify_output_creates_only_the_final_missing_component() {
    let temp = TempDir::new("creates-leaf");
    let leaf = temp.join("frames");
    VerifyOutput::prepare(&leaf).expect("one missing leaf is legal");
    assert!(leaf.is_dir());

    let deep = temp.join("missing/parent/frames");
    assert_eq!(
        VerifyOutput::prepare(&deep),
        Err(VerifyOutputError::MissingParent {
            parent: temp.join("missing/parent")
        }),
        "a typo two directories deep must fail loudly instead of being created"
    );
    assert!(!temp.join("missing").exists());
}

#[test]
fn verify_output_rejects_an_empty_path() {
    assert_eq!(
        VerifyOutput::prepare(Path::new("")),
        Err(VerifyOutputError::Empty)
    );
}

#[test]
fn verify_output_rejects_parent_traversal() {
    let temp = TempDir::new("traversal");
    let escape = temp.join("..").join("midcreek-escape");
    assert_eq!(
        VerifyOutput::prepare(&escape),
        Err(VerifyOutputError::ParentTraversal { path: escape })
    );
}

#[test]
fn verify_output_rejects_the_filesystem_root() {
    assert_eq!(
        VerifyOutput::prepare(Path::new("/")),
        Err(VerifyOutputError::RefusedRoot {
            path: PathBuf::from("/")
        })
    );
}

#[test]
fn verify_output_rejects_a_path_that_is_not_a_directory() {
    let temp = TempDir::new("not-a-directory");
    let file = temp.join("report.json");
    fs::write(&file, b"{}").expect("the test owns this file");
    assert_eq!(
        VerifyOutput::prepare(&file),
        Err(VerifyOutputError::NotADirectory { path: file })
    );
}

#[test]
fn verify_output_rejects_a_symbolic_link() {
    let temp = TempDir::new("symlink");
    let real = temp.join("real");
    fs::create_dir_all(&real).expect("the test owns this directory");
    let link = temp.join("link");
    std::os::unix::fs::symlink(&real, &link).expect("the test owns this link");

    assert_eq!(
        VerifyOutput::prepare(&link),
        Err(VerifyOutputError::SymbolicLink { path: link })
    );
}

#[test]
fn verify_output_rejects_an_unwritable_directory() {
    let temp = TempDir::new("unwritable");
    let locked = temp.join("locked");
    fs::create_dir_all(&locked).expect("the test owns this directory");
    let mut permissions = fs::metadata(&locked)
        .expect("the directory exists")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o500);
    fs::set_permissions(&locked, permissions).expect("the test owns this directory");

    let error = VerifyOutput::prepare(&locked).expect_err("a read-only directory is unwritable");
    assert!(
        matches!(error, VerifyOutputError::Unwritable { .. }),
        "expected an unwritable error, got {error:?}"
    );

    let mut restore = fs::metadata(&locked)
        .expect("the directory exists")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut restore, 0o700);
    let _ = fs::set_permissions(&locked, restore);
}

#[test]
fn verify_output_rejects_an_artifact_name_that_is_not_a_regular_file() {
    let temp = TempDir::new("artifact-kind");
    let hostile = temp.join(REPORT_FILE_NAME);
    fs::create_dir_all(&hostile).expect("the test owns this directory");

    assert_eq!(
        VerifyOutput::prepare(temp.path()),
        Err(VerifyOutputError::UnsafeArtifact {
            name: REPORT_FILE_NAME.to_owned(),
            path: hostile
        })
    );
}

#[test]
fn verify_output_clears_only_its_own_named_artifacts() {
    let temp = TempDir::new("owned-artifacts");
    let keeper = temp.join("someone-elses-notes.txt");
    fs::write(&keeper, b"keep me").expect("the test owns this file");
    let stale = temp.join(REPORT_FILE_NAME);
    fs::write(&stale, b"stale").expect("the test owns this file");
    let stale_frame = temp.join(FrameName::HealthyCenterNorthEast.file_name());
    fs::write(&stale_frame, b"stale").expect("the test owns this file");

    let output = VerifyOutput::prepare(temp.path()).expect("the directory is legal");
    output
        .clear()
        .expect("clearing named artifacts must succeed");

    assert!(
        keeper.is_file(),
        "an unrelated file must survive: {} was removed",
        keeper.display()
    );
    assert_eq!(fs::read_to_string(&keeper).unwrap(), "keep me");
    assert!(!stale.exists(), "a stale report must be removed");
    assert!(!stale_frame.exists(), "a stale frame must be removed");
    assert!(temp.path().is_dir(), "the directory itself must survive");
}

#[test]
fn verify_output_names_exactly_the_fifteen_owned_artifacts() {
    assert_eq!(
        ARTIFACT_NAMES.len(),
        FrameName::ALL.len() + 1,
        "the owned set is the 14 frames plus the canonical report"
    );
    assert!(ARTIFACT_NAMES.contains(&REPORT_FILE_NAME));
    for frame in FrameName::ALL {
        assert!(
            ARTIFACT_NAMES.contains(&frame.file_name()),
            "{frame:?} must be an owned artifact"
        );
    }
    let mut sorted = ARTIFACT_NAMES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ARTIFACT_NAMES.len(),
        "artifact names must be unique"
    );
}

#[test]
fn verify_output_artifact_paths_stay_inside_the_prepared_root() {
    let temp = TempDir::new("artifact-paths");
    let output = VerifyOutput::prepare(temp.path()).expect("the directory is legal");
    for name in ARTIFACT_NAMES {
        let path = output.artifact(name);
        assert_eq!(path.parent(), Some(temp.path()));
        assert_eq!(path.file_name().and_then(|name| name.to_str()), Some(name));
    }
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

#[test]
fn frame_names_cover_the_fourteen_reviewed_captures_with_stable_files() {
    assert_eq!(FrameName::ALL.len(), 14);
    let expected = [
        (
            FrameName::HealthyCenterNorthEast,
            "01-healthy-center-ne.png",
            1280,
            720,
        ),
        (
            FrameName::FaultQueueNorthEast,
            "02-fault-queue-ne.png",
            1280,
            720,
        ),
        (FrameName::WalkNorthEast, "03-walk-ne.png", 1280, 720),
        (
            FrameName::RepairingNorthEast,
            "04-repairing-ne.png",
            1280,
            720,
        ),
        (
            FrameName::ResolvedNorthEast,
            "05-resolved-ne.png",
            1280,
            720,
        ),
        (FrameName::SettledSouthEast, "06-settled-se.png", 1280, 720),
        (FrameName::SettledSouthWest, "07-settled-sw.png", 1280, 720),
        (FrameName::SettledNorthWest, "08-settled-nw.png", 1280, 720),
        (FrameName::MidOrbit, "09-mid-orbit.png", 1280, 720),
        (FrameName::CornerNorthEast, "10-corner-ne.png", 1280, 720),
        (FrameName::CornerSouthEast, "11-corner-se.png", 1280, 720),
        (FrameName::CornerSouthWest, "12-corner-sw.png", 1280, 720),
        (FrameName::CornerNorthWest, "13-corner-nw.png", 1280, 720),
        (
            FrameName::LowResolutionQueue,
            "14-low-resolution-queue.png",
            960,
            540,
        ),
    ];
    for (index, (frame, file, width, height)) in expected.into_iter().enumerate() {
        assert_eq!(FrameName::ALL[index], frame);
        assert_eq!(frame.file_name(), file);
        assert_eq!(frame.size(), (width, height), "{frame:?}");
    }
}

// ---------------------------------------------------------------------------
// Stage machine
// ---------------------------------------------------------------------------

#[test]
fn verification_stages_walk_the_documented_order_exactly_once() {
    let expected = [
        VerificationStage::Boot,
        VerificationStage::WaitForAssets,
        VerificationStage::ValidateBlueprint,
        VerificationStage::HealthyCapture,
        VerificationStage::SeedThreeFaults,
        VerificationStage::FaultQueueCapture,
        VerificationStage::KeyboardJourney,
        VerificationStage::WalkCapture,
        VerificationStage::BeginRepair,
        VerificationStage::RepairCapture,
        VerificationStage::CompleteRepair,
        VerificationStage::ResolvedCapture,
        VerificationStage::OrbitSouthEast,
        VerificationStage::SettledSouthEastCapture,
        VerificationStage::OrbitSouthWest,
        VerificationStage::SettledSouthWestCapture,
        VerificationStage::OrbitNorthWest,
        VerificationStage::SettledNorthWestCapture,
        VerificationStage::MidOrbitCapture,
        VerificationStage::CornerProbes,
        VerificationStage::LowResolutionCapture,
        VerificationStage::AnalyzeReady,
        VerificationStage::WriteReport,
        VerificationStage::Success,
        VerificationStage::Failure,
    ];
    assert_eq!(VerificationStage::ALL.to_vec(), expected.to_vec());

    let mut walked = vec![VerificationStage::Boot];
    let mut stage = VerificationStage::Boot;
    while let Some(next) = stage.next() {
        walked.push(next);
        stage = next;
    }
    assert_eq!(stage, VerificationStage::Success);
    assert_eq!(walked.len(), VerificationStage::ALL.len() - 1);
    assert!(!walked.contains(&VerificationStage::Failure));
}

#[test]
fn verification_stage_terminals_have_no_successor() {
    assert_eq!(VerificationStage::Success.next(), None);
    assert_eq!(VerificationStage::Failure.next(), None);
    assert!(VerificationStage::Success.is_terminal());
    assert!(VerificationStage::Failure.is_terminal());
    for stage in VerificationStage::ALL {
        assert_eq!(
            stage.is_terminal(),
            stage.next().is_none(),
            "{stage:?} disagrees about being terminal"
        );
    }
}

#[test]
fn verification_stage_names_are_stable_and_unique() {
    let mut names = VerificationStage::ALL
        .iter()
        .map(|stage| stage.name())
        .collect::<Vec<_>>();
    assert_eq!(names[0], "boot");
    assert_eq!(names[names.len() - 1], "failure");
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), VerificationStage::ALL.len());
}

#[test]
fn verification_stage_capture_frames_cover_every_named_frame_once() {
    let mut frames = Vec::new();
    for stage in VerificationStage::ALL {
        frames.extend(stage.frames());
    }
    assert_eq!(
        frames,
        FrameName::ALL.to_vec(),
        "the stage machine must capture all 14 frames in declaration order"
    );
}

#[test]
fn verification_machine_accepts_every_legal_transition() {
    let mut machine = StageMachine::default();
    assert_eq!(machine.stage(), VerificationStage::Boot);
    let mut walked = vec![VerificationStage::Boot];
    while machine.stage() != VerificationStage::Success {
        let stage = machine.advance().expect("the documented order is legal");
        walked.push(stage);
    }
    assert_eq!(walked.len(), VerificationStage::ALL.len() - 1);
    assert_eq!(machine.visited(), walked.as_slice());
}

#[test]
fn verification_machine_rejects_every_illegal_transition() {
    for from in VerificationStage::ALL {
        for to in VerificationStage::ALL {
            let legal = from.next() == Some(to)
                || (to == VerificationStage::Failure && !from.is_terminal());
            let mut machine = StageMachine::at(from);
            let result = machine.transition(to);
            if legal {
                assert_eq!(result, Ok(()), "{from:?} -> {to:?} must be legal");
                assert_eq!(machine.stage(), to);
            } else {
                let expected = if from.is_terminal() {
                    StageError::AlreadyTerminal { stage: from }
                } else {
                    StageError::IllegalTransition { from, to }
                };
                assert_eq!(result, Err(expected), "{from:?} -> {to:?} must be illegal");
                assert_eq!(
                    machine.stage(),
                    from,
                    "a rejected transition must not move the machine"
                );
            }
        }
    }
}

#[test]
fn verification_machine_records_the_failure_stage_and_reason() {
    let mut machine = StageMachine::at(VerificationStage::RepairCapture);
    machine.fail("the screenshot callback never returned");

    assert_eq!(machine.stage(), VerificationStage::Failure);
    assert_eq!(
        machine.failure(),
        Some(("repair-capture", "the screenshot callback never returned"))
    );
    assert_eq!(
        machine.advance(),
        Err(StageError::AlreadyTerminal {
            stage: VerificationStage::Failure
        }),
        "a failed run must never resume"
    );
}

#[test]
fn verification_machine_refuses_to_fail_twice() {
    let mut machine = StageMachine::at(VerificationStage::WalkCapture);
    machine.fail("first cause");
    machine.fail("second cause");
    assert_eq!(
        machine.failure(),
        Some(("walk-capture", "first cause")),
        "the first real cause must survive"
    );
}

// ---------------------------------------------------------------------------
// Canonical report
// ---------------------------------------------------------------------------

#[test]
fn canonical_floats_round_to_the_documented_grid() {
    assert_eq!(canonical_f64(0.123_456_789), 0.123_457);
    assert_eq!(canonical_f64(-0.000_000_4), 0.0);
    assert_eq!(canonical_f64(f64::NAN), 0.0);
    assert_eq!(canonical_f64(f64::INFINITY), 0.0);
    assert_eq!(canonical_float(-0.0), 0.0);
    assert_eq!(canonical_float(1.0 / 3.0), 0.333_333);
}

#[test]
fn canonical_json_sorts_every_map_and_ends_with_one_newline() {
    let mut report = sample_report();
    report
        .assets
        .insert("assets/generated/zeta.glb".to_owned(), "ff".to_owned());
    report
        .assets
        .insert("assets/generated/alpha.glb".to_owned(), "aa".to_owned());
    let text = canonical_json(&report);

    assert!(text.ends_with("}\n"));
    let alpha = text.find("alpha.glb").expect("alpha is present");
    let zeta = text.find("zeta.glb").expect("zeta is present");
    assert!(alpha < zeta, "asset keys must be sorted");
    let parsed: VerificationReport =
        serde_json::from_str(&text).expect("the canonical report round-trips");
    assert_eq!(parsed, report);
}

#[test]
fn canonical_report_carries_no_wall_clock_host_or_absolute_path() {
    let text = canonical_json(&sample_report());
    for banned in [
        "timestamp",
        "generated_at",
        "elapsed",
        "duration_ms",
        "hostname",
        "user",
        "/Users/",
        "/home/",
        "/tmp/",
        "C:\\",
    ] {
        assert!(
            !text.contains(banned),
            "the canonical report must not carry {banned}"
        );
    }
}

#[test]
fn semantic_hash_is_stable_and_sensitive() {
    let report = sample_report();
    let canonical = canonical_json(&report);
    assert_eq!(semantic_hash(&canonical), semantic_hash(&canonical));
    assert_eq!(semantic_hash(&canonical).len(), 64);

    let mut changed = report.clone();
    changed.gameplay.tickets_emitted += 1;
    assert_ne!(
        semantic_hash(&canonical),
        semantic_hash(&canonical_json(&changed)),
        "a different simulation must hash differently"
    );
}

#[test]
fn report_frame_paths_are_relative_file_names() {
    let report = sample_report();
    for (name, facts) in &report.frames {
        assert_eq!(&facts.path, name);
        assert!(
            !Path::new(&facts.path).is_absolute(),
            "{name} must be a relative path"
        );
        assert!(
            !facts.path.contains('/') && !facts.path.contains('\\'),
            "{name} must be a bare file name"
        );
    }
}

fn sample_report() -> VerificationReport {
    let facts = FrameFacts {
        path: FrameName::HealthyCenterNorthEast.file_name().to_owned(),
        width: 1280,
        height: 720,
        stage: "healthy-capture".to_owned(),
        heading: "north-east".to_owned(),
        camera_yaw_degrees: 45.0,
        camera_settled: true,
        camera_progress: 1.0,
        camera_target: [-6.0, -11.0],
        ground_quadrilateral: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        player_position: [-6.0, -11.0],
        player_clip: "idle".to_owned(),
        movement_locked: false,
        worker_crop: RectFacts {
            x: 600.0,
            y: 300.0,
            width: 40.0,
            height: 90.0,
        },
        hud_status: "all-healthy".to_owned(),
        hud_rows: Vec::new(),
        hud_panels: BTreeMap::from([(
            "queue".to_owned(),
            RectFacts {
                x: 16.0,
                y: 16.0,
                width: 216.0,
                height: 96.0,
            },
        )]),
        badges: Vec::new(),
        tickets: Vec::new(),
        rack_states: vec!["healthy".to_owned(); 4],
        equipment: Vec::new(),
    };
    VerificationReport {
        schema_version: 1,
        result: "success".to_owned(),
        failed_stage: None,
        failure_reason: None,
        stages: vec!["boot".to_owned(), "success".to_owned()],
        assets: BTreeMap::new(),
        asset_sources: BTreeMap::new(),
        references: BTreeMap::new(),
        sources: BTreeMap::new(),
        blueprint: BlueprintFacts {
            room: [40.0, 40.0],
            coverage: [72.0, 72.0],
            visuals: 1,
            colliders: 1,
            rack_rows: 4,
            aisles: 3,
            player_spawn: [-6.0, -11.0],
            walkable_connected: true,
            validation_errors: Vec::new(),
        },
        camera: CameraRenderFacts {
            tonemapping: "None".to_owned(),
            deband_dither: "Disabled".to_owned(),
            msaa_samples: 1,
            clear_color: "#FF00FF".to_owned(),
        },
        gameplay: GameplayFacts {
            fault_seed: "0xce115a1fda7ace01".to_owned(),
            fixed_step_seconds: 0.016_667,
            ticket_history: Vec::new(),
            interactions: Vec::new(),
            keys: Vec::new(),
            tickets_emitted: 3,
            capacity_pauses: 0,
            duplicate_pauses: 0,
            busy_pauses: 0,
            rig_parts: vec!["bone-hips".to_owned()],
            repaired_rack: 1,
        },
        frames: BTreeMap::from([(facts.path.clone(), facts)]),
    }
}

// ---------------------------------------------------------------------------
// Analyzer and generated negative fixtures
// ---------------------------------------------------------------------------

const FIXTURE_WIDTH: u32 = 1280;
const FIXTURE_HEIGHT: u32 = 720;

fn synthetic_regions() -> BTreeMap<String, PixelRect> {
    let mut regions = BTreeMap::from([(WORKER_REGION.to_owned(), synthetic_worker_crop())]);
    for (index, (rect, _)) in synthetic_badges().into_iter().enumerate() {
        regions.insert(badge_region(index), rect);
    }
    regions.insert(hud_region("queue"), synthetic_hud_panel());
    regions
}

fn synthetic_facts() -> FrameFacts {
    let badges = synthetic_badges()
        .into_iter()
        .enumerate()
        .map(|(index, (rect, role))| BadgeFacts {
            rack: index,
            kind: match role {
                PaletteRole::FaultRed => "fault",
                PaletteRole::WorkerHardHat => "repairing",
                _ => "resolved",
            }
            .to_owned(),
            visibility: "shown".to_owned(),
            rect: Some(RectFacts {
                x: f64::from(rect.x),
                y: f64::from(rect.y),
                width: f64::from(rect.width),
                height: f64::from(rect.height),
            }),
        })
        .collect();
    let panel = synthetic_hud_panel();
    let mut facts = sample_report()
        .frames
        .remove(FrameName::HealthyCenterNorthEast.file_name())
        .expect("the sample report holds one frame");
    facts.badges = badges;
    facts.hud_rows = vec![HudRowFacts {
        slot: 0,
        ticket: 1,
        rack: 0,
        severity: "critical".to_owned(),
        state: "faulted".to_owned(),
        progress: 0.0,
        label: "T0001 R01 Critical".to_owned(),
    }];
    facts.hud_panels = BTreeMap::from([(
        "queue".to_owned(),
        RectFacts {
            x: f64::from(panel.x),
            y: f64::from(panel.y),
            width: f64::from(panel.width),
            height: f64::from(panel.height),
        },
    )]);
    facts
}

fn metrics_of(image: &RgbImage) -> FrameMetrics {
    FrameMetrics::compute(image, &synthetic_regions())
}

fn failed_metrics(image: &RgbImage) -> Vec<String> {
    evaluate_frame(
        FrameName::HealthyCenterNorthEast,
        &synthetic_facts(),
        &metrics_of(image),
        reference_metrics(),
    )
    .into_iter()
    .map(|failure| failure.metric)
    .collect()
}

fn assert_rejects(fixture: &RgbImage, metric: &str) {
    let failures = failed_metrics(fixture);
    assert!(
        failures.iter().any(|name| name == metric),
        "the analyzer accepted a bad frame: {metric} is missing from {failures:?}"
    );
}

#[test]
fn frame_metrics_measure_one_image_in_a_single_pass() {
    let image = synthetic_frame(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    let metrics = metrics_of(&image);

    assert_eq!(metrics.width, FIXTURE_WIDTH);
    assert_eq!(metrics.height, FIXTURE_HEIGHT);
    assert_eq!(
        metrics.pixels,
        u64::from(FIXTURE_WIDTH) * u64::from(FIXTURE_HEIGHT)
    );
    let total: f64 = PaletteRole::ALL
        .iter()
        .map(|role| metrics.nearest(*role))
        .sum();
    assert!(
        (total - 1.0).abs() < 1.0e-9,
        "the nearest-role histogram must be normalized, got {total}"
    );
    assert_eq!(
        metrics.regions.len(),
        synthetic_regions().len(),
        "every requested region must be measured in the same pass"
    );
    let worker = metrics
        .region(WORKER_REGION)
        .expect("the worker crop is measured");
    assert_eq!(worker.pixels, synthetic_worker_crop().area());
}

#[test]
fn analyzer_accepts_the_synthetic_base_for_every_family_the_fixtures_target() {
    let image = synthetic_frame(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    let failures = failed_metrics(&image);
    for family in [
        "sentinel-ratio",
        "mean-linear-luminance",
        "palette-ratio",
        "floor-ratio",
        "rack-ratio",
        "signature-yellow-ratio",
        "ink-and-hose-ratio",
        "diagonal-edge-band-30-50",
        "diagonal-edge-band-130-150",
        "edge-density-vs-key-art",
        "worker-crop-WorkerHardHat",
        "worker-crop-WorkerHiVis",
        "badge-0-FaultRed",
        "badge-1-WorkerHardHat",
        "badge-2-HealthyGreen",
        "hud-queue-FaultRed",
    ] {
        assert!(
            !failures.contains(&family.to_owned()),
            "the synthetic base must satisfy {family} so its mutation proves rejection; got {failures:?}"
        );
    }
}

#[test]
fn analyzer_rejects_an_all_black_frame() {
    let fixture = black_fixture(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    assert_rejects(&fixture, "mean-linear-luminance");
    assert_rejects(&fixture, "palette-ratio");
    assert_rejects(&fixture, "floor-ratio");
    assert_rejects(&fixture, "edge-density-vs-key-art");
}

#[test]
fn analyzer_rejects_a_gradient_noise_frame() {
    let fixture = gradient_noise_fixture(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    assert_rejects(&fixture, "palette-ratio");
    assert_rejects(&fixture, "floor-ratio");
}

#[test]
fn analyzer_rejects_a_magenta_sentinel_border() {
    let fixture = magenta_border_fixture(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    assert_rejects(&fixture, "sentinel-ratio");
    assert!(
        metrics_of(&fixture).sentinel_ratio > SENTINEL_MAX,
        "the fixture must actually contain sentinel pixels"
    );
}

#[test]
fn analyzer_rejects_an_axis_aligned_only_frame() {
    let fixture = axis_aligned_fixture(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    assert_rejects(&fixture, "diagonal-edge-band-30-50");
    assert_rejects(&fixture, "diagonal-edge-band-130-150");
}

#[test]
fn analyzer_rejects_a_frame_with_the_worker_colours_painted_out() {
    let base = synthetic_frame(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    let fixture = missing_worker_fixture(&base, synthetic_worker_crop());
    assert_rejects(&fixture, "worker-crop-WorkerHardHat");
    assert_rejects(&fixture, "worker-crop-WorkerHiVis");
}

#[test]
fn analyzer_rejects_a_frame_with_the_badges_painted_out() {
    let base = synthetic_frame(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    let rects = synthetic_badges()
        .into_iter()
        .map(|(rect, _)| rect)
        .collect::<Vec<_>>();
    let fixture = missing_badge_fixture(&base, &rects);
    assert_rejects(&fixture, "badge-0-FaultRed");
    assert_rejects(&fixture, "badge-1-WorkerHardHat");
    assert_rejects(&fixture, "badge-2-HealthyGreen");
}

#[test]
fn analyzer_rejects_a_frame_with_a_blank_hud() {
    let base = synthetic_frame(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    let fixture = blank_hud_fixture(&base, &[synthetic_hud_panel()]);
    assert_rejects(&fixture, "hud-queue-FaultRed");
}

#[test]
fn analyzer_rejects_a_frame_of_the_wrong_size() {
    let fixture = synthetic_frame(640, 360);
    assert_rejects(&fixture, "dimensions");
}

#[test]
fn analyzer_rejects_a_hud_rectangle_that_leaves_the_screen() {
    let image = synthetic_frame(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    let mut facts = synthetic_facts();
    facts.hud_panels.insert(
        "controls".to_owned(),
        RectFacts {
            x: 1200.0,
            y: 700.0,
            width: 200.0,
            height: 60.0,
        },
    );
    let failures = evaluate_frame(
        FrameName::HealthyCenterNorthEast,
        &facts,
        &metrics_of(&image),
        reference_metrics(),
    )
    .into_iter()
    .map(|failure| failure.metric)
    .collect::<Vec<_>>();
    assert!(
        failures.contains(&"hud-controls-on-screen".to_owned()),
        "an off-screen HUD panel must fail, got {failures:?}"
    );
}

#[test]
fn reference_metrics_are_measured_once_and_cached() {
    let first = reference_metrics();
    let second = reference_metrics();
    assert!(
        std::ptr::eq(first, second),
        "the key-art metrics must be computed once for the process"
    );
    assert_eq!(first.width, 1536);
    assert_eq!(first.height, 1024);
    assert!(first.edge_density > 0.0);
}

#[test]
fn clip_difference_separates_poses_and_ignores_position() {
    let mut left = synthetic_frame(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    let crop = synthetic_worker_crop();
    let moved = PixelRect {
        x: crop.x + 200,
        ..crop
    };
    // The same silhouette drawn somewhere else must read as the same pose.
    let mut right = synthetic_frame(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    for y in 0..crop.height {
        for x in 0..crop.width {
            let pixel = *left.get_pixel(crop.x + x, crop.y + y);
            right.put_pixel(moved.x + x, moved.y + y, pixel);
        }
    }
    assert!(
        clip_difference(&left, crop, &right, moved) < 0.02,
        "the same pose in a different place must not read as a different clip"
    );

    // A different silhouette in the same place must read as a different pose:
    // the technician's whole upper body leaves the mask.
    let floor = PaletteRole::FloorLight.color().to_u8_array_no_alpha();
    for y in 0..crop.height / 2 {
        for x in 0..crop.width {
            left.put_pixel(
                crop.x + x,
                crop.y + y,
                image::Rgb([floor[0], floor[1], floor[2]]),
            );
        }
    }
    let difference = clip_difference(&left, crop, &right, moved);
    assert!(
        difference > CLIP_DIFFERENCE_RANGE.0,
        "a changed pose must exceed {}, got {difference}",
        CLIP_DIFFERENCE_RANGE.0
    );
}

#[test]
fn outside_crop_change_ignores_the_excluded_rectangles() {
    let base = synthetic_frame(FIXTURE_WIDTH, FIXTURE_HEIGHT);
    let crop = synthetic_worker_crop();
    let mut changed = base.clone();
    for y in crop.y..crop.y + crop.height {
        for x in crop.x..crop.x + crop.width {
            changed.put_pixel(x, y, image::Rgb([1, 2, 3]));
        }
    }
    assert_eq!(outside_crop_change(&base, &changed, &[crop]), 0.0);
    assert!(outside_crop_change(&base, &changed, &[]) > 0.0);
    assert_eq!(
        outside_crop_change(&base, &synthetic_frame(640, 360), &[]),
        1.0,
        "a size change is a total change"
    );
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

fn parse(arguments: &[&str]) -> Result<VerificationRequest, String> {
    parse_verification_args(arguments.iter().map(|argument| (*argument).to_owned()))
}

#[test]
fn command_line_accepts_the_documented_shapes() {
    assert_eq!(parse(&[]), Ok(VerificationRequest::default()));
    assert_eq!(
        parse(&["--verify-output", "frames"]),
        Ok(VerificationRequest {
            output: Some(PathBuf::from("frames")),
            fault: None,
            flood: None,
        })
    );
    assert_eq!(
        parse(&["--verify-output=frames", "--verify-fault=stall"]),
        Ok(VerificationRequest {
            output: Some(PathBuf::from("frames")),
            fault: Some(VerificationFault::Stall),
            flood: None,
        })
    );
    assert_eq!(
        parse(&["--verify-flood", "4096"]),
        Ok(VerificationRequest {
            output: None,
            fault: None,
            flood: Some(4096),
        })
    );
}

#[test]
fn command_line_rejects_every_other_shape() {
    for arguments in [
        vec!["--verify-output"],
        vec!["--verify-output", "a", "--verify-output", "b"],
        vec!["--verify-fault", "stall"],
        vec!["--verify-output", "a", "--verify-fault"],
        vec!["--verify-output", "a", "--verify-fault", "explode"],
        vec!["--verify-flood"],
        vec!["--verify-flood", "0"],
        vec!["--verify-flood", "lots"],
        vec!["--verify-flood", "16", "--verify-flood", "32"],
        vec!["--verify-flood", "16", "--verify-output", "a"],
        vec!["--play"],
        vec!["frames"],
    ] {
        assert!(
            parse(&arguments).is_err(),
            "{arguments:?} must be a usage error"
        );
    }
}

#[test]
fn cli_exits_with_code_two_for_every_unusable_output_path() {
    let temp = TempDir::new("cli-exit");
    let file = temp.join("not-a-directory");
    fs::write(&file, b"x").expect("the test owns this file");

    for (arguments, expected) in [
        (vec!["--verify-output"], "requires a directory path"),
        (vec!["--verify-output", ""], "requires a directory path"),
        (
            vec!["--verify-output", "/"],
            "verification output directory",
        ),
        (
            vec!["--verify-output", file.to_str().unwrap()],
            "is not a directory",
        ),
        (
            vec!["--verify-output", temp.join("a/b/c").to_str().unwrap()],
            "does not exist",
        ),
        (vec!["--nonsense"], "unknown argument"),
    ] {
        let output = Command::new(binary())
            .args(&arguments)
            .current_dir(repository())
            .output()
            .expect("the compiled game runs");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{arguments:?} must exit 2, got {:?}",
            output.status
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected),
            "{arguments:?} stderr {stderr:?} must explain {expected:?}"
        );
        assert!(
            output.stdout.is_empty(),
            "{arguments:?} must not print anything on stdout"
        );
    }
}

// ---------------------------------------------------------------------------
// The real rendered run
// ---------------------------------------------------------------------------

/// How long the parent gives the child before it kills that exact process.
const PARENT_WATCHDOG: Duration = Duration::from_secs(50);

/// Only one real game window at a time, whatever the test harness does.
static APP_LOCK: Mutex<()> = Mutex::new(());

fn sha256(path: impl AsRef<Path>) -> String {
    let bytes = fs::read(path.as_ref()).expect("a pinned input is readable");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_midcreek-cs-1"))
}

struct Launch {
    code: Option<i32>,
    killed: bool,
    elapsed: Duration,
    stdout: String,
    stderr: String,
}

impl Launch {
    /// Everything the child said, for a failure message.
    fn diagnostics(&self) -> String {
        format!(
            "stdout ({} bytes):\n{}\nstderr ({} bytes):\n{}",
            self.stdout.len(),
            self.stdout,
            self.stderr.len(),
            self.stderr
        )
    }
}

/// Runs the compiled game once, polling with `try_wait` and killing the exact
/// child process if the parent watchdog expires.
///
/// Both pipes are drained by their own thread from the moment the child
/// starts. A parent that instead waits for exit before reading deadlocks the
/// first time the child writes more than one pipe buffer — a real risk here,
/// because a failing run prints every metric it measured — and a parent that
/// drains one pipe to the end before touching the other deadlocks as soon as
/// the child fills the one it is not reading. `--verify-flood` exists to prove
/// this loop against a child that overruns both.
fn launch_arguments(arguments: &[String], watchdog: Duration) -> Launch {
    let _serialized = APP_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut command = Command::new(binary());
    command
        .args(arguments)
        .current_dir(repository())
        .env("BEVY_ASSET_ROOT", repository())
        .env("RUST_LOG", "error")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("the compiled game starts");

    let mut out_pipe = child.stdout.take().expect("stdout was piped");
    let mut err_pipe = child.stderr.take().expect("stderr was piped");
    let out_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = out_pipe.read_to_end(&mut buffer);
        buffer
    });
    let err_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = err_pipe.read_to_end(&mut buffer);
        buffer
    });

    let started = Instant::now();
    let deadline = started + watchdog;
    let mut killed = false;
    let status = loop {
        match child.try_wait().expect("the child can be polled") {
            Some(status) => break status,
            None => {
                if Instant::now() >= deadline {
                    child.kill().expect("the exact child can be killed");
                    killed = true;
                    break child.wait().expect("the killed child is reaped");
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };
    let elapsed = started.elapsed();
    // Both pipes are closed by the exited child, so both readers are already
    // finishing; joining them is what guarantees the buffers are complete.
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Launch {
        code: status.code(),
        killed,
        elapsed,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

fn launch(output: &Path, fault: Option<VerificationFault>, watchdog: Duration) -> Launch {
    let mut arguments = vec![
        "--verify-output".to_owned(),
        output.to_string_lossy().into_owned(),
    ];
    if let Some(fault) = fault {
        arguments.push("--verify-fault".to_owned());
        arguments.push(fault.name().to_owned());
    }
    launch_arguments(&arguments, watchdog)
}

/// One complete rendered run, kept alive for the whole test binary.
struct RenderedRun {
    root: PathBuf,
    report: VerificationReport,
    canonical: String,
    frames: BTreeMap<String, RgbImage>,
    metrics: BTreeMap<String, FrameMetrics>,
}

impl RenderedRun {
    fn frame(&self, frame: FrameName) -> &RgbImage {
        self.frames
            .get(frame.file_name())
            .unwrap_or_else(|| panic!("{} was captured", frame.file_name()))
    }

    fn facts(&self, frame: FrameName) -> &FrameFacts {
        self.report
            .frames
            .get(frame.file_name())
            .unwrap_or_else(|| panic!("{} is in the report", frame.file_name()))
    }

    fn metrics(&self, frame: FrameName) -> &FrameMetrics {
        self.metrics
            .get(frame.file_name())
            .unwrap_or_else(|| panic!("{} was measured", frame.file_name()))
    }

    fn crop(&self, frame: FrameName) -> PixelRect {
        let facts = self.facts(frame);
        PixelRect::snap(facts.worker_crop, facts.width, facts.height)
    }
}

/// Runs the real game once and keeps every artifact for the whole test binary.
///
/// Artifacts are deliberately never removed: a failing gate has to leave its
/// frames, report, stdout, and stderr behind for a human or a workflow to
/// collect.
fn rendered_run() -> &'static RenderedRun {
    static RUN: OnceLock<RenderedRun> = OnceLock::new();
    RUN.get_or_init(|| {
        let root = repository().join("target/render-contract/primary");
        render_into(&root, "primary")
    })
}

fn render_into(root: &Path, label: &str) -> RenderedRun {
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root).expect("the render contract owns target/render-contract");
    let launched = launch(root, None, PARENT_WATCHDOG);
    let diagnostics = root.join("stdout.log");
    let _ = fs::write(&diagnostics, &launched.stdout);
    let _ = fs::write(root.join("stderr.log"), &launched.stderr);

    assert!(
        !launched.killed,
        "the {label} verification run had to be killed by the {PARENT_WATCHDOG:?} parent watchdog; artifacts kept in {}\n{}",
        root.display(),
        launched.diagnostics()
    );
    assert_eq!(
        launched.code,
        Some(0),
        "the {label} verification run failed; artifacts kept in {}\n{}",
        root.display(),
        launched.diagnostics()
    );

    let canonical = fs::read_to_string(root.join(REPORT_FILE_NAME))
        .unwrap_or_else(|error| panic!("the {label} run wrote no report: {error}"));
    let report: VerificationReport =
        serde_json::from_str(&canonical).expect("the report matches the canonical schema");
    let frames: BTreeMap<String, RgbImage> = FrameName::ALL
        .into_iter()
        .map(|frame| {
            let image = image::open(root.join(frame.file_name()))
                .unwrap_or_else(|error| {
                    panic!("{} could not be decoded: {error}", frame.file_name())
                })
                .to_rgb8();
            (frame.file_name().to_owned(), image)
        })
        .collect();

    // Every frame is measured once here, so no later test repeats the walk.
    let metrics = FrameName::ALL
        .into_iter()
        .map(|frame| {
            let facts = report
                .frames
                .get(frame.file_name())
                .unwrap_or_else(|| panic!("{} is in the report", frame.file_name()));
            (
                frame.file_name().to_owned(),
                FrameMetrics::compute(&frames[frame.file_name()], &frame_regions(facts)),
            )
        })
        .collect();

    RenderedRun {
        root: root.to_path_buf(),
        report,
        canonical,
        frames,
        metrics,
    }
}

#[test]
fn rendered_run_writes_only_its_named_artifacts_and_succeeds() {
    let run = rendered_run();
    assert_eq!(run.report.result, "success");
    assert_eq!(run.report.failed_stage, None);
    assert_eq!(run.report.failure_reason, None);
    assert_eq!(
        run.report.stages.last().map(String::as_str),
        Some("success")
    );
    assert_eq!(
        run.report.stages,
        VerificationStage::ALL
            .iter()
            .filter(|stage| **stage != VerificationStage::Failure)
            .map(|stage| stage.name().to_owned())
            .collect::<Vec<_>>(),
        "the run must walk the documented stage order exactly once"
    );

    let mut written = fs::read_dir(&run.root)
        .expect("the output directory exists")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with(".log"))
        .collect::<Vec<_>>();
    written.sort();
    let mut expected = ARTIFACT_NAMES.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(
        written, expected,
        "the run must write exactly its fifteen named artifacts"
    );
}

#[test]
fn rendered_run_pins_every_source_asset_and_reference_hash() {
    let run = rendered_run();
    assert_eq!(run.report.assets.len(), 5);
    assert_eq!(run.report.asset_sources.len(), 5);
    assert_eq!(run.report.references.len(), 2);
    assert!(!run.report.sources.is_empty());
    for map in [
        &run.report.assets,
        &run.report.asset_sources,
        &run.report.references,
        &run.report.sources,
    ] {
        for (path, digest) in map {
            assert!(
                !Path::new(path).is_absolute(),
                "{path} must be repository relative"
            );
            assert_eq!(digest.len(), 64, "{path} must carry a SHA-256");
            assert_eq!(&sha256(repository().join(path)), digest, "{path} drifted");
        }
    }
    assert_eq!(
        run.report.references.get(KEY_ART_REFERENCE_PATH),
        Some(&KEY_ART_SHA256.to_owned())
    );
    assert_eq!(
        run.report.references.get(CHARACTER_SHEET_REFERENCE_PATH),
        Some(&CHARACTER_SHEET_SHA256.to_owned())
    );
}

#[test]
fn rendered_run_drove_the_real_seeded_gameplay_with_real_keys() {
    let run = rendered_run();
    let gameplay = &run.report.gameplay;

    let opened = gameplay
        .ticket_history
        .iter()
        .filter(|event| event.event == "opened")
        .collect::<Vec<_>>();
    assert!(opened.len() >= 4, "got {:?}", gameplay.ticket_history);
    assert_eq!(
        opened
            .iter()
            .map(|event| (event.rack, event.severity.as_str(), event.tick))
            .take(3)
            .collect::<Vec<_>>(),
        vec![
            (2, "critical", 240),
            (1, "critical", 480),
            (3, "critical", 720)
        ],
        "the fixed seed and the fixed 1/60 step pin the exact rack, severity, and tick"
    );

    assert!(
        gameplay
            .interactions
            .iter()
            .any(|interaction| interaction.outcome == "out-of-range"),
        "the journey must record a real out-of-range Space rejection"
    );
    let started = gameplay
        .interactions
        .iter()
        .find(|interaction| interaction.outcome == "started")
        .expect("the journey must start one repair");
    assert_eq!(started.rack, Some(gameplay.repaired_rack));

    let keys = gameplay
        .keys
        .iter()
        .map(|key| key.key.as_str())
        .collect::<BTreeSet<_>>();
    let arrows = keys.iter().filter(|key| key.starts_with("arrow-")).count();
    assert!(
        arrows >= 2,
        "camera-relative movement must press at least two arrow keys, got {keys:?}"
    );
    for expected in ["q", "e", "space"] {
        assert!(keys.contains(expected), "{expected} was never injected");
    }
    // The journey presses Q and E on one frame and requires the orbit to have
    // cancelled, so the recorded pair is a proof rather than a note.
    let cancel = gameplay
        .keys
        .iter()
        .position(|key| key.key == "q" && key.state == "pressed")
        .expect("the journey presses Q");
    assert_eq!(
        gameplay.keys.get(cancel + 1).map(|key| key.key.as_str()),
        Some("e"),
        "Q and E must be pressed on the same frame to cancel"
    );
    for arrow in keys.iter().filter(|key| key.starts_with("arrow-")) {
        assert!(
            gameplay
                .keys
                .iter()
                .any(|key| key.state == "pressed" && &key.key == arrow)
                && gameplay
                    .keys
                    .iter()
                    .any(|key| key.state == "released" && &key.key == arrow),
            "{arrow} must be really pressed and really released"
        );
    }
    assert_eq!(gameplay.rig_parts.len(), required_player_parts().len());
    assert_eq!(gameplay.fixed_step_seconds, canonical_f64(1.0 / 60.0));
}

#[test]
fn rendered_run_captured_the_documented_camera_and_hud_state() {
    let run = rendered_run();
    for (frame, heading, settled) in [
        (FrameName::HealthyCenterNorthEast, "north-east", true),
        (FrameName::SettledSouthEast, "south-east", true),
        (FrameName::SettledSouthWest, "south-west", true),
        (FrameName::SettledNorthWest, "north-west", true),
        (FrameName::MidOrbit, "north-east", false),
        (FrameName::CornerNorthEast, "north-east", true),
        (FrameName::CornerSouthEast, "south-east", true),
        (FrameName::CornerSouthWest, "south-west", true),
        (FrameName::CornerNorthWest, "north-west", true),
    ] {
        let facts = run.facts(frame);
        assert_eq!(facts.heading, heading, "{frame:?}");
        assert_eq!(facts.camera_settled, settled, "{frame:?}");
    }
    let tween = run.facts(FrameName::MidOrbit);
    assert!(
        (0.4..=0.75).contains(&tween.camera_progress),
        "the tween capture must land near the midpoint, got {}",
        tween.camera_progress
    );

    assert_eq!(run.facts(FrameName::WalkNorthEast).player_clip, "walk");
    assert_eq!(
        run.facts(FrameName::RepairingNorthEast).player_clip,
        "repair"
    );
    assert!(run.facts(FrameName::RepairingNorthEast).movement_locked);
    assert_eq!(run.facts(FrameName::ResolvedNorthEast).player_clip, "idle");
    assert!(!run.facts(FrameName::ResolvedNorthEast).movement_locked);

    assert!(
        run.facts(FrameName::HealthyCenterNorthEast)
            .hud_rows
            .is_empty()
    );
    assert_eq!(run.facts(FrameName::FaultQueueNorthEast).hud_rows.len(), 3);
    assert_eq!(run.facts(FrameName::LowResolutionQueue).hud_rows.len(), 3);
    assert_eq!(run.facts(FrameName::LowResolutionQueue).width, 960);
    assert_eq!(run.facts(FrameName::LowResolutionQueue).height, 540);

    let repairing = run.facts(FrameName::RepairingNorthEast);
    assert!(
        repairing
            .badges
            .iter()
            .any(|badge| badge.kind == "repairing" && badge.visibility == "shown"),
        "the repair capture must show the blue wrench badge"
    );
    assert!(
        repairing
            .badges
            .iter()
            .any(|badge| badge.kind == "fault" && badge.visibility == "shown"),
        "the other open faults must still show red badges"
    );
    assert!(
        run.facts(FrameName::ResolvedNorthEast)
            .badges
            .iter()
            .any(|badge| badge.kind == "resolved" && badge.visibility == "shown"),
        "the resolved capture must show the healthy badge"
    );
}

#[test]
fn every_captured_frame_meets_every_mandatory_visual_contract() {
    let run = rendered_run();
    let reference = reference_metrics();
    let mut failures = Vec::new();
    for frame in FrameName::ALL {
        failures.extend(evaluate_frame(
            frame,
            run.facts(frame),
            run.metrics(frame),
            reference,
        ));
    }
    assert!(
        failures.is_empty(),
        "artifacts kept in {}\n{}",
        run.root.display(),
        failures
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn animation_clips_change_the_worker_crop_without_changing_the_rest_of_the_frame() {
    let run = rendered_run();
    let idle = FrameName::ResolvedNorthEast;
    let walk = FrameName::WalkNorthEast;
    let repair = FrameName::RepairingNorthEast;

    for (left, right) in [(idle, walk), (walk, repair), (idle, repair)] {
        let difference = clip_difference(
            run.frame(left),
            run.crop(left),
            run.frame(right),
            run.crop(right),
        );
        assert!(
            (CLIP_DIFFERENCE_RANGE.0..=CLIP_DIFFERENCE_RANGE.1).contains(&difference),
            "{left:?} against {right:?} differed by {difference}, expected between {} and {}",
            CLIP_DIFFERENCE_RANGE.0,
            CLIP_DIFFERENCE_RANGE.1
        );
    }

    // The repairing and resolved captures are taken from the same standing
    // position, so everything outside the technician must hold still.
    assert_eq!(
        run.facts(repair).player_position,
        run.facts(idle).player_position,
        "the repair and resolved captures must share one position"
    );
    let change = outside_crop_change(
        run.frame(repair),
        run.frame(idle),
        &[run.crop(repair), run.crop(idle)],
    );
    assert!(
        change <= OUTSIDE_CROP_MAX,
        "{change} of the frame outside the worker crop changed, expected at most {OUTSIDE_CROP_MAX}"
    );
}

#[test]
fn generated_negative_fixtures_cut_from_real_frames_are_rejected() {
    let run = rendered_run();
    let reference = reference_metrics();
    let frame = FrameName::RepairingNorthEast;
    let facts = run.facts(frame);
    let regions = frame_regions(facts);
    let base = run.frame(frame);

    assert!(
        evaluate_frame(frame, facts, run.metrics(frame), reference).is_empty(),
        "the real frame must pass before its mutations can prove anything"
    );

    let named = |image: &RgbImage| {
        evaluate_frame(
            frame,
            facts,
            &FrameMetrics::compute(image, &regions),
            reference,
        )
        .into_iter()
        .map(|failure| failure.metric)
        .collect::<Vec<_>>()
    };

    let crop = run.crop(frame);
    let without_worker = named(&missing_worker_fixture(base, crop));
    assert!(
        without_worker.contains(&"worker-crop-WorkerHardHat".to_owned())
            && without_worker.contains(&"worker-crop-WorkerHiVis".to_owned()),
        "painting the technician out of a real frame must fail the worker crop: {without_worker:?}"
    );

    let badges = facts
        .badges
        .iter()
        .filter(|badge| badge.visibility == "shown")
        .filter_map(|badge| {
            badge
                .rect
                .map(|rect| PixelRect::snap(rect, facts.width, facts.height))
        })
        .collect::<Vec<_>>();
    assert!(!badges.is_empty(), "the repair capture draws badges");
    let without_badges = named(&missing_badge_fixture(base, &badges));
    assert!(
        without_badges
            .iter()
            .any(|metric| metric.starts_with("badge-")),
        "painting the badges out of a real frame must fail: {without_badges:?}"
    );

    let panels = facts
        .hud_panels
        .values()
        .map(|rect| PixelRect::snap(*rect, facts.width, facts.height))
        .collect::<Vec<_>>();
    let blank = named(&blank_hud_fixture(base, &panels));
    assert!(
        blank.iter().any(|metric| metric.starts_with("hud-queue-")),
        "painting the HUD out of a real frame must fail: {blank:?}"
    );
}

// ---------------------------------------------------------------------------
// Equipment-scoped contracts
// ---------------------------------------------------------------------------

/// Every authored prop identifier of one category, from the real report.
fn category_props(facts: &FrameFacts, category: EquipmentCategory) -> Vec<&EquipmentFacts> {
    facts
        .equipment
        .iter()
        .filter(|prop| prop.category == category.name())
        .collect()
}

/// The four settled headings taken from the middle of the hall.
const CENTER_FRAMES: [FrameName; 4] = [
    FrameName::HealthyCenterNorthEast,
    FrameName::SettledSouthEast,
    FrameName::SettledSouthWest,
    FrameName::SettledNorthWest,
];

/// Whether one failing metric is one equipment category's own category-level
/// contract, rather than a per-prop one.
fn is_category_metric(metric: &str, category: EquipmentCategory) -> bool {
    metric.starts_with(&format!("equipment-{}", category.name()))
}

/// Whether one failing metric names one authored prop of a category.
fn is_prop_metric(metric: &str, facts: &FrameFacts, category: EquipmentCategory) -> bool {
    category_props(facts, category)
        .iter()
        .any(|prop| metric == format!("equipment-{}", prop.id))
}

/// Paints a set of projected regions out with the floor colour, which is
/// exactly what a frame would look like if that equipment had failed to spawn.
fn mask_regions(base: &RgbImage, facts: &FrameFacts, regions: &[RectFacts]) -> RgbImage {
    let mut image = base.clone();
    let floor = image::Rgb(FLOOR_LIGHT.to_u8_array_no_alpha());
    for rect in regions {
        let snapped = PixelRect::snap(*rect, facts.width, facts.height);
        for y in snapped.y..snapped.y + snapped.height {
            for x in snapped.x..snapped.x + snapped.width {
                image.put_pixel(x, y, floor);
            }
        }
    }
    image
}

/// Every projected region of one category.
fn category_regions(facts: &FrameFacts, category: EquipmentCategory) -> Vec<RectFacts> {
    category_props(facts, category)
        .iter()
        .flat_map(|prop| prop.regions.clone())
        .collect()
}

/// Every mandatory contract one image fails, by metric name.
fn failed_metric_names(run: &RenderedRun, frame: FrameName, image: &RgbImage) -> Vec<String> {
    let facts = run.facts(frame);
    evaluate_frame(
        frame,
        facts,
        &FrameMetrics::compute(image, &frame_regions(facts)),
        reference_metrics(),
    )
    .into_iter()
    .map(|failure| failure.metric)
    .collect()
}

#[test]
fn equipment_categories_partition_every_authored_asset_kind() {
    let mut covered = BTreeSet::new();
    for kind in AssetKind::ALL {
        if let Some(category) = EquipmentCategory::of(kind) {
            covered.insert(category.name());
        }
    }
    assert_eq!(
        covered,
        EquipmentCategory::ALL
            .into_iter()
            .map(EquipmentCategory::name)
            .collect::<BTreeSet<_>>(),
        "every category must be reachable from an authored asset kind"
    );

    // The global surfaces are deliberately not equipment: they are exactly
    // what the equipment contracts exist to stop standing in for it.
    for kind in [
        AssetKind::RenderApron,
        AssetKind::Floor,
        AssetKind::FloorGrid,
        AssetKind::Wall,
    ] {
        assert_eq!(EquipmentCategory::of(kind), None, "{kind:?}");
    }
    for category in EquipmentCategory::ALL {
        let groups = category.role_groups();
        assert!(!groups.is_empty(), "{category:?} needs a role group");
        assert!(
            !groups
                .iter()
                .all(|group| group == &[PaletteRole::Ink].as_slice()),
            "{category:?} must not qualify on the inked floor grid alone"
        );
    }
}

#[test]
fn rendered_run_projects_every_required_equipment_family_from_real_bounds() {
    let run = rendered_run();
    let blueprint_props = SceneBlueprint::v0()
        .visuals
        .iter()
        .filter(|visual| EquipmentCategory::of(visual.asset).is_some())
        .map(|visual| visual.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    for frame in CENTER_FRAMES {
        let facts = run.facts(frame);
        let reported = facts
            .equipment
            .iter()
            .map(|prop| prop.id.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            reported, blueprint_props,
            "{frame:?} must project every authored equipment prop"
        );
        assert!(
            facts
                .equipment
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id),
            "{frame:?} equipment must be sorted by prop identifier"
        );

        for prop in &facts.equipment {
            let [min, max] = prop.world_bounds;
            assert!(
                max[0] > min[0] && max[1] >= min[1] && max[2] > min[2],
                "{frame:?} {} must carry real 3D bounds, got {:?}",
                prop.id,
                prop.world_bounds
            );
            let bounds = prop.projected_bounds;
            let intersects = bounds[2] > 0.0
                && bounds[3] > 0.0
                && bounds[0] < f64::from(facts.width)
                && bounds[1] < f64::from(facts.height);
            assert_eq!(
                prop.on_screen, intersects,
                "{frame:?} {} may only be excluded when its projected bounds miss the viewport, got {bounds:?}",
                prop.id
            );
            if !prop.on_screen {
                assert!(
                    prop.regions.is_empty(),
                    "{frame:?} {} is off screen and must contribute no region",
                    prop.id
                );
            }
            for rect in &prop.regions {
                let snapped = PixelRect::snap(*rect, facts.width, facts.height);
                assert!(
                    snapped.area() >= EQUIPMENT_REGION_MIN_PIXELS,
                    "{frame:?} {} kept a {snapped:?} region below the measurable floor",
                    prop.id
                );
            }
        }

        for category in EquipmentCategory::ALL {
            let props = category_props(facts, category);
            let measurable = props
                .iter()
                .filter(|prop| prop.on_screen && !prop.regions.is_empty())
                .count();
            let excluded = EXPECTED_OFF_SCREEN.contains(&(frame.file_name(), category.name()));
            if excluded {
                assert!(
                    props.iter().all(|prop| !prop.on_screen),
                    "{frame:?} {} is recorded as out of shot, so no prop of it may project into the viewport",
                    category.name()
                );
            } else {
                assert!(
                    measurable > 0,
                    "{frame:?} must keep at least one measurable {} region",
                    category.name()
                );
            }
        }
        for rack in 1..=4 {
            let id = format!("rack-row-{rack:02}");
            assert!(
                facts.equipment.iter().any(|prop| prop.id == id),
                "{frame:?} must project {id}"
            );
        }
    }

    // Every category is in shot on at least three of the four centre frames,
    // so no family can be excluded everywhere.
    for category in EquipmentCategory::ALL {
        let excluded = CENTER_FRAMES
            .into_iter()
            .filter(|frame| EXPECTED_OFF_SCREEN.contains(&(frame.file_name(), category.name())))
            .count();
        assert!(
            excluded <= 1,
            "{} may not be out of shot on more than one centre frame",
            category.name()
        );
    }
}

/// The one authored family that genuinely leaves the orthographic rectangle at
/// a centre heading, with the frame it leaves it on.
///
/// The utility cart is a single prop parked at `(-13, -10)`; at the SouthWest
/// heading the camera follows the technician to `(-10.35, 0)` and the cart
/// projects entirely below the 720-pixel viewport. That is the one exclusion
/// the equipment contracts allow, it is proven from the cart's own unclipped
/// projected bounds in the report, and pinning it here means a *new* exclusion
/// can never appear quietly.
const EXPECTED_OFF_SCREEN: [(&str, &str); 1] = [("07-settled-sw.png", "utility-cart")];

#[test]
fn every_center_frame_carries_real_equipment_pixels_with_margin() {
    let run = rendered_run();
    let mut report = Vec::new();
    for frame in CENTER_FRAMES {
        let facts = run.facts(frame);
        let metrics = run.metrics(frame);
        for category in EquipmentCategory::ALL {
            if EXPECTED_OFF_SCREEN.contains(&(frame.file_name(), category.name())) {
                continue;
            }
            let best = category_props(facts, category)
                .iter()
                .flat_map(|prop| {
                    prop.regions.iter().enumerate().filter_map(|(segment, _)| {
                        let region = metrics.region(&equipment_region(&prop.id, segment))?;
                        category
                            .role_groups()
                            .iter()
                            .map(|group| group.iter().map(|role| region.near(*role)).sum::<f64>())
                            .fold(None::<f64>, |low, share| {
                                Some(low.map_or(share, |value| value.min(share)))
                            })
                    })
                })
                .fold(0.0f64, f64::max);
            report.push(format!(
                "{} {} {best:.4}",
                frame.file_name(),
                category.name()
            ));
            assert!(
                best >= EQUIPMENT_ROLE_MIN * 2.0,
                "{} {} measured {best:.4}, which leaves no margin over the {EQUIPMENT_ROLE_MIN} floor",
                frame.file_name(),
                category.name()
            );
        }
    }
    println!("equipment margins:\n{}", report.join("\n"));
}

#[test]
fn masking_one_equipment_category_fails_that_category_alone() {
    let run = rendered_run();
    for frame in CENTER_FRAMES {
        let facts = run.facts(frame);
        assert!(
            failed_metric_names(run, frame, run.frame(frame)).is_empty(),
            "{frame:?} must pass before its mutations can prove anything"
        );

        for category in EquipmentCategory::ALL {
            if EXPECTED_OFF_SCREEN.contains(&(frame.file_name(), category.name())) {
                continue;
            }
            let regions = category_regions(facts, category);
            let masked = mask_regions(run.frame(frame), facts, &regions);
            let failures = failed_metric_names(run, frame, &masked);
            let equipment = failures
                .iter()
                .filter(|metric| metric.starts_with("equipment-"))
                .cloned()
                .collect::<Vec<_>>();

            assert!(
                equipment
                    .iter()
                    .any(|metric| is_category_metric(metric, category)),
                "{frame:?}: masking {} must fail its own equipment contract, got {failures:?}",
                category.name()
            );
            for other in EquipmentCategory::ALL {
                if other == category {
                    continue;
                }
                let collateral = equipment
                    .iter()
                    .filter(|metric| is_category_metric(metric, other))
                    .collect::<Vec<_>>();
                assert!(
                    collateral.is_empty(),
                    "{frame:?}: masking {} must leave {} green, got {collateral:?}",
                    category.name(),
                    other.name()
                );

                // Per-prop collateral is only ever allowed between the one
                // pair the hall is authored to overlap: `OVERHEAD_TRAY_HEIGHT`
                // is chosen so a tray hung over an aisle projects onto a rack
                // row, so masking either family does cover part of the other.
                let coupled = matches!(
                    (category, other),
                    (
                        EquipmentCategory::OverheadRouting,
                        EquipmentCategory::RackRows
                    ) | (
                        EquipmentCategory::RackRows,
                        EquipmentCategory::OverheadRouting
                    )
                );
                if coupled {
                    continue;
                }
                let props = equipment
                    .iter()
                    .filter(|metric| is_prop_metric(metric, facts, other))
                    .collect::<Vec<_>>();
                assert!(
                    props.is_empty(),
                    "{frame:?}: masking {} must leave every {} prop green, got {props:?}",
                    category.name(),
                    other.name()
                );
            }
        }
    }
}

#[test]
fn masking_one_rack_row_fails_only_that_rack_row() {
    let run = rendered_run();
    for frame in CENTER_FRAMES {
        let facts = run.facts(frame);
        let rows = category_props(facts, EquipmentCategory::RackRows)
            .into_iter()
            .filter(|prop| !prop.regions.is_empty())
            .map(|prop| (prop.id.clone(), prop.regions.clone()))
            .collect::<Vec<_>>();
        assert!(
            rows.len() >= 3,
            "{frame:?} must measure at least three of the four rack rows, got {}",
            rows.len()
        );

        for (id, regions) in &rows {
            let masked = mask_regions(run.frame(frame), facts, regions);
            let failures = failed_metric_names(run, frame, &masked);
            assert!(
                failures.contains(&format!("equipment-{id}")),
                "{frame:?}: masking {id} must fail its own contract, got {failures:?}"
            );
            for (other, _) in &rows {
                if other == id {
                    continue;
                }
                assert!(
                    !failures.contains(&format!("equipment-{other}")),
                    "{frame:?}: masking {id} must leave {other} green, got {failures:?}"
                );
            }
        }
    }
}

#[test]
fn hiding_the_utility_cart_passes_every_global_contract_and_fails_the_equipment_one() {
    let run = rendered_run();
    let frame = FrameName::HealthyCenterNorthEast;
    let facts = run.facts(frame);
    let regions = category_regions(facts, EquipmentCategory::UtilityCart);
    let masked = mask_regions(run.frame(frame), facts, &regions);
    let failures = failed_metric_names(run, frame, &masked);

    // This is the whole reason the equipment contracts exist: a frame the cart
    // vanished from still satisfies every whole-frame histogram, because the
    // floor grid, apron, and walls supply the ratios on their own.
    let global = failures
        .iter()
        .filter(|metric| !metric.starts_with("equipment-"))
        .collect::<Vec<_>>();
    assert!(
        global.is_empty(),
        "the global contracts were expected to miss a vanished cart entirely, they reported {global:?}"
    );
    assert!(
        failures.contains(&"equipment-utility-cart".to_owned()),
        "the equipment contract must catch it, got {failures:?}"
    );
}

#[test]
fn semantic_report_reproduces_in_a_different_output_directory() {
    let primary = rendered_run();
    let second = render_into(
        &repository().join("target/render-contract/reproduction"),
        "reproduction",
    );

    assert_ne!(
        primary.root, second.root,
        "the two runs must use different directories"
    );
    assert_eq!(
        primary.report, second.report,
        "two semantically identical runs must produce the same report"
    );
    assert_eq!(
        primary.canonical, second.canonical,
        "two semantically identical runs must serialize byte for byte"
    );
    assert_eq!(
        semantic_hash(&primary.canonical),
        semantic_hash(&second.canonical),
        "the canonical semantic hash must not depend on the output directory"
    );
    // Neither document may carry the directory it happened to be written into.
    for (label, run) in [("primary", primary), ("reproduction", &second)] {
        for other in [&primary.root, &second.root] {
            assert_scan_is_clean(
                &run.canonical,
                &[other.to_string_lossy().into_owned()],
                label,
            );
        }
    }
    let _ = fs::remove_dir_all(&second.root);
}

/// Fails when a canonical document carries any of `banned`.
fn assert_scan_is_clean(canonical: &str, banned: &[String], label: &str) {
    let found = scan_violations(canonical, banned);
    assert!(
        found.is_empty(),
        "the {label} canonical report must not carry {found:?}"
    );
}

/// Every banned substring one canonical document actually carries.
fn scan_violations(canonical: &str, banned: &[String]) -> Vec<String> {
    banned
        .iter()
        .filter(|needle| canonical.contains(needle.as_str()))
        .cloned()
        .collect()
}

/// Everything a canonical report may never carry, whoever produced it.
fn banned_report_substrings() -> Vec<String> {
    let mut banned = [
        "timestamp",
        "generated_at",
        "elapsed",
        "duration_ms",
        "hostname",
        "user",
        "/Users/",
        "/home/",
        "/tmp/",
        "/var/folders/",
        "C:\\",
        "BEVY_ASSET_ROOT",
        "CARGO",
        "RUST_LOG",
        "HOME",
        "PATH",
    ]
    .map(str::to_owned)
    .to_vec();
    banned.push(repository().to_string_lossy().into_owned());
    if let Some(home) = std::env::var_os("HOME") {
        banned.push(home.to_string_lossy().into_owned());
    }
    banned
}

#[test]
fn the_real_canonical_report_carries_no_wall_clock_host_path_or_environment() {
    let run = rendered_run();
    let banned = banned_report_substrings();
    assert_scan_is_clean(&run.canonical, &banned, "real");

    // The scan has to be load-bearing rather than vacuous: doctoring the real
    // document with the absolute output root it was written into, a host
    // clock, and an environment name must be caught by the same scan.
    for injected in [
        run.root.to_string_lossy().into_owned(),
        "\"generated_at\": \"2026-01-01T00:00:00Z\"".to_owned(),
        "\"BEVY_ASSET_ROOT\": \"x\"".to_owned(),
    ] {
        let doctored = run
            .canonical
            .replace("\"frames\":", &format!("{injected}\n  \"frames\":"));
        assert!(
            !scan_violations(&doctored, &banned).is_empty(),
            "the scan must catch {injected:?}"
        );
    }

    // The sample unit test proves the schema; this proves the document the
    // real child actually wrote, byte for byte.
    assert_eq!(
        run.canonical,
        canonical_json(&run.report),
        "the child must write exactly the canonical serialization of its own report"
    );
    assert!(run.canonical.ends_with("}\n"));
    assert_eq!(run.canonical.matches("\r\n").count(), 0);

    let assets = run.report.assets.keys().cloned().collect::<Vec<_>>();
    assert!(!assets.is_empty(), "the report pins generated assets");
    for key in assets
        .iter()
        .chain(run.report.sources.keys())
        .chain(run.report.references.keys())
        .chain(run.report.asset_sources.keys())
    {
        assert!(
            !Path::new(key).is_absolute() && !key.contains("..") && !key.starts_with('/'),
            "{key} must be a repository-relative path"
        );
    }
    for name in run.report.frames.keys() {
        assert!(
            !name.contains('/') && !name.contains('\\'),
            "{name} must be a bare file name"
        );
    }

    let mut sorted = assets.clone();
    sorted.sort();
    assert_eq!(assets, sorted, "the real report's maps must be sorted");
}

#[test]
fn the_real_run_camera_renders_the_production_display_contract() {
    let run = rendered_run();
    let camera = &run.report.camera;

    assert_eq!(
        camera.tonemapping,
        format!("{CEL_SHIFT_TONEMAPPING:?}"),
        "the captured frames must come from the production display transform"
    );
    assert_eq!(
        camera.deband_dither,
        format!("{CEL_SHIFT_DEBAND_DITHER:?}"),
        "the captured frames must come from the production dither"
    );
    assert_eq!(
        camera.msaa_samples,
        VERIFICATION_MSAA.samples(),
        "multisampling is the one render setting verification may change"
    );
    assert_eq!(
        camera.clear_color,
        format!("#{}", SENTINEL_CLEAR.to_hex().trim_start_matches('#')).to_uppercase(),
        "the clear colour is the other allowed difference: the magenta sentinel"
    );
}

#[test]
fn the_parent_drains_both_pipes_while_the_child_is_still_running() {
    // 8 MiB on each stream is more than a hundred times any platform's pipe
    // capacity, so a parent that waited for exit before reading, or drained
    // one stream to the end before the other, would be blocked here forever
    // and would come back killed with truncated buffers.
    const REQUESTED: u64 = 8 * 1024 * 1024;
    let expected = flood_bytes(REQUESTED);
    let launched = launch_arguments(
        &["--verify-flood".to_owned(), REQUESTED.to_string()],
        PARENT_WATCHDOG,
    );

    assert!(
        !launched.killed,
        "the flood fixture must finish on its own, it was killed after {:?}",
        launched.elapsed
    );
    assert_eq!(launched.code, Some(0));
    assert_eq!(
        launched.stdout.len() as u64,
        expected,
        "stdout must be captured whole"
    );
    assert_eq!(
        launched.stderr.len() as u64,
        expected,
        "stderr must be captured whole"
    );
    assert!(
        launched.stdout.ends_with('\n') && launched.stderr.ends_with('\n'),
        "both buffers must end on a complete line"
    );
    assert!(
        launched.diagnostics().len() as u64 >= expected * 2,
        "a failure message must carry both complete buffers"
    );
}

#[test]
fn a_lost_screenshot_callback_fails_the_run_and_names_its_stage() {
    let root = repository().join("target/render-contract/drop-capture");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the render contract owns target/render-contract");
    let launched = launch(&root, Some(VerificationFault::DropCapture), PARENT_WATCHDOG);

    assert!(!launched.killed, "the app must fail on its own");
    assert_eq!(launched.code, Some(1));
    let report: VerificationReport =
        serde_json::from_str(&fs::read_to_string(root.join(REPORT_FILE_NAME)).expect("a report"))
            .expect("a canonical report");
    assert_eq!(report.result, "failure");
    assert_eq!(report.failed_stage.as_deref(), Some("healthy-capture"));
    assert!(
        report
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("screenshot callback")),
        "got {:?}",
        report.failure_reason
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_app_watchdog_fails_a_stalled_run_with_its_stage_name() {
    let root = repository().join("target/render-contract/stall");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the render contract owns target/render-contract");
    let launched = launch(&root, Some(VerificationFault::Stall), PARENT_WATCHDOG);
    let elapsed = launched.elapsed;

    assert!(
        !launched.killed,
        "the app watchdog must fire before the parent"
    );
    assert_eq!(launched.code, Some(1));
    assert!(
        elapsed >= APP_WATCHDOG && elapsed < PARENT_WATCHDOG,
        "the app watchdog must fire between {APP_WATCHDOG:?} and {PARENT_WATCHDOG:?}, took {elapsed:?}"
    );
    let report: VerificationReport =
        serde_json::from_str(&fs::read_to_string(root.join(REPORT_FILE_NAME)).expect("a report"))
            .expect("a canonical report");
    assert_eq!(report.result, "failure");
    assert!(
        report.failure_reason.as_deref().is_some_and(
            |reason| reason.contains("watchdog") && reason.contains("seed-three-faults")
        ),
        "got {:?}",
        report.failure_reason
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_parent_watchdog_kills_the_exact_child_and_keeps_its_artifacts() {
    let root = repository().join("target/render-contract/hang");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the render contract owns target/render-contract");
    // The production parent watchdog is 50 seconds; this proves the kill path
    // itself against a child that has had its own watchdog disabled.
    assert_eq!(PARENT_WATCHDOG, Duration::from_secs(50));
    let launched = launch(
        &root,
        Some(VerificationFault::Hang),
        Duration::from_secs(10),
    );

    assert!(
        launched.killed,
        "a hung child must be killed by the parent watchdog"
    );
    assert_ne!(launched.code, Some(0));
    assert!(
        root.join(FrameName::HealthyCenterNorthEast.file_name())
            .is_file(),
        "a killed run must keep the frames it already captured"
    );
    let _ = fs::remove_dir_all(&root);
}
