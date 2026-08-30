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
        APP_WATCHDOG, ARTIFACT_NAMES, BadgeFacts, BlueprintFacts, CAPTURE_DELAY_LIMIT,
        CAPTURE_TIMEOUT, CLIP_DIFFERENCE_RANGE, CameraRenderFacts, DROP_CAPTURE_TIMEOUT,
        EQUIPMENT_REGION_MIN_PIXELS, EQUIPMENT_ROLE_MIN, EquipmentCategory, EquipmentFacts,
        FrameFacts, FrameMetrics, FrameName, GameplayFacts, HudRowFacts, LAUNCH_MARGIN,
        OUTSIDE_CROP_MAX, OWNED_NAMES, PALETTE_TOLERANCE, PARENT_WATCHDOG, PROBE_FILE_NAME,
        PixelRect, REPORT_FILE_NAME, RectFacts, SENTINEL_CLEAR, SENTINEL_MAX, STALL_WATCHDOG,
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

/// Preparing a directory writes and removes a probe, and that probe is a name
/// this module has published rather than an ad-hoc temporary.
///
/// The whole safety story of `VerifyOutput` is "it touches only names it has
/// declared", so a writability probe under an undeclared name was the one file
/// the type wrote that its own contract did not cover.
#[test]
fn the_writable_probe_is_a_declared_owned_name_and_never_survives() {
    assert!(
        OWNED_NAMES.contains(&PROBE_FILE_NAME),
        "the probe must be declared alongside the artifacts it proves are writable"
    );
    assert!(
        !ARTIFACT_NAMES.contains(&PROBE_FILE_NAME),
        "the probe is not published evidence, so it must not be an artifact"
    );
    assert_eq!(
        OWNED_NAMES.len(),
        ARTIFACT_NAMES.len() + 1,
        "the owned set is exactly the artifacts plus the probe"
    );
    for name in ARTIFACT_NAMES {
        assert!(OWNED_NAMES.contains(&name), "{name} must be owned");
    }

    let temp = TempDir::new("probe");
    let keeper = temp.join("someone-elses-notes.txt");
    fs::write(&keeper, b"keep me").expect("the test owns this file");
    let output = VerifyOutput::prepare(temp.path()).expect("the directory is legal");
    output.clear().expect("clearing named artifacts succeeds");

    let left = fs::read_dir(temp.path())
        .expect("the prepared directory is readable")
        .map(|entry| entry.expect("a readable entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    assert!(
        !left.contains(PROBE_FILE_NAME),
        "the probe must never outlive `prepare`, the directory held {left:?}"
    );
    assert_eq!(
        left,
        BTreeSet::from(["someone-elses-notes.txt".to_owned()]),
        "preparing and clearing must leave exactly what was already there"
    );
}

/// A probe name that is already taken belongs to somebody else, and `prepare`
/// must refuse rather than reuse it.
///
/// A plain file there is debris from a crashed run and not ours to truncate. A
/// symbolic link is the dangerous case: `fs::write` follows it, so a link left
/// pointing at any file this process can write would have been overwritten
/// with the probe text and then unlinked by the cleanup, under a name the
/// caller never supplied and inside a directory whose whole contract is that
/// it touches only names it declared.
#[test]
fn verify_output_refuses_a_probe_name_it_did_not_create() {
    // A regular file on the name.
    let temp = TempDir::new("probe-file");
    let probe = temp.join(PROBE_FILE_NAME);
    fs::write(&probe, b"someone got here first").expect("the test owns this file");

    let error = VerifyOutput::prepare(temp.path())
        .expect_err("a probe name this run did not create must be refused");
    assert_eq!(
        error,
        VerifyOutputError::StaleProbe {
            path: probe.clone()
        }
    );
    assert_eq!(
        fs::read_to_string(&probe).expect("the stale probe survives"),
        "someone got here first",
        "the refusal must leave the file it refused exactly as it found it"
    );

    // A symbolic link on the name, aimed at a file outside the directory.
    let linked = TempDir::new("probe-symlink-target");
    let target = linked.join("private.txt");
    fs::write(&target, b"do not touch").expect("the test owns this file");
    let temp = TempDir::new("probe-symlink");
    let probe = temp.join(PROBE_FILE_NAME);
    std::os::unix::fs::symlink(&target, &probe).expect("the test owns this link");

    let error = VerifyOutput::prepare(temp.path())
        .expect_err("a probe name held by a symbolic link must be refused");
    assert_eq!(
        error,
        VerifyOutputError::StaleProbe {
            path: probe.clone()
        }
    );
    assert!(
        fs::symlink_metadata(&probe)
            .expect("the link survives")
            .file_type()
            .is_symlink(),
        "the refusal must not unlink what it refused"
    );
    assert_eq!(
        fs::read_to_string(&target).expect("the link target survives"),
        "do not touch",
        "the refusal must never have written through the link"
    );

    // And the refusal is specific: the same directory prepares once the name
    // is free again, so this is not simply a directory that never prepares.
    fs::remove_file(&probe).expect("the test owns this link");
    VerifyOutput::prepare(temp.path()).expect("a free probe name prepares");
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

/// Every ordered pair of palette roles whose authored colours sit inside
/// [`PALETTE_TOLERANCE`] of one another, and therefore cannot be told apart by
/// a `near` measurement.
///
/// `near` asks "is this pixel within tolerance of that role", not "which role
/// is it nearest", so two roles this close both claim the same pixels. That is
/// deliberate — the tolerance exists so a shaded or dithered fill still reads
/// as its role — but it means the collisions have to be *known*, because a
/// contract that spends one role's evidence on another's pixels is not
/// measuring what it says. The two here are both benign:
///
/// * `HoseCharcoal` and `Ink` are the hose and the cel outline; the one group
///   that names either names both, `[HoseCharcoal, Ink]` for overhead routing,
///   so no contract distinguishes them in the first place;
/// * `HoseCharcoal` and `WorkerTrousers` are 8.7 apart, so a technician
///   standing under a hose drop contributes to that region. Every equipment
///   region is a projection of authored *static* geometry, and the technician
///   is a single 1.5 m figure in a 40 m hall, so no equipment contract can be
///   carried by trousers alone — but the pairing is pinned here so a palette
///   edit that makes it worse fails loudly.
///
/// A new collision appearing is a real change to what every `near`-based
/// contract measures, and this test is what refuses to let it appear quietly.
const KNOWN_PALETTE_COLLISIONS: [(PaletteRole, PaletteRole); 2] = [
    (PaletteRole::HoseCharcoal, PaletteRole::Ink),
    (PaletteRole::HoseCharcoal, PaletteRole::WorkerTrousers),
];

#[test]
fn only_the_known_palette_roles_are_indistinguishable_within_tolerance() {
    fn distance(left: PaletteRole, right: PaletteRole) -> f64 {
        let left = left.color().to_u8_array_no_alpha();
        let right = right.color().to_u8_array_no_alpha();
        (0..3)
            .map(|channel| (f64::from(left[channel]) - f64::from(right[channel])).powi(2))
            .sum::<f64>()
            .sqrt()
    }

    let mut found = Vec::new();
    for (index, left) in PaletteRole::ALL.into_iter().enumerate() {
        for right in PaletteRole::ALL.into_iter().skip(index + 1) {
            if distance(left, right) < PALETTE_TOLERANCE {
                found.push((left, right));
            }
        }
    }
    assert_eq!(
        found,
        KNOWN_PALETTE_COLLISIONS.to_vec(),
        "the set of palette roles a `near` measurement cannot separate has changed; each one \
         has to be reasoned about before it is accepted"
    );

    // The pin is load-bearing rather than a restatement of the palette: every
    // pinned pair really is inside the tolerance, and the roles the equipment
    // contracts lean on hardest really are outside it.
    for (left, right) in KNOWN_PALETTE_COLLISIONS {
        assert!(
            distance(left, right) < PALETTE_TOLERANCE,
            "{left:?} {right:?}"
        );
    }
    for (left, right) in [
        (PaletteRole::SignatureYellow, PaletteRole::FloorLight),
        (PaletteRole::SignatureYellow, PaletteRole::FloorShadow),
        (PaletteRole::FaultRed, PaletteRole::FloorLight),
        (PaletteRole::RackWhite, PaletteRole::FloorLight),
        (PaletteRole::Ink, PaletteRole::WorkerTrousers),
    ] {
        assert!(
            distance(left, right) >= PALETTE_TOLERANCE,
            "{left:?} and {right:?} are {} apart, so an equipment contract resting on {left:?} \
             could be paid for out of {right:?}",
            distance(left, right)
        );
    }
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
            capture_delay: None,
            flood: None,
        })
    );
    assert_eq!(
        parse(&["--verify-output=frames", "--verify-fault=stall"]),
        Ok(VerificationRequest {
            output: Some(PathBuf::from("frames")),
            fault: Some(VerificationFault::Stall),
            capture_delay: None,
            flood: None,
        })
    );
    assert_eq!(
        parse(&["--verify-output=frames", "--verify-capture-delay=30"]),
        Ok(VerificationRequest {
            output: Some(PathBuf::from("frames")),
            fault: None,
            capture_delay: Some(30),
            flood: None,
        })
    );
    assert_eq!(
        parse(&["--verify-output", "frames", "--verify-capture-delay", "0"]),
        Ok(VerificationRequest {
            output: Some(PathBuf::from("frames")),
            fault: None,
            capture_delay: Some(0),
            flood: None,
        })
    );
    assert_eq!(
        parse(&["--verify-flood", "4096"]),
        Ok(VerificationRequest {
            output: None,
            fault: None,
            capture_delay: None,
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
        vec!["--verify-capture-delay", "4"],
        vec!["--verify-output", "a", "--verify-capture-delay"],
        vec!["--verify-output", "a", "--verify-capture-delay", "soon"],
        vec!["--verify-output", "a", "--verify-capture-delay", "-1"],
        vec!["--verify-output", "a", "--verify-capture-delay", "601"],
        vec![
            "--verify-output",
            "a",
            "--verify-capture-delay",
            "4",
            "--verify-capture-delay",
            "5",
        ],
        vec!["--verify-flood", "16", "--verify-capture-delay", "4"],
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
    assert_eq!(
        CAPTURE_DELAY_LIMIT, 600,
        "the rejected 601 above is one past the documented limit"
    );
}

/// A readback delay holds a capture open for further pumps *after the observer
/// recorded it*, and `drop-capture` is defined as never recording one. Asking
/// for both is asking for a delay that can never be applied, and would quietly
/// produce an ordinary lost-callback run under a name suggesting otherwise.
#[test]
fn command_line_rejects_a_readback_delay_no_readback_can_ever_serve() {
    let reason = parse(&[
        "--verify-output",
        "a",
        "--verify-fault",
        "drop-capture",
        "--verify-capture-delay",
        "4",
    ])
    .expect_err("a delay on a run that records nothing is a usage error");
    assert!(
        reason.contains("--verify-capture-delay") && reason.contains("drop-capture"),
        "the usage error must name both flags: {reason}"
    );

    // The other faults do record their readbacks, so the combination is legal
    // and this rejection is specific rather than a blanket ban.
    for fault in [VerificationFault::Stall, VerificationFault::Hang] {
        let request = parse(&[
            "--verify-output",
            "a",
            "--verify-fault",
            fault.name(),
            "--verify-capture-delay",
            "4",
        ])
        .unwrap_or_else(|error| panic!("{} may carry a delay: {error}", fault.name()));
        assert_eq!(request.capture_delay, Some(4));
        assert_eq!(request.fault, Some(fault));
    }
}

/// Every injectable fault is reachable by its documented name, and no two
/// names collide.
///
/// The faults are the failure registry's executable half, so a fault that
/// stopped parsing would silently stop being proven end to end.
#[test]
fn every_injectable_fault_parses_from_its_documented_name() {
    assert_eq!(
        VerificationFault::ALL.map(VerificationFault::name).to_vec(),
        vec!["drop-capture", "stall", "hang"]
    );
    for fault in VerificationFault::ALL {
        assert_eq!(
            parse(&["--verify-output", "a", "--verify-fault", fault.name()])
                .expect("a documented fault name parses")
                .fault,
            Some(fault)
        );
    }
    assert_eq!(
        VerificationFault::ALL
            .map(VerificationFault::name)
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        VerificationFault::ALL.len(),
        "two faults may not answer to the same name"
    );
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

/// The production parent cap is a sum of named child budgets, not a round
/// number, and this is the launcher that really uses it.
///
/// The child polices its own active work with `APP_WATCHDOG` and each of its
/// fourteen readbacks with `CAPTURE_TIMEOUT`. The parent's only job is to be
/// the backstop for a child that has stopped honouring either, so its cap has
/// to sit above the longest life a *correct* child can have.
#[test]
fn the_parent_cap_is_the_sum_of_the_named_child_budgets() {
    assert_eq!(
        PARENT_WATCHDOG,
        APP_WATCHDOG + CAPTURE_TIMEOUT * FrameName::ALL.len() as u32 + LAUNCH_MARGIN,
        "the parent cap is active work + every readback's own budget + startup and shutdown"
    );
    assert_eq!(PARENT_WATCHDOG, Duration::from_secs(210));
    assert_eq!(ARTIFACT_NAMES.len(), FrameName::ALL.len() + 1);
}

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
    launch_delayed(output, fault, 0, watchdog)
}

/// Runs the game once, holding every screenshot readback open for
/// `capture_delay` further zero-time render pumps after the observer already
/// recorded it.
fn launch_delayed(
    output: &Path,
    fault: Option<VerificationFault>,
    capture_delay: u64,
    watchdog: Duration,
) -> Launch {
    let mut arguments = vec![
        "--verify-output".to_owned(),
        output.to_string_lossy().into_owned(),
    ];
    if let Some(fault) = fault {
        arguments.push("--verify-fault".to_owned());
        arguments.push(fault.name().to_owned());
    }
    if capture_delay > 0 {
        arguments.push("--verify-capture-delay".to_owned());
        arguments.push(capture_delay.to_string());
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
    render_delayed(root, label, 0)
}

/// One complete run whose every readback is held open for `capture_delay`
/// further zero-time render pumps.
fn render_delayed(root: &Path, label: &str, capture_delay: u64) -> RenderedRun {
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root).expect("the render contract owns target/render-contract");
    let launched = launch_delayed(root, None, capture_delay, PARENT_WATCHDOG);
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

/// The fraction of one prop's measured rectangles that a set of masked
/// rectangles actually paints over.
///
/// Counted per pixel rather than per rectangle, so overlapping masks are not
/// double counted and a rectangle that clips a corner contributes only the
/// corner. This is the quantity the collateral exemption is decided on: a
/// masked region either removed enough of a prop's evidence to explain that
/// prop failing, or it did not.
fn masked_coverage(facts: &FrameFacts, masked: &[RectFacts], prop: &[RectFacts]) -> f64 {
    let snap = |rect: &RectFacts| PixelRect::snap(*rect, facts.width, facts.height);
    let masks = masked.iter().map(snap).collect::<Vec<_>>();
    let mut total = 0u64;
    let mut covered = 0u64;
    for rect in prop.iter().map(snap) {
        total += rect.area();
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                if masks.iter().any(|mask| {
                    x >= mask.x
                        && x < mask.x + mask.width
                        && y >= mask.y
                        && y < mask.y + mask.height
                }) {
                    covered += 1;
                }
            }
        }
    }
    if total == 0 {
        return 0.0;
    }
    covered as f64 / total as f64
}

/// How much of a prop's own measured area a mask has to remove before that
/// prop failing counts as explained collateral rather than a regression.
///
/// A shared edge or a clipped corner is not an explanation. The authored
/// coupling this exists for — a tray hung over an aisle projecting onto the
/// rack row behind it — covers most of the rack segment it lands on, so half
/// is a threshold the real overlap clears comfortably while a grazing
/// rectangle cannot. Anything below it and the secondary failure has to stand
/// on its own as a genuine regression.
const COLLATERAL_COVERAGE_MIN: f64 = 0.5;

/// Whether masking `masked` removed enough of `prop`'s measured pixels to
/// explain `prop` failing too.
///
/// The earlier form of this asked only whether the two rectangle sets
/// intersected at all, which a single shared pixel satisfied — so a prop that
/// merely touched the masked family was excused from every contract it
/// subsequently failed, whatever the real cause.
fn mask_explains_collateral(facts: &FrameFacts, masked: &[RectFacts], prop: &[RectFacts]) -> bool {
    masked_coverage(facts, masked, prop) >= COLLATERAL_COVERAGE_MIN
}

/// A shared pixel is not an explanation.
///
/// The collateral exemption exists for the one authored coupling in the hall —
/// a tray hung over an aisle projects onto the rack row behind it — and it has
/// to be earned by the mask actually removing that prop's evidence. Deciding
/// it on bare intersection meant one touching pixel excused a prop from every
/// contract it went on to fail.
#[test]
fn collateral_is_explained_by_covered_pixels_and_not_by_a_shared_edge() {
    let facts = synthetic_facts();
    let rect = |x: f64, y: f64, width: f64, height: f64| RectFacts {
        x,
        y,
        width,
        height,
    };
    let prop = vec![rect(100.0, 100.0, 100.0, 100.0)];

    // One pixel of overlap in the corner: 1 of 10,000 covered.
    let corner = vec![rect(199.0, 199.0, 1.0, 1.0)];
    let coverage = masked_coverage(&facts, &corner, &prop);
    assert!(
        (coverage - 0.0001).abs() < 1.0e-9,
        "a one-pixel touch covers one pixel in ten thousand, measured {coverage}"
    );
    assert!(
        !mask_explains_collateral(&facts, &corner, &prop),
        "a single shared pixel may never excuse a prop from failing"
    );

    // No pixels at all, in both directions.
    assert_eq!(masked_coverage(&facts, &[], &prop), 0.0);
    assert!(!mask_explains_collateral(&facts, &[], &prop));
    assert_eq!(masked_coverage(&facts, &corner, &[]), 0.0);
    assert!(
        !mask_explains_collateral(&facts, &corner, &[]),
        "a prop that measured nothing has no evidence a mask could have removed"
    );

    // A rectangle that shares an edge but no interior pixel.
    let abutting = vec![rect(200.0, 100.0, 50.0, 100.0)];
    assert_eq!(masked_coverage(&facts, &abutting, &prop), 0.0);
    assert!(!mask_explains_collateral(&facts, &abutting, &prop));

    // Just under and just over the threshold.
    let half = vec![rect(100.0, 100.0, 100.0, 49.0)];
    assert!(!mask_explains_collateral(&facts, &half, &prop));
    let over = vec![rect(100.0, 100.0, 100.0, 51.0)];
    assert!(mask_explains_collateral(&facts, &over, &prop));

    // Overlapping masks are counted once, not twice: two rectangles that each
    // cover a third and share most of it must not add up to an explanation.
    let overlapping = vec![
        rect(100.0, 100.0, 100.0, 34.0),
        rect(100.0, 110.0, 100.0, 24.0),
    ];
    let coverage = masked_coverage(&facts, &overlapping, &prop);
    assert!(
        coverage < COLLATERAL_COVERAGE_MIN,
        "double counting overlapping masks would manufacture an explanation, measured {coverage}"
    );
    assert!(!mask_explains_collateral(&facts, &overlapping, &prop));

    // The real coupling still qualifies.
    let most = vec![rect(90.0, 90.0, 120.0, 120.0)];
    assert!((masked_coverage(&facts, &most, &prop) - 1.0).abs() < 1.0e-9);
    assert!(mask_explains_collateral(&facts, &most, &prop));
}

/// An empty region set is not proof that a prop is out of shot.
///
/// `on_screen` is the projection's own answer to "is this in the viewport";
/// `regions` is the separate question of whether anything measurable survived
/// clipping and the minimum-area floor. The per-prop rack-row contract used to
/// iterate only the measurable props, so a row that projected into the frame
/// and measured nothing was skipped in silence — indistinguishable, to the
/// gate, from a row that was legitimately off screen.
#[test]
fn an_on_screen_prop_with_no_measured_region_fails_instead_of_being_skipped() {
    /// One rack row's projected rectangle, laid out so four fit side by side.
    fn row_rect(row: usize) -> RectFacts {
        RectFacts {
            x: 40.0 + 300.0 * row as f64,
            y: 200.0,
            width: 200.0,
            height: 200.0,
        }
    }

    fn rack_row(row: usize, on_screen: bool, measurable: bool) -> EquipmentFacts {
        EquipmentFacts {
            id: format!("rack-row-{:02}", row + 1),
            category: EquipmentCategory::RackRows.name().to_owned(),
            world_bounds: [[-1.0, 0.0, -1.0], [1.0, 2.0, 1.0]],
            projected_bounds: [0.0, 0.0, 1.0, 1.0],
            on_screen,
            regions: if measurable {
                vec![row_rect(row)]
            } else {
                Vec::new()
            },
        }
    }

    /// A frame whose four rack rows are exactly as described, with every
    /// measurable row painted so it satisfies the rack role groups.
    fn evaluate(rows: [EquipmentFacts; 4]) -> Vec<String> {
        let mut facts = synthetic_facts();
        facts.equipment = rows.to_vec();
        let mut image = synthetic_frame(FIXTURE_WIDTH, FIXTURE_HEIGHT);
        for prop in &facts.equipment {
            for rect in &prop.regions {
                let snapped = PixelRect::snap(*rect, facts.width, facts.height);
                for y in snapped.y..snapped.y + snapped.height {
                    for x in snapped.x..snapped.x + snapped.width {
                        // Four fifths rack white, one fifth ink: comfortably
                        // over the role floor for both required groups.
                        let role = if y < snapped.y + snapped.height / 5 {
                            PaletteRole::Ink
                        } else {
                            PaletteRole::RackWhite
                        };
                        image.put_pixel(x, y, image::Rgb(role.color().to_u8_array_no_alpha()));
                    }
                }
            }
        }
        let regions = frame_regions(&facts);
        evaluate_frame(
            FrameName::HealthyCenterNorthEast,
            &facts,
            &FrameMetrics::compute(&image, &regions),
            reference_metrics(),
        )
        .into_iter()
        .map(|failure| failure.metric)
        .collect()
    }

    // All four rows in shot and measurable: no rack-row prop complains.
    let healthy = evaluate([
        rack_row(0, true, true),
        rack_row(1, true, true),
        rack_row(2, true, true),
        rack_row(3, true, true),
    ]);
    for row in 1..=4 {
        for suffix in ["", "-unmeasured"] {
            let metric = format!("equipment-rack-row-{row:02}{suffix}");
            assert!(
                !healthy.contains(&metric),
                "a fully measured hall must not report {metric}: {healthy:?}"
            );
        }
    }

    // The fourth row projects into the viewport and measures nothing. That is
    // the case this contract exists for, and it has to be a failure naming
    // that row alone.
    let unmeasured = evaluate([
        rack_row(0, true, true),
        rack_row(1, true, true),
        rack_row(2, true, true),
        rack_row(3, true, false),
    ]);
    assert!(
        unmeasured.contains(&"equipment-rack-row-04-unmeasured".to_owned()),
        "an on-screen rack row with no measurable region must fail: {unmeasured:?}"
    );
    for row in 1..=3 {
        let metric = format!("equipment-rack-row-{row:02}-unmeasured");
        assert!(
            !unmeasured.contains(&metric),
            "the measured rows must stay green, got {metric}"
        );
    }

    // The same row genuinely off screen is the documented exclusion, and must
    // not be reported — otherwise the new failure would just be noise.
    let off_screen = evaluate([
        rack_row(0, true, true),
        rack_row(1, true, true),
        rack_row(2, true, true),
        rack_row(3, false, false),
    ]);
    assert!(
        !off_screen.contains(&"equipment-rack-row-04-unmeasured".to_owned()),
        "a row that really is out of shot carries no region and no failure: {off_screen:?}"
    );
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
            } else if prop.regions.is_empty() {
                // The other direction, which is the one an empty region set
                // used to be allowed to fake. A prop that projects into the
                // viewport and measures nothing carries no evidence, and the
                // gate has to know which props those are by name rather than
                // discovering the set has quietly grown.
                assert!(
                    EXPECTED_ON_SCREEN_SLIVERS.contains(&(frame.file_name(), prop.id.as_str())),
                    "{frame:?} {} projects into the viewport at {:?} but measured no region; \
                     an unmeasured prop is not proof of being out of shot, so it has to be \
                     either measurable or pinned",
                    prop.id,
                    prop.projected_bounds
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

/// The individual rack rows that leave the viewport at a centre heading, by
/// frame and prop identifier.
///
/// The rack rows are the one family every prop of which has to carry its own
/// evidence, so "three of the four were measurable" is not a statement the
/// contract may accept on its own: it would pass just as happily if a
/// *different* row vanished, or if a row stopped spawning altogether. The
/// rows are at x = -12, -4, 4 and 12 spanning z = -8..8, and at the NorthEast
/// heading the camera's rectangle is centred on the technician at the spawn
/// point, which puts `rack-row-04` past the right edge. Every other centre
/// heading sees all four. This list is exhaustive and exact: a row that
/// disappears anywhere else, or a row that reappears here, fails.
const EXPECTED_OFF_SCREEN_ROWS: [(&str, &str); 1] = [("01-healthy-center-ne.png", "rack-row-04")];

/// The props that project into the viewport but measure nothing, by frame and
/// prop identifier.
///
/// These are painted floor markings the camera catches edge-on at the bottom
/// of the frame: the whole prop is in shot by a few pixels of height, so every
/// segment it is split into clips to a sliver below the
/// [`EQUIPMENT_REGION_MIN_PIXELS`] floor and is dropped before measurement.
/// That is a real and acceptable outcome — a four-pixel band carries no stable
/// ratio — but it is *not* the same thing as being off screen, and letting the
/// two look alike is exactly how an unmeasured prop hides. Every one is named
/// here, so a prop that newly stops being measurable fails until somebody
/// decides it is this and not a regression.
///
/// No rack row appears here, and none can: a rack row is two metres tall, so
/// it cannot reduce to a sliver while it is still in shot. The rows are the
/// one family contracted prop by prop, which is why that matters.
const EXPECTED_ON_SCREEN_SLIVERS: [(&str, &str); 10] = [
    ("01-healthy-center-ne.png", "floor-marking-aisle-03-east"),
    ("06-settled-se.png", "floor-marking-hazard-east"),
    ("06-settled-se.png", "floor-marking-hazard-north"),
    ("06-settled-se.png", "floor-marking-hazard-south"),
    ("07-settled-sw.png", "floor-marking-hazard-east"),
    ("07-settled-sw.png", "floor-marking-hazard-north"),
    ("07-settled-sw.png", "floor-marking-hazard-south"),
    ("08-settled-nw.png", "floor-marking-hazard-east"),
    ("08-settled-nw.png", "floor-marking-hazard-north"),
    ("08-settled-nw.png", "floor-marking-hazard-south"),
];

/// No rack row may be pinned as an unmeasured sliver: the rows are the family
/// every prop of which carries its own evidence, and a two-metre cabinet in
/// shot always projects a measurable region.
#[test]
fn no_individually_contracted_prop_is_pinned_as_an_unmeasured_sliver() {
    for (frame, id) in EXPECTED_ON_SCREEN_SLIVERS {
        assert!(
            !id.starts_with("rack-row-"),
            "{frame} {id}: a rack row may never be excused as a sliver"
        );
    }
    let mut seen = BTreeSet::new();
    for entry in EXPECTED_ON_SCREEN_SLIVERS {
        assert!(seen.insert(entry), "{entry:?} is pinned twice");
    }
    for (frame, id) in EXPECTED_OFF_SCREEN_ROWS {
        assert!(
            !EXPECTED_ON_SCREEN_SLIVERS.contains(&(frame, id)),
            "{frame} {id} cannot be both off screen and an on-screen sliver"
        );
    }
}

/// The measured share, for one category on one frame, of the weakest role
/// group in whichever of that category's regions carries the most evidence.
///
/// This is exactly the quantity `evaluate_frame` gates on, recomputed here so
/// the margin over the acceptance floor is visible and assertable.
fn category_best_share(
    facts: &FrameFacts,
    metrics: &FrameMetrics,
    category: EquipmentCategory,
) -> f64 {
    category_props(facts, category)
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
        .fold(0.0f64, f64::max)
}

#[test]
fn every_center_frame_carries_real_equipment_pixels_with_margin() {
    let run = rendered_run();
    let mut report = Vec::new();
    let mut weakest: Option<(String, f64)> = None;
    for frame in CENTER_FRAMES {
        let facts = run.facts(frame);
        let metrics = run.metrics(frame);
        for category in EquipmentCategory::ALL {
            if EXPECTED_OFF_SCREEN.contains(&(frame.file_name(), category.name())) {
                continue;
            }
            let best = category_best_share(facts, metrics, category);
            let label = format!("{} {}", frame.file_name(), category.name());
            report.push(format!("{label} {best:.4}"));
            if weakest.as_ref().is_none_or(|(_, low)| best < *low) {
                weakest = Some((label.clone(), best));
            }
            assert!(
                best >= EQUIPMENT_ROLE_MIN * 2.0,
                "{label} measured {best:.4}, which leaves no margin over the {EQUIPMENT_ROLE_MIN} \
                 floor"
            );
        }
    }
    println!("equipment margins:\n{}", report.join("\n"));

    // The thinnest margin in the table belongs to the painted floor markings,
    // and that is geometry rather than luck: a marking is a flat strip, so the
    // tightest rectangle its bounds project into is mostly the floor it is
    // painted on. What matters is not that the number is large but that all of
    // it is the equipment's own — so the same measurement is repeated with the
    // category painted out, and has to collapse under the acceptance floor. A
    // share partly supplied by the surrounding room would survive that.
    let (label, low) = weakest.expect("the centre frames measure at least one category");
    println!("weakest equipment margin: {label} {low:.4}");
    for frame in CENTER_FRAMES {
        let facts = run.facts(frame);
        for category in EquipmentCategory::ALL {
            if EXPECTED_OFF_SCREEN.contains(&(frame.file_name(), category.name())) {
                continue;
            }
            let regions = category_regions(facts, category);
            let masked = mask_regions(run.frame(frame), facts, &regions);
            let residue = category_best_share(
                facts,
                &FrameMetrics::compute(&masked, &frame_regions(facts)),
                category,
            );
            assert!(
                residue < EQUIPMENT_ROLE_MIN,
                "{} {} keeps {residue:.4} of its evidence with every one of its own pixels \
                 painted out, so that share is not equipment-specific",
                frame.file_name(),
                category.name()
            );
        }
    }
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

                // Per-prop collateral is allowed only where the report's own
                // geometry says the masked rectangles really do cover part of
                // that prop's measured rectangle. The hall is authored so that
                // this happens: `OVERHEAD_TRAY_HEIGHT` hangs the trays over
                // the aisles, and at every centre heading a tray projects onto
                // the rack row behind it, so masking either family paints over
                // some of the other's pixels. Deciding it from the measured
                // rectangles rather than exempting the pair by name means a
                // tray that had stopped being drawn — and so overlapped
                // nothing — could not use the exemption.
                let props = equipment
                    .iter()
                    .filter(|metric| is_prop_metric(metric, facts, other))
                    .filter(|metric| {
                        let id = metric.trim_start_matches("equipment-");
                        !facts
                            .equipment
                            .iter()
                            .find(|prop| prop.id == id)
                            .is_some_and(|prop| {
                                mask_explains_collateral(facts, &regions, &prop.regions)
                            })
                    })
                    .collect::<Vec<_>>();
                assert!(
                    props.is_empty(),
                    "{frame:?}: masking {} must leave green every {} prop whose own \
                     measured area it did not mostly paint over, got {props:?}",
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

        // Which rows are measurable is pinned by name, not counted. A bare
        // "at least three of four" would pass just as happily if a different
        // row vanished, or if the hall stopped spawning one entirely.
        let measured = rows
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        let expected = (1..=4)
            .map(|rack| format!("rack-row-{rack:02}"))
            .filter(|id| !EXPECTED_OFF_SCREEN_ROWS.contains(&(frame.file_name(), id.as_str())))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            measured, expected,
            "{frame:?} must measure exactly the rack rows that are in shot; the only rows allowed \
             out of shot anywhere are {EXPECTED_OFF_SCREEN_ROWS:?}"
        );
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
            for (other, other_regions) in &rows {
                if other == id || mask_explains_collateral(facts, regions, other_regions) {
                    continue;
                }
                assert!(
                    !failures.contains(&format!("equipment-{other}")),
                    "{frame:?}: masking {id} must leave {other} green unless it painted \
                     over most of {other}, got {failures:?}"
                );
            }
        }
    }
}

/// Every rack row is out of shot on at most the frames pinned above, so no row
/// can quietly stop being evidence on every frame at once.
#[test]
fn every_rack_row_is_measured_on_at_least_three_centre_frames() {
    let run = rendered_run();
    for rack in 1..=4 {
        let id = format!("rack-row-{rack:02}");
        let measured = CENTER_FRAMES
            .into_iter()
            .filter(|frame| {
                run.facts(*frame)
                    .equipment
                    .iter()
                    .any(|prop| prop.id == id && !prop.regions.is_empty())
            })
            .count();
        let allowed = CENTER_FRAMES
            .into_iter()
            .filter(|frame| EXPECTED_OFF_SCREEN_ROWS.contains(&(frame.file_name(), id.as_str())))
            .count();
        assert_eq!(
            measured,
            CENTER_FRAMES.len() - allowed,
            "{id} must be measurable on every centre frame it is not pinned as out of shot on"
        );
        assert!(
            measured >= 3,
            "{id} must carry evidence on at least three centre frames, it carried {measured}"
        );
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
                &Banned {
                    keys: Vec::new(),
                    values: vec![other.to_string_lossy().into_owned()],
                },
                label,
            );
        }
    }
    let _ = fs::remove_dir_all(&second.root);
}

/// How many further render pumps the slow reproduction holds each readback for.
///
/// It is deliberately past the twenty-four frame budget captures used to be
/// waited out with, so this run is one the retired fixed-frame rule would have
/// failed outright. Because a pending capture advances no simulated time, the
/// only thing it costs is pumps, and those are exactly what the evidence must
/// be independent of.
const SLOW_READBACK_PUMPS: u64 = 30;

/// The number of render pumps a readback takes is a property of the machine,
/// never of the game. Two runs of the same journey that differ only in how
/// long every callback takes must produce the same evidence, byte for byte.
///
/// The prompt half of that comparison is the primary run itself. It is already
/// a full journey taken with no injected delay, so launching a second identical
/// child to be the control would spend a whole cold software-rendered run —
/// tens of seconds on llvmpipe — re-establishing something already on disk.
#[test]
fn the_readback_pump_count_never_reaches_the_evidence() {
    let quick = rendered_run();
    let slow = render_delayed(
        &repository().join("target/render-contract/slow-readback"),
        "slow-readback",
        SLOW_READBACK_PUMPS,
    );

    assert_eq!(quick.report.result, "success");
    assert_eq!(slow.report.result, "success");
    assert_ne!(
        quick.root, slow.root,
        "the two runs must use different directories"
    );
    assert_eq!(
        quick.canonical, slow.canonical,
        "a slow readback must not change one byte of the canonical report"
    );
    assert_eq!(
        semantic_hash(&quick.canonical),
        semantic_hash(&slow.canonical),
        "the semantic hash must not depend on readback latency"
    );

    for frame in FrameName::ALL {
        let left = fs::read(quick.root.join(frame.file_name()))
            .unwrap_or_else(|error| panic!("{} is readable: {error}", frame.file_name()));
        let right = fs::read(slow.root.join(frame.file_name()))
            .unwrap_or_else(|error| panic!("{} is readable: {error}", frame.file_name()));
        assert_eq!(
            left,
            right,
            "{} must be the same pixels whatever the readback cost",
            frame.file_name()
        );
    }

    let _ = fs::remove_dir_all(&slow.root);
}

/// Fails when a canonical document carries any banned key or substring.
fn assert_scan_is_clean(canonical: &str, banned: &Banned, label: &str) {
    let found = scan_violations(canonical, banned);
    assert!(
        found.is_empty(),
        "the {label} canonical report must not carry {found:?}"
    );
}

/// Everything a canonical report may never carry.
///
/// The scan walks the parsed document rather than grepping its text, because
/// the two questions it asks are structural. A *key* is judged by name and
/// matched whole, so a schema that one day gains `user_presses` or
/// `elapsed_ticks` is not failed for a name that leaks nothing. A *value* is
/// judged by shape: the report's paths are all repository-relative by
/// contract, so anything absolute is a leak whatever key it hides under, and
/// so is any value carrying this host's own names.
struct Banned {
    keys: Vec<String>,
    /// Values that identify this machine or the directory it ran in.
    values: Vec<String>,
}

/// Whether one string value is an absolute path in any form the report might
/// pick up.
///
/// Report values are read after JSON unescaping, so a Windows path that was
/// serialized as `"C:\\Users\\bob"` is examined here as `C:\Users\bob` — which
/// is why the drive test is one character and a colon rather than a search for
/// doubled backslashes. Any drive letter counts, not just `C`.
fn is_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    // UNC, `\\server\share`, and its forward-slash spelling.
    if value.starts_with("\\\\") || value.starts_with("//") {
        return true;
    }
    // Unix absolute, and a Windows root-relative `\path`.
    if value.starts_with('/') || value.starts_with('\\') {
        return true;
    }
    // Any drive letter: `C:\`, `d:/`, or a bare `C:`.
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }
    false
}

/// Whether one string value climbs out of the directory it is relative to.
fn escapes_upwards(value: &str) -> bool {
    value.split(['/', '\\']).any(|component| component == "..")
}

/// Every banned key or value one canonical document actually carries.
///
/// Reported as `key = value` so a failure names the leak rather than the rule.
fn scan_violations(canonical: &str, banned: &Banned) -> Vec<String> {
    let document: serde_json::Value =
        serde_json::from_str(canonical).expect("a canonical report is JSON");
    let mut found = Vec::new();
    walk_json(&document, "", banned, &mut found);
    found
}

/// Walks every key and string value of one parsed document.
fn walk_json(value: &serde_json::Value, key: &str, banned: &Banned, found: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, child) in fields {
                if banned.keys.iter().any(|banned| banned == name) {
                    found.push(format!("key {name}"));
                }
                walk_json(child, name, banned, found);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                walk_json(item, key, banned, found);
            }
        }
        serde_json::Value::String(text) => {
            if is_absolute_path(text) {
                found.push(format!("absolute path at {key}: {text}"));
            }
            if escapes_upwards(text) {
                found.push(format!("upward path at {key}: {text}"));
            }
            for needle in &banned.values {
                if !needle.is_empty() && text.contains(needle.as_str()) {
                    found.push(format!("host value at {key}: {text}"));
                }
            }
        }
        _ => {}
    }
}

/// Everything a canonical report may never carry, whoever produced it.
fn banned_report_substrings() -> Banned {
    let mut values = vec![repository().to_string_lossy().into_owned()];
    for name in ["HOME", "USER", "LOGNAME", "HOSTNAME"] {
        if let Some(value) = std::env::var_os(name) {
            let value = value.to_string_lossy().into_owned();
            if value.len() >= 3 {
                values.push(value);
            }
        }
    }
    if let Ok(host) = std::process::Command::new("hostname").output() {
        let host = String::from_utf8_lossy(&host.stdout).trim().to_owned();
        if host.len() >= 3 {
            values.push(host);
        }
    }
    Banned {
        keys: [
            "timestamp",
            "generated_at",
            "elapsed",
            "duration_ms",
            "hostname",
            "host",
            "user",
            "username",
            "cwd",
            "BEVY_ASSET_ROOT",
            "CARGO",
            "RUST_LOG",
            "HOME",
            "PATH",
        ]
        .map(str::to_owned)
        .to_vec(),
        values,
    }
}

/// The scan has to catch what it exists to catch and nothing else.
///
/// Every positive fixture is a leak the report would be wrong to publish;
/// every negative one is a value the report legitimately carries today.
#[test]
fn the_privacy_scan_reads_paths_and_keys_rather_than_bare_words() {
    let banned = banned_report_substrings();
    let document = |body: &str| format!("{{\"frames\": {body}}}");

    for leak in [
        // Keys, whatever their value.
        "{\"user\": 1}".to_owned(),
        "{\"timestamp\": 1}".to_owned(),
        "{\"elapsed\": 1}".to_owned(),
        "{\"hostname\": 1}".to_owned(),
        "{\"cwd\": 1}".to_owned(),
        "{\"HOME\": 1}".to_owned(),
        "{\"PATH\": 1}".to_owned(),
        "{\"BEVY_ASSET_ROOT\": 1}".to_owned(),
        // Unix absolute paths, under an innocent key.
        document("\"/var/folders/mh/x/report.json\""),
        document("\"/home/runner/work/midcreek\""),
        document("\"/Users/someone/frames\""),
        // Windows drives, on any letter, as the document really carries them.
        document(r#""C:\\Users\\someone\\frames""#),
        document(r#""D:\\build\\midcreek""#),
        document(r#""z:/build/midcreek""#),
        // UNC, both spellings.
        document(r#""\\\\build-server\\share\\frames""#),
        document("\"//build-server/share/frames\""),
        // Upward escapes at any depth.
        document("\"../../etc/passwd\""),
        document("\"assets/../../../etc/passwd\""),
        // Nested inside arrays and objects, not just at the top level.
        "{\"gameplay\": {\"keys\": [{\"stage\": \"/Users/someone\"}]}}".to_owned(),
    ] {
        assert!(
            !scan_violations(&leak, &banned).is_empty(),
            "the scan must catch {leak}"
        );
    }

    for innocent in [
        // The repository-relative values the report is built out of.
        document("\"assets/generated/hall.glb\""),
        document("\"src/verification.rs\""),
        document("\"01-healthy-center-ne.png\""),
        document("\"assets/references/key-art.png\""),
        // Keys that merely contain a banned word.
        "{\"user_presses\": 2}".to_owned(),
        "{\"elapsed_ticks\": 2}".to_owned(),
        "{\"pathological\": 2}".to_owned(),
        "{\"repaired_rack\": 1}".to_owned(),
        // A value that contains a colon but is not a drive.
        document("\"rack-row-01:segment-3\""),
        // A stage name with a dot that is not an upward component.
        document("\"begin-repair.stage\""),
    ] {
        assert!(
            scan_violations(&innocent, &banned).is_empty(),
            "the scan must not fail a legitimate value: {innocent}, got {:?}",
            scan_violations(&innocent, &banned)
        );
    }
}

#[test]
fn the_real_canonical_report_carries_no_wall_clock_host_path_or_environment() {
    let run = rendered_run();
    let banned = banned_report_substrings();
    assert_scan_is_clean(&run.canonical, &banned, "real");

    // The scan has to be load-bearing rather than vacuous: doctoring the real
    // document with the absolute output root it was written into, a host
    // clock, and an environment name must be caught by the same scan. Each
    // injection is a well-formed field, because the scan now parses the
    // document rather than grepping it.
    for injected in [
        format!(
            "\"leaked_root\": {},",
            serde_json::to_string(&run.root.to_string_lossy().into_owned())
                .expect("a path serializes")
        ),
        "\"generated_at\": \"2026-01-01T00:00:00Z\",".to_owned(),
        "\"BEVY_ASSET_ROOT\": \"x\",".to_owned(),
        "\"leaked_unc\": \"\\\\\\\\build-server\\\\share\",".to_owned(),
        "\"leaked_drive\": \"D:\\\\build\\\\midcreek\",".to_owned(),
    ] {
        let doctored = run
            .canonical
            .replace("\"frames\":", &format!("{injected}\n  \"frames\":"));
        assert!(
            serde_json::from_str::<serde_json::Value>(&doctored).is_ok(),
            "the doctored document must stay parseable, or the scan is not being exercised: \
             {injected}"
        );
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
    // The child's readback budget here is a fast test override, not the
    // production one. The fixture never records a callback, so the only way it
    // can end is by waiting the budget out — and what it proves, a lost
    // callback failing the run with its frame, stage, and artifact state, is
    // the same proof at two seconds as at ten. The override lives on the
    // injected fault, which nothing but `--verify-fault drop-capture` reaches.
    assert!(
        DROP_CAPTURE_TIMEOUT < CAPTURE_TIMEOUT,
        "the override exists to be faster than production, not to weaken it"
    );
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
    // A frame whose callback never landed is not evidence, and the failure
    // report may not describe it as though it were.
    assert!(
        report.frames.is_empty(),
        "a run that got no frame back must report no frame facts, it reported {:?}",
        report.frames.keys().collect::<Vec<_>>()
    );
    let _ = fs::remove_dir_all(&root);
}

/// How long the parent gives the stall fixture. It is a fast override, not the
/// production cap: the child fails itself after `STALL_WATCHDOG` of inactivity,
/// and this only has to be far enough above that to distinguish "the app
/// watchdog fired" from "the parent gave up".
const STALL_PARENT_CAP: Duration = Duration::from_secs(90);

#[test]
fn the_app_watchdog_fails_a_stalled_run_with_its_stage_name() {
    let root = repository().join("target/render-contract/stall");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the render contract owns target/render-contract");
    // Both budgets here are deliberately short. What the fixture proves is
    // that an inactive state machine is failed *by the inactivity timeout*,
    // naming the stage it is stuck in — and that proof is identical at twenty
    // seconds and at forty-five, so the gate is not made to sit through the
    // production budget to re-measure a constant. The override lives on the
    // injected fault, which nothing but `--verify-fault stall` can reach, so
    // the production constants are untouched.
    assert!(STALL_WATCHDOG < APP_WATCHDOG && STALL_PARENT_CAP < PARENT_WATCHDOG);
    let launched = launch(&root, Some(VerificationFault::Stall), STALL_PARENT_CAP);
    let elapsed = launched.elapsed;

    assert!(
        !launched.killed,
        "the app watchdog must fire before the parent"
    );
    assert_eq!(launched.code, Some(1));
    assert!(
        elapsed >= STALL_WATCHDOG && elapsed < STALL_PARENT_CAP,
        "the app watchdog must fire between {STALL_WATCHDOG:?} and {STALL_PARENT_CAP:?}, \
         took {elapsed:?}"
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
    // The cap here is a fast test override, not the production one. Production
    // is `PARENT_WATCHDOG`, and waiting it out would add three and a half
    // minutes to prove a kill that is provable in ten seconds: what is under
    // test is that the parent kills *that exact child process* and keeps its
    // artifacts, which is the same code path at any cap. The child has had its
    // own watchdog disabled by the injected fault, so nothing else can stop it.
    const HANG_PARENT_CAP: Duration = Duration::from_secs(10);
    assert!(HANG_PARENT_CAP < PARENT_WATCHDOG);
    let launched = launch(&root, Some(VerificationFault::Hang), HANG_PARENT_CAP);

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
