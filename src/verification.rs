//! Autonomous gameplay and render verification.
//!
//! This module drives the *real* game — the same plugins `cargo run` builds —
//! through one scripted, deterministic journey, captures fourteen named frames
//! from the real renderer, and writes one canonical semantic report. Nothing in
//! here substitutes a synthetic scene for the product: every frame is a real
//! screenshot of the real window taken with
//! [`Screenshot::primary_window`](bevy::render::view::screenshot::Screenshot::primary_window)
//! and [`save_to_disk`](bevy::render::view::screenshot::save_to_disk).
//!
//! # Stage machine
//!
//! ```text
//! Boot
//!  -> WaitForAssets
//!  -> ValidateBlueprint
//!  -> HealthyCapture
//!  -> SeedThreeFaults
//!  -> FaultQueueCapture
//!  -> KeyboardJourney
//!  -> WalkCapture
//!  -> BeginRepair
//!  -> RepairCapture
//!  -> CompleteRepair
//!  -> ResolvedCapture
//!  -> OrbitSouthEast -> SettledSouthEastCapture
//!  -> OrbitSouthWest -> SettledSouthWestCapture
//!  -> OrbitNorthWest -> SettledNorthWestCapture
//!  -> MidOrbitCapture
//!  -> CornerProbes
//!  -> LowResolutionCapture
//!  -> AnalyzeReady
//!  -> WriteReport
//!  -> Success
//!
//! Any invalid transition, missing entity, asset failure, capture failure, or
//! watchdog expiry -> Failure -> AppExit::error()
//! ```
//!
//! The only legal transitions are `stage -> stage.next()` and, from any
//! non-terminal stage, `stage -> Failure`. [`StageMachine`] enforces that; it
//! never moves on a rejected transition and never resumes after a failure.
//!
//! # Output safety
//!
//! [`VerifyOutput`] is fail-closed. It refuses an empty path, a path with a
//! `..` component, the filesystem root, a symbolic link, anything that is not a
//! directory, an unwritable directory, and any directory in which one of the
//! fifteen owned artifact names is already something other than a regular file.
//! It creates at most the final missing path component, and it only ever
//! removes the exact artifact names this module writes. No code path in this
//! module recursively deletes a caller-supplied path.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs, io,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use bevy::{
    camera::primitives::Aabb,
    color::{ColorToPacked, Srgba},
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    input::{
        ButtonState,
        keyboard::{Key, KeyboardInput, NativeKey},
    },
    prelude::*,
    render::view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk},
    time::TimeUpdateStrategy,
    ui::{ComputedNode, UiGlobalTransform},
};
use image::RgbImage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CellShiftSet,
    assetgen::ASSET_NAMES,
    assets::AssetLoadState,
    camera::{
        CEL_SHIFT_DEBAND_DITHER, CEL_SHIFT_TONEMAPPING, CameraHeading, CameraOrbit,
        CellShiftCamera, clamp_follow_target, ground_quadrilateral,
    },
    design::{
        AssetKind, MAX_ACTIVE_TICKETS, PLAYER_RADIUS, PaletteRole, RACK_ROW_X,
        RENDER_COVERAGE_SIZE, ROOM_SIZE, SceneBlueprint, VERIFICATION_WINDOW_HEIGHT,
        VERIFICATION_WINDOW_WIDTH,
    },
    hud::{
        BadgeKind, BadgeVisibility, ControlsPanel, HudReport, HudStatus, QueueRowNode,
        TicketQueuePanel, badge_half_extents,
    },
    metrics::{
        FrameMetrics, MeasureSource, PALETTE_TOLERANCE, PixelRect, WORKER_REGION, palette_table,
        squared_distance,
    },
    operations::{
        FAULT_INTERVAL, FAULT_SCHEDULER_SEED, FaultScheduler, InteractionOutcome, LastInteraction,
        MovementLock, OperationsClock, REPAIR_KEY, RackOperations, RackRoster, RackState,
        TicketQueue, TicketSeverity,
    },
    player::{
        PlayerAnimationState, PlayerAnimations, PlayerClip, PlayerParts, PlayerRigReport,
        PlayerRigState, Technician, ViewBasis, required_player_parts,
    },
    world::{HallBlueprint, HallProp, HallState, PlayerSpawnPoint},
};

// ---------------------------------------------------------------------------
// Named artifacts
// ---------------------------------------------------------------------------

/// The canonical semantic report every run writes.
pub const REPORT_FILE_NAME: &str = "report.json";

/// One of the fourteen reviewed captures.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FrameName {
    /// Healthy hall, technician at the spawn point, NorthEast heading.
    HealthyCenterNorthEast,
    /// Three simultaneous seeded tickets, NorthEast heading.
    FaultQueueNorthEast,
    /// Mid-walk pose driven by real arrow keys, NorthEast heading.
    WalkNorthEast,
    /// A running repair, movement locked, NorthEast heading.
    RepairingNorthEast,
    /// The resolved indicator, NorthEast heading.
    ResolvedNorthEast,
    /// Settled SouthEast heading.
    SettledSouthEast,
    /// Settled SouthWest heading.
    SettledSouthWest,
    /// Settled NorthWest heading.
    SettledNorthWest,
    /// Halfway through one real `E` quarter turn.
    MidOrbit,
    /// The technician in a room corner, NorthEast heading.
    CornerNorthEast,
    /// The technician in a room corner, SouthEast heading.
    CornerSouthEast,
    /// The technician in a room corner, SouthWest heading.
    CornerSouthWest,
    /// The technician in a room corner, NorthWest heading.
    CornerNorthWest,
    /// The three-ticket layout at the 960x540 verification resolution.
    LowResolutionQueue,
}

impl FrameName {
    /// Every frame, in capture order.
    pub const ALL: [Self; 14] = [
        Self::HealthyCenterNorthEast,
        Self::FaultQueueNorthEast,
        Self::WalkNorthEast,
        Self::RepairingNorthEast,
        Self::ResolvedNorthEast,
        Self::SettledSouthEast,
        Self::SettledSouthWest,
        Self::SettledNorthWest,
        Self::MidOrbit,
        Self::CornerNorthEast,
        Self::CornerSouthEast,
        Self::CornerSouthWest,
        Self::CornerNorthWest,
        Self::LowResolutionQueue,
    ];

    /// The exact file this frame is written to, relative to the output root.
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::HealthyCenterNorthEast => "01-healthy-center-ne.png",
            Self::FaultQueueNorthEast => "02-fault-queue-ne.png",
            Self::WalkNorthEast => "03-walk-ne.png",
            Self::RepairingNorthEast => "04-repairing-ne.png",
            Self::ResolvedNorthEast => "05-resolved-ne.png",
            Self::SettledSouthEast => "06-settled-se.png",
            Self::SettledSouthWest => "07-settled-sw.png",
            Self::SettledNorthWest => "08-settled-nw.png",
            Self::MidOrbit => "09-mid-orbit.png",
            Self::CornerNorthEast => "10-corner-ne.png",
            Self::CornerSouthEast => "11-corner-se.png",
            Self::CornerSouthWest => "12-corner-sw.png",
            Self::CornerNorthWest => "13-corner-nw.png",
            Self::LowResolutionQueue => "14-low-resolution-queue.png",
        }
    }

    /// The exact pixel size this frame must have.
    pub const fn size(self) -> (u32, u32) {
        match self {
            Self::LowResolutionQueue => (
                crate::design::VERIFICATION_WINDOW_WIDTH,
                crate::design::VERIFICATION_WINDOW_HEIGHT,
            ),
            _ => (
                crate::design::DEFAULT_WINDOW_WIDTH,
                crate::design::DEFAULT_WINDOW_HEIGHT,
            ),
        }
    }

    /// Whether this frame is captured at a settled heading, where the magenta
    /// sentinel gate applies at its strictest.
    pub const fn is_settled(self) -> bool {
        !matches!(self, Self::MidOrbit)
    }

    /// Whether this frame is one of the four settled headings taken from the
    /// middle of the hall, where the authored equipment is in shot.
    ///
    /// These are the frames the equipment contracts run on: one per heading,
    /// with the technician at the spawn point or at the repaired rack rather
    /// than pushed into a room corner, so every family of authored equipment
    /// has something inside the orthographic rectangle to be seen in.
    pub const fn is_center_settled(self) -> bool {
        matches!(
            self,
            Self::HealthyCenterNorthEast
                | Self::SettledSouthEast
                | Self::SettledSouthWest
                | Self::SettledNorthWest
        )
    }
}

/// The scratch file [`VerifyOutput::prepare`] writes and removes to prove the
/// directory is writable before a run commits to it.
///
/// It is named here rather than inlined because this module's contract is that
/// it touches only files it has declared. The probe is not an artifact — it
/// never survives `prepare`, is never published, and is never cleared — so it
/// is deliberately not in [`ARTIFACT_NAMES`]; [`OWNED_NAMES`] is the union the
/// contract is stated over.
pub const PROBE_FILE_NAME: &str = ".midcreek-verify-probe";

/// Every artifact a run publishes, in stable order.
pub const ARTIFACT_NAMES: [&str; 15] = [
    "01-healthy-center-ne.png",
    "02-fault-queue-ne.png",
    "03-walk-ne.png",
    "04-repairing-ne.png",
    "05-resolved-ne.png",
    "06-settled-se.png",
    "07-settled-sw.png",
    "08-settled-nw.png",
    "09-mid-orbit.png",
    "10-corner-ne.png",
    "11-corner-se.png",
    "12-corner-sw.png",
    "13-corner-nw.png",
    "14-low-resolution-queue.png",
    REPORT_FILE_NAME,
];

/// Every file name this module may ever write or remove, in stable order.
///
/// This is the whole of it: the published [`ARTIFACT_NAMES`] plus the writable
/// probe. Anything outside this set is a name the module has no business
/// touching, which is the property [`VerifyOutput`] exists to hold. It is
/// derived from [`ARTIFACT_NAMES`] rather than restated, so a fifteenth frame
/// joins it without anyone editing a second list.
pub const OWNED_NAMES: [&str; ARTIFACT_NAMES.len() + 1] = owned_names();

const fn owned_names() -> [&'static str; ARTIFACT_NAMES.len() + 1] {
    let mut names = [PROBE_FILE_NAME; ARTIFACT_NAMES.len() + 1];
    let mut index = 0;
    while index < ARTIFACT_NAMES.len() {
        names[index] = ARTIFACT_NAMES[index];
        index += 1;
    }
    names
}

// ---------------------------------------------------------------------------
// Output directory
// ---------------------------------------------------------------------------

/// Every way a supplied `--verify-output` path is refused. Each variant names
/// the offending path so the CLI can print a cause rather than a code.
#[derive(Clone, Debug, PartialEq)]
pub enum VerifyOutputError {
    /// The caller supplied an empty path.
    Empty,
    /// The path contains a `..` component, so it could escape anywhere.
    ParentTraversal {
        /// The offending path, exactly as supplied.
        path: PathBuf,
    },
    /// The path is a filesystem root or has no parent to create it in.
    RefusedRoot {
        /// The offending path, exactly as supplied.
        path: PathBuf,
    },
    /// The path exists but is not a directory.
    NotADirectory {
        /// The offending path.
        path: PathBuf,
    },
    /// The path is a symbolic link, so writing through it leaves the named
    /// directory.
    SymbolicLink {
        /// The offending path.
        path: PathBuf,
    },
    /// The path does not exist and neither does its parent.
    MissingParent {
        /// The parent that would have had to be created too.
        parent: PathBuf,
    },
    /// The directory could not be created or written to.
    Unwritable {
        /// The offending path.
        path: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// One of the owned artifact names already exists as something other than
    /// a regular file, so this module would have to remove a directory or
    /// follow a link to write it.
    UnsafeArtifact {
        /// The owned artifact name.
        name: String,
        /// The offending path.
        path: PathBuf,
    },
    /// The writable probe name is already taken by something this run did not
    /// create, so proving the directory writable would mean overwriting,
    /// following, or unlinking a stranger.
    StaleProbe {
        /// The offending path.
        path: PathBuf,
    },
}

impl fmt::Display for VerifyOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "--verify-output requires a directory path"),
            Self::ParentTraversal { path } => write!(
                formatter,
                "refusing the verification output path {} because it contains a `..` component",
                path.display()
            ),
            Self::RefusedRoot { path } => write!(
                formatter,
                "refusing {} as a verification output directory because it has no parent",
                path.display()
            ),
            Self::NotADirectory { path } => write!(
                formatter,
                "the verification output path {} exists and is not a directory",
                path.display()
            ),
            Self::SymbolicLink { path } => write!(
                formatter,
                "refusing the symbolic link {} as a verification output directory",
                path.display()
            ),
            Self::MissingParent { parent } => write!(
                formatter,
                "the verification output parent directory {} does not exist",
                parent.display()
            ),
            Self::Unwritable { path, reason } => write!(
                formatter,
                "the verification output directory {} is not writable: {reason}",
                path.display()
            ),
            Self::UnsafeArtifact { name, path } => write!(
                formatter,
                "refusing to write {name} because {} is not a regular file",
                path.display()
            ),
            Self::StaleProbe { path } => write!(
                formatter,
                "refusing to prove {} writable because the probe name is already taken; this run \
                 did not create it and will not overwrite, follow, or remove it",
                path.display()
            ),
        }
    }
}

impl std::error::Error for VerifyOutputError {}

/// A validated verification output directory.
///
/// Construction is the whole safety boundary: once a [`VerifyOutput`] exists,
/// the directory has been proven to be a real, non-symlinked, writable
/// directory whose owned artifact names are free, and the only paths this type
/// hands out are `root/<owned artifact name>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyOutput {
    root: PathBuf,
}

impl VerifyOutput {
    /// Validates `path` and creates at most its final missing component.
    ///
    /// Writability is proven by writing and removing [`PROBE_FILE_NAME`],
    /// which is a declared [`OWNED_NAMES`] entry rather than an undeclared
    /// temporary: this type may only ever touch names it has published.
    pub fn prepare(path: &Path) -> Result<Self, VerifyOutputError> {
        if path.as_os_str().is_empty() {
            return Err(VerifyOutputError::Empty);
        }
        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(VerifyOutputError::ParentTraversal {
                path: path.to_path_buf(),
            });
        }

        let metadata = fs::symlink_metadata(path);
        match metadata {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(VerifyOutputError::SymbolicLink {
                    path: path.to_path_buf(),
                });
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(VerifyOutputError::NotADirectory {
                    path: path.to_path_buf(),
                });
            }
            Ok(_) => {}
            Err(_) => {
                let parent = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty());
                let Some(parent) = parent else {
                    return Err(VerifyOutputError::RefusedRoot {
                        path: path.to_path_buf(),
                    });
                };
                if !parent.is_dir() {
                    return Err(VerifyOutputError::MissingParent {
                        parent: parent.to_path_buf(),
                    });
                }
                fs::create_dir(path).map_err(|error| VerifyOutputError::Unwritable {
                    path: path.to_path_buf(),
                    reason: error.to_string(),
                })?;
            }
        }

        if path.parent().is_none() {
            return Err(VerifyOutputError::RefusedRoot {
                path: path.to_path_buf(),
            });
        }

        for name in ARTIFACT_NAMES {
            let artifact = path.join(name);
            match fs::symlink_metadata(&artifact) {
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => {
                    return Err(VerifyOutputError::UnsafeArtifact {
                        name: name.to_owned(),
                        path: artifact,
                    });
                }
                Err(_) => {}
            }
        }

        // The probe is the one file `prepare` creates, and it may only ever be
        // a file `prepare` created. Anything already sitting on the name is
        // refused outright rather than reused: a regular file is debris from a
        // crashed run and not ours to truncate, and a symbolic link is worse
        // than that — `fs::write` follows it, so a link aimed at any file this
        // process can write would be overwritten with the probe text and then
        // unlinked by the cleanup below, all under a name the caller never
        // supplied.
        let probe = path.join(PROBE_FILE_NAME);
        if fs::symlink_metadata(&probe).is_ok() {
            return Err(VerifyOutputError::StaleProbe { path: probe });
        }
        // `create_new` closes the gap between that check and this write rather
        // than trusting it: `O_CREAT | O_EXCL` fails on an existing entry and
        // fails on a symbolic link, so the handle below is always a regular
        // file this call has just made.
        let created = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe);
        let mut file = match created {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(VerifyOutputError::StaleProbe { path: probe });
            }
            Err(error) => {
                return Err(VerifyOutputError::Unwritable {
                    path: path.to_path_buf(),
                    reason: error.to_string(),
                });
            }
        };
        io::Write::write_all(&mut file, b"probe").map_err(|error| {
            VerifyOutputError::Unwritable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            }
        })?;
        drop(file);
        fs::remove_file(&probe).map_err(|error| VerifyOutputError::Unwritable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;

        Ok(Self {
            root: path.to_path_buf(),
        })
    }

    /// The validated directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path one owned artifact is written to.
    ///
    /// # Panics
    ///
    /// Panics when `name` is not one of [`ARTIFACT_NAMES`], because writing an
    /// unowned name would break the "only exact named files" contract.
    pub fn artifact(&self, name: &str) -> PathBuf {
        assert!(
            ARTIFACT_NAMES.contains(&name),
            "{name} is not a verification artifact"
        );
        self.root.join(name)
    }

    /// The path one frame is written to.
    pub fn frame(&self, frame: FrameName) -> PathBuf {
        self.artifact(frame.file_name())
    }

    /// The path the canonical report is written to.
    pub fn report(&self) -> PathBuf {
        self.artifact(REPORT_FILE_NAME)
    }

    /// Removes exactly the owned artifact names, and nothing else.
    ///
    /// This is the only removal this module performs. It never recurses, never
    /// touches an unowned name, and never removes the directory itself.
    pub fn clear(&self) -> io::Result<()> {
        for name in ARTIFACT_NAMES {
            let artifact = self.root.join(name);
            match fs::symlink_metadata(&artifact) {
                Ok(metadata) if metadata.is_file() => fs::remove_file(&artifact)?,
                Ok(_) => {
                    return Err(io::Error::other(format!(
                        "{} is not a regular file",
                        artifact.display()
                    )));
                }
                Err(_) => {}
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stage machine
// ---------------------------------------------------------------------------

/// One step of the documented verification journey.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VerificationStage {
    /// Configure the deterministic run before anything is loaded.
    Boot,
    /// Wait for every generated GLB to reach `AssetLoadState::Ready`.
    WaitForAssets,
    /// Re-validate the spawned hall blueprint and the bound rig.
    ValidateBlueprint,
    /// Capture the healthy hall at the spawn point.
    HealthyCapture,
    /// Run the real seeded scheduler until three tickets are open.
    SeedThreeFaults,
    /// Capture the three-ticket queue.
    FaultQueueCapture,
    /// Drive real arrow keys towards the highest-priority faulted rack.
    KeyboardJourney,
    /// Capture the walk pose while the arrow keys are still held.
    WalkCapture,
    /// Press the real `Space` key and require a started repair.
    BeginRepair,
    /// Capture the running repair.
    RepairCapture,
    /// Let the real repair timer finish.
    CompleteRepair,
    /// Capture the resolved indicator.
    ResolvedCapture,
    /// Press the real `E` key and settle on SouthEast.
    OrbitSouthEast,
    /// Capture the settled SouthEast heading.
    SettledSouthEastCapture,
    /// Press the real `E` key and settle on SouthWest.
    OrbitSouthWest,
    /// Capture the settled SouthWest heading.
    SettledSouthWestCapture,
    /// Press the real `E` key and settle on NorthWest.
    OrbitNorthWest,
    /// Capture the settled NorthWest heading.
    SettledNorthWestCapture,
    /// Press the real `E` key and capture the tween midpoint.
    MidOrbitCapture,
    /// Capture the technician in a room corner at each of the four headings.
    CornerProbes,
    /// Resize to 960x540 and capture the three-ticket layout.
    LowResolutionCapture,
    /// Every frame is on disk and every observation is collected.
    AnalyzeReady,
    /// Write the canonical semantic report.
    WriteReport,
    /// The run finished; the process exits successfully.
    Success,
    /// The run failed; the process exits with an error.
    Failure,
}

impl VerificationStage {
    /// Every stage, in declaration order, ending with the two terminals.
    pub const ALL: [Self; 25] = [
        Self::Boot,
        Self::WaitForAssets,
        Self::ValidateBlueprint,
        Self::HealthyCapture,
        Self::SeedThreeFaults,
        Self::FaultQueueCapture,
        Self::KeyboardJourney,
        Self::WalkCapture,
        Self::BeginRepair,
        Self::RepairCapture,
        Self::CompleteRepair,
        Self::ResolvedCapture,
        Self::OrbitSouthEast,
        Self::SettledSouthEastCapture,
        Self::OrbitSouthWest,
        Self::SettledSouthWestCapture,
        Self::OrbitNorthWest,
        Self::SettledNorthWestCapture,
        Self::MidOrbitCapture,
        Self::CornerProbes,
        Self::LowResolutionCapture,
        Self::AnalyzeReady,
        Self::WriteReport,
        Self::Success,
        Self::Failure,
    ];

    /// The only stage this one may advance to on its own.
    pub const fn next(self) -> Option<Self> {
        Some(match self {
            Self::Boot => Self::WaitForAssets,
            Self::WaitForAssets => Self::ValidateBlueprint,
            Self::ValidateBlueprint => Self::HealthyCapture,
            Self::HealthyCapture => Self::SeedThreeFaults,
            Self::SeedThreeFaults => Self::FaultQueueCapture,
            Self::FaultQueueCapture => Self::KeyboardJourney,
            Self::KeyboardJourney => Self::WalkCapture,
            Self::WalkCapture => Self::BeginRepair,
            Self::BeginRepair => Self::RepairCapture,
            Self::RepairCapture => Self::CompleteRepair,
            Self::CompleteRepair => Self::ResolvedCapture,
            Self::ResolvedCapture => Self::OrbitSouthEast,
            Self::OrbitSouthEast => Self::SettledSouthEastCapture,
            Self::SettledSouthEastCapture => Self::OrbitSouthWest,
            Self::OrbitSouthWest => Self::SettledSouthWestCapture,
            Self::SettledSouthWestCapture => Self::OrbitNorthWest,
            Self::OrbitNorthWest => Self::SettledNorthWestCapture,
            Self::SettledNorthWestCapture => Self::MidOrbitCapture,
            Self::MidOrbitCapture => Self::CornerProbes,
            Self::CornerProbes => Self::LowResolutionCapture,
            Self::LowResolutionCapture => Self::AnalyzeReady,
            Self::AnalyzeReady => Self::WriteReport,
            Self::WriteReport => Self::Success,
            Self::Success | Self::Failure => return None,
        })
    }

    /// Whether the run has finished, successfully or not.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Success | Self::Failure)
    }

    /// The stable name this stage is reported and logged under.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::WaitForAssets => "wait-for-assets",
            Self::ValidateBlueprint => "validate-blueprint",
            Self::HealthyCapture => "healthy-capture",
            Self::SeedThreeFaults => "seed-three-faults",
            Self::FaultQueueCapture => "fault-queue-capture",
            Self::KeyboardJourney => "keyboard-journey",
            Self::WalkCapture => "walk-capture",
            Self::BeginRepair => "begin-repair",
            Self::RepairCapture => "repair-capture",
            Self::CompleteRepair => "complete-repair",
            Self::ResolvedCapture => "resolved-capture",
            Self::OrbitSouthEast => "orbit-south-east",
            Self::SettledSouthEastCapture => "settled-south-east-capture",
            Self::OrbitSouthWest => "orbit-south-west",
            Self::SettledSouthWestCapture => "settled-south-west-capture",
            Self::OrbitNorthWest => "orbit-north-west",
            Self::SettledNorthWestCapture => "settled-north-west-capture",
            Self::MidOrbitCapture => "mid-orbit-capture",
            Self::CornerProbes => "corner-probes",
            Self::LowResolutionCapture => "low-resolution-capture",
            Self::AnalyzeReady => "analyze-ready",
            Self::WriteReport => "write-report",
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }

    /// The frames this stage captures, in capture order.
    pub fn frames(self) -> Vec<FrameName> {
        match self {
            Self::HealthyCapture => vec![FrameName::HealthyCenterNorthEast],
            Self::FaultQueueCapture => vec![FrameName::FaultQueueNorthEast],
            Self::WalkCapture => vec![FrameName::WalkNorthEast],
            Self::RepairCapture => vec![FrameName::RepairingNorthEast],
            Self::ResolvedCapture => vec![FrameName::ResolvedNorthEast],
            Self::SettledSouthEastCapture => vec![FrameName::SettledSouthEast],
            Self::SettledSouthWestCapture => vec![FrameName::SettledSouthWest],
            Self::SettledNorthWestCapture => vec![FrameName::SettledNorthWest],
            Self::MidOrbitCapture => vec![FrameName::MidOrbit],
            Self::CornerProbes => vec![
                FrameName::CornerNorthEast,
                FrameName::CornerSouthEast,
                FrameName::CornerSouthWest,
                FrameName::CornerNorthWest,
            ],
            Self::LowResolutionCapture => vec![FrameName::LowResolutionQueue],
            _ => Vec::new(),
        }
    }
}

/// Why a requested stage transition was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StageError {
    /// The target stage is not the current stage's successor and is not
    /// [`VerificationStage::Failure`].
    IllegalTransition {
        /// The stage the machine was in.
        from: VerificationStage,
        /// The stage that was requested.
        to: VerificationStage,
    },
    /// The machine has already finished; nothing may follow a terminal.
    AlreadyTerminal {
        /// The terminal stage the machine is in.
        stage: VerificationStage,
    },
}

impl fmt::Display for StageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IllegalTransition { from, to } => write!(
                formatter,
                "illegal verification transition {} -> {}",
                from.name(),
                to.name()
            ),
            Self::AlreadyTerminal { stage } => write!(
                formatter,
                "the verification run already finished at {}",
                stage.name()
            ),
        }
    }
}

impl std::error::Error for StageError {}

/// The live verification stage, the path it walked, and why it failed.
#[derive(Clone, Debug, PartialEq)]
pub struct StageMachine {
    stage: VerificationStage,
    visited: Vec<VerificationStage>,
    failure: Option<(VerificationStage, String)>,
}

impl Default for StageMachine {
    fn default() -> Self {
        Self::at(VerificationStage::Boot)
    }
}

impl StageMachine {
    /// A machine parked at `stage`, having visited only that stage.
    pub fn at(stage: VerificationStage) -> Self {
        Self {
            stage,
            visited: vec![stage],
            failure: None,
        }
    }

    /// The current stage.
    pub fn stage(&self) -> VerificationStage {
        self.stage
    }

    /// Every stage the machine entered, in order.
    pub fn visited(&self) -> &[VerificationStage] {
        &self.visited
    }

    /// The stage that failed and the first recorded cause.
    pub fn failure(&self) -> Option<(&'static str, &str)> {
        self.failure
            .as_ref()
            .map(|(stage, reason)| (stage.name(), reason.as_str()))
    }

    /// Whether the machine has finished.
    pub fn is_terminal(&self) -> bool {
        self.stage.is_terminal()
    }

    /// Moves to `to`, refusing every transition the documented machine forbids.
    pub fn transition(&mut self, to: VerificationStage) -> Result<(), StageError> {
        if self.stage.is_terminal() {
            return Err(StageError::AlreadyTerminal { stage: self.stage });
        }
        if to == VerificationStage::Failure {
            self.enter(to);
            return Ok(());
        }
        if self.stage.next() != Some(to) {
            return Err(StageError::IllegalTransition {
                from: self.stage,
                to,
            });
        }
        self.enter(to);
        Ok(())
    }

    /// Moves to the current stage's successor.
    pub fn advance(&mut self) -> Result<VerificationStage, StageError> {
        let Some(next) = self.stage.next() else {
            return Err(StageError::AlreadyTerminal { stage: self.stage });
        };
        self.transition(next)?;
        Ok(next)
    }

    /// Fails the run, keeping the first cause and the stage it happened in.
    ///
    /// A second failure never overwrites the first: the first cause is the real
    /// one, and everything after it is a consequence.
    pub fn fail(&mut self, reason: impl Into<String>) {
        if self.failure.is_some() {
            return;
        }
        self.failure = Some((self.stage, reason.into()));
        self.enter(VerificationStage::Failure);
    }

    fn enter(&mut self, stage: VerificationStage) {
        self.stage = stage;
        self.visited.push(stage);
    }
}

// ---------------------------------------------------------------------------
// Deterministic run configuration
// ---------------------------------------------------------------------------

/// The fixed simulation step every verification frame advances by.
pub const FIXED_STEP_SECONDS: f64 = 1.0 / 60.0;

/// The magenta clear colour. Nothing in the authored palette is near it, so any
/// pixel of it on screen means the camera rendered past the 72 m apron.
pub const SENTINEL_CLEAR: Srgba = Srgba::rgb(1.0, 0.0, 1.0);

/// How long one screenshot readback may take before it is declared lost.
///
/// This is a wall clock, not a frame count, because that is what the readback
/// really is: a GPU handing a buffer back on its own schedule. A frame budget
/// measures how fast the harness can spin, which on an unthrottled software
/// renderer says nothing at all about whether the callback is late or lost.
/// The budget is generous — a cold software rasterizer is slow — and it is the
/// *only* budget a readback is charged against: [`APP_WATCHDOG`] excludes
/// capture waiting entirely, so a genuinely lost callback is always named as a
/// lost callback naming its own frame rather than swallowed by a global
/// watchdog that would only ever say "stuck in some stage".
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);

/// Readback allowance for the capture that follows a DX12 swapchain resize.
pub const LOW_RESOLUTION_CAPTURE_TIMEOUT: Duration = Duration::from_secs(150);

/// Delay between zero-time render pumps while a screenshot is outstanding.
///
/// Without backpressure the app thread can queue hundreds of frames ahead of
/// a software render thread, burying the requested readback behind work that
/// exists only because the callback has not arrived yet.
pub const CAPTURE_PUMP_INTERVAL: Duration = Duration::from_millis(250);

/// How many frames a window resize is allowed, and always costs.
pub const RESIZE_FRAMES: u64 = 45;

/// How many frames a corner probe lets the camera settle after placing the
/// technician, and always costs.
pub const PROBE_SETTLE_FRAMES: u64 = 6;

/// How much *active, non-capture* wall time the app gives itself before it
/// fails with the stage it is stuck in.
///
/// The watchdog exists to name a state machine that stopped moving. It is not
/// a budget for the run as a whole, and deliberately so: waiting on an
/// asynchronous readback is not the state machine failing to move, it is the
/// state machine doing exactly what it is supposed to do, and that wait is
/// already governed second by second by [`CAPTURE_TIMEOUT`]. Charging it here
/// as well made a merely slow renderer indistinguishable from a stuck one —
/// which is precisely how a healthy CI run died in `keyboard-journey` with two
/// frames already on disk. So while a capture is outstanding the watchdog
/// clock is not running, and when that capture resolves its whole wall
/// duration is excluded for good.
///
/// A lost callback still fails the run: it fails through [`CAPTURE_TIMEOUT`],
/// naming the frame, the stage, and the artifact, which is strictly better
/// evidence than a watchdog expiry could ever be.
pub const APP_WATCHDOG: Duration = Duration::from_secs(300);

/// What the parent allows the child on top of its own budgets: process start,
/// asset load, window creation, report write, and shutdown.
pub const LAUNCH_MARGIN: Duration = Duration::from_secs(25);

/// The absolute wall clock a parent gives one child before it kills that exact
/// process.
///
/// This is derived, not chosen. The child polices itself with two budgets that
/// are deliberately independent — [`APP_WATCHDOG`] over active work and
/// [`CAPTURE_TIMEOUT`] over each of the [`FrameName::ALL`] readbacks — so the
/// longest a *correct* child can legitimately live is the sum of both plus
/// [`LAUNCH_MARGIN`] for the work that belongs to neither. The parent cap has
/// to sit above that sum or it would kill runs the child was entitled to
/// finish; tying it to the equation rather than to a number means adding a
/// fifteenth frame moves it automatically.
///
/// With today's budgets: 300 s + 13 x 30 s + 150 s + 25 s = 865 s.
pub const PARENT_WATCHDOG: Duration = Duration::from_secs(
    APP_WATCHDOG.as_secs()
        + CAPTURE_TIMEOUT.as_secs() * (FrameName::ALL.len() as u64 - 1)
        + LOW_RESOLUTION_CAPTURE_TIMEOUT.as_secs()
        + LAUNCH_MARGIN.as_secs(),
);

/// The short active-work watchdog [`VerificationFault::Stall`] runs with.
///
/// This is a test instrument and nothing else. The stall fixture proves the
/// inactivity timeout *fires and names its stage*; that proof is identical at
/// any budget, and running it at the production [`APP_WATCHDOG`] would add
/// three quarters of a minute to every gate to re-measure a constant. It is
/// generous enough that booting the real game, loading its assets, and taking
/// the healthy capture can never be mistaken for the stall it is testing.
pub const STALL_WATCHDOG: Duration = Duration::from_secs(20);

/// The short readback budget [`VerificationFault::DropCapture`] runs with.
///
/// Another test instrument. The drop fixture refuses to record any readback,
/// so the run can only end by waiting the budget out; the failure it proves —
/// a lost callback named with its frame, stage, and artifact state — is
/// identical at any budget, and the production [`CAPTURE_TIMEOUT`] would spend
/// ten seconds of every gate run idling to re-measure a constant. It is still
/// far longer than a healthy readback of the first frame takes, so a real
/// callback can never be mistaken for the loss it is testing.
pub const DROP_CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);

/// How many frames one non-capture stage may take before it is declared stuck.
pub const STAGE_FRAME_BUDGET: u64 = 4_000;

/// Half extents of the box the technician is projected through, in metres.
const WORKER_HALF_EXTENTS: Vec3 = Vec3::new(0.75, 1.15, 0.75);

/// Height of the centre of that box above the floor, in metres.
const WORKER_CENTER_HEIGHT: f32 = 1.05;

/// Margin added around the projected worker box, in pixels.
const WORKER_CROP_MARGIN: f32 = 6.0;

/// How far into the room corner the corner probes place the technician.
fn corner_inset() -> f32 {
    ROOM_SIZE.x * 0.5 - PLAYER_RADIUS
}

/// The room corner each heading is probed at.
fn probe_corner(heading: CameraHeading) -> Vec2 {
    let inset = corner_inset();
    match heading {
        CameraHeading::NorthEast => Vec2::new(inset, -inset),
        CameraHeading::SouthEast => Vec2::new(inset, inset),
        CameraHeading::SouthWest => Vec2::new(-inset, inset),
        CameraHeading::NorthWest => Vec2::new(-inset, -inset),
    }
}

/// How close the scripted walk has to get before it stops pressing a key.
///
/// One simulated frame moves the technician `PLAYER_SPEED / 60` metres, so a
/// tolerance below that would oscillate around the target forever.
const ARRIVAL_TOLERANCE: f32 = 0.08;

/// The z the walk capture is taken at, part way up the aisle.
const WALK_CAPTURE_Z: f32 = -6.0;

/// The rack the scripted journey walks to and repairs.
///
/// The seeded stream opens rack 2, rack 1, then rack 3. Rack 1 is the one whose
/// repair lets the stream continue: the fourth seeded candidate is rack 1
/// again, so repairing it is what returns the queue to three tickets in time
/// for the low-resolution capture. Repairing any other rack would leave the
/// scheduler holding a duplicate candidate forever.
pub const JOURNEY_RACK: usize = 1;

/// Where the technician stands to repair [`JOURNEY_RACK`], in metres.
fn journey_repair_spot(center: Vec2, half_extents: Vec2) -> Vec2 {
    Vec2::new(center.x - half_extents.x - PLAYER_RADIUS - 0.2, 0.0)
}

// ---------------------------------------------------------------------------
// Equipment identity
// ---------------------------------------------------------------------------

/// One family of authored equipment the rendered hall has to actually show.
///
/// The whole-frame contracts are global histograms: floor, rack, ink, yellow,
/// and edge mass measured over every pixel. A 72 m inked floor grid, a hazard
/// striped apron, and four white perimeter walls can satisfy all of them on
/// their own, so a frame in which every rack, cooling unit, tray, hose, and
/// cart failed to spawn could still pass. These categories close that hole:
/// each one is measured *inside the screen rectangle its own authored geometry
/// projects into*, so the evidence cannot be borrowed from the room around it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EquipmentCategory {
    /// The four server rack rows.
    RackRows,
    /// The four wall-side cooling units.
    CoolingUnits,
    /// The overhead cable trays and the hose drops hung under them.
    OverheadRouting,
    /// The red service cart.
    UtilityCart,
    /// The yellow step stool and the painted floor markings.
    FloorFurniture,
}

impl EquipmentCategory {
    /// Every category, in stable order.
    pub const ALL: [Self; 5] = [
        Self::RackRows,
        Self::CoolingUnits,
        Self::OverheadRouting,
        Self::UtilityCart,
        Self::FloorFurniture,
    ];

    /// The stable name this category is reported and measured under.
    pub const fn name(self) -> &'static str {
        match self {
            Self::RackRows => "rack-rows",
            Self::CoolingUnits => "cooling-units",
            Self::OverheadRouting => "overhead-routing",
            Self::UtilityCart => "utility-cart",
            Self::FloorFurniture => "floor-furniture",
        }
    }

    /// The category one authored [`AssetKind`] belongs to.
    ///
    /// The apron, floor, floor grid, and walls are deliberately absent: they
    /// are the global surfaces these contracts exist to stop standing in for
    /// equipment.
    pub const fn of(kind: AssetKind) -> Option<Self> {
        match kind {
            AssetKind::RackRow => Some(Self::RackRows),
            AssetKind::CoolingUnit => Some(Self::CoolingUnits),
            AssetKind::OverheadTray | AssetKind::HoseDrop => Some(Self::OverheadRouting),
            AssetKind::UtilityCart => Some(Self::UtilityCart),
            AssetKind::StepStool | AssetKind::FloorMarking => Some(Self::FloorFurniture),
            AssetKind::RenderApron | AssetKind::Floor | AssetKind::FloorGrid | AssetKind::Wall => {
                None
            }
        }
    }

    /// The palette groups a region has to carry before it counts as raster
    /// evidence that this category was really drawn.
    ///
    /// Every group is authored into the module itself. No group can be
    /// satisfied by the floor, which is [`PaletteRole::FloorLight`] and
    /// [`PaletteRole::FloorShadow`] — and no category is allowed to qualify on
    /// [`PaletteRole::Ink`] alone, because the raised floor grid inks a panel
    /// seam grid across the whole rendered coverage and would otherwise hand
    /// every category a free pass.
    ///
    /// The cooling unit's [`PaletteRole::TealAccent`] is its fan hub and
    /// spokes: a 0.8 m detail on one face of a 2.1 m by 4.1 m unit, which the
    /// measured frames put between 0.03 % and 0.4 % of the unit's own
    /// rectangle depending on which way that face is turned. It is therefore
    /// grouped with the unit's inked outlines rather than given a threshold of
    /// its own, which no honest number could carry margin at.
    pub const fn role_groups(self) -> &'static [&'static [PaletteRole]] {
        match self {
            Self::RackRows | Self::CoolingUnits => &[
                &[PaletteRole::RackWhite, PaletteRole::RackShadow],
                &[PaletteRole::TealAccent, PaletteRole::Ink],
            ],
            Self::OverheadRouting => &[&[PaletteRole::HoseCharcoal, PaletteRole::Ink]],
            Self::UtilityCart => &[&[PaletteRole::FaultRed], &[PaletteRole::Ink]],
            Self::FloorFurniture => &[&[PaletteRole::SignatureYellow], &[PaletteRole::Ink]],
        }
    }

    /// Whether every on-screen prop of this category has to carry its own
    /// evidence, rather than the category being satisfied by any one member.
    ///
    /// Only the rack rows are named individually by the contract: four rows is
    /// the authored hall, and one row disappearing is exactly the regression a
    /// category-wide check would hide behind the other three.
    pub const fn requires_every_prop(self) -> bool {
        matches!(self, Self::RackRows)
    }
}

/// Longest world-space span, in metres, of one projected equipment segment.
///
/// A rack row is 16 m long and a floor marking 24 m. Projecting the whole prop
/// as one box would produce a screen rectangle that is mostly the floor either
/// side of a thin diagonal, so the measured shares would say more about the
/// floor than about the equipment. Splitting the real 3D bounds along their
/// longest horizontal axis first keeps every measured rectangle tight around
/// the geometry that produced it.
pub const EQUIPMENT_SEGMENT_METRES: f32 = 2.5;

/// Most segments one prop's bounds are ever split into.
pub const EQUIPMENT_MAX_SEGMENTS: usize = 16;

/// Smallest on-screen area, in pixels, a projected segment must keep before it
/// is measured at all. Anything smaller is a sliver at the frame edge with too
/// few pixels to carry a stable ratio.
pub const EQUIPMENT_REGION_MIN_PIXELS: u64 = 900;

// ---------------------------------------------------------------------------
// Canonical report
// ---------------------------------------------------------------------------

/// Rounds one float to the canonical 1e-6 grid so two semantically identical
/// runs serialize identically. Negative zero is normalized away.
pub fn canonical_float(value: f32) -> f64 {
    canonical_f64(f64::from(value))
}

/// Rounds one double to the canonical 1e-6 grid.
pub fn canonical_f64(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    let rounded = (value * 1.0e6).round() / 1.0e6;
    if rounded == 0.0 { 0.0 } else { rounded }
}

/// A laid-out rectangle, in logical pixels.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RectFacts {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl PixelRect {
    /// Snaps a reported logical rectangle onto the pixel grid of one frame.
    ///
    /// Lives here rather than beside the rest of `PixelRect` because it is the
    /// one operation that needs a reported rectangle, and the measurement
    /// engine must not depend on the report schema.
    pub fn snap(rect: RectFacts, width: u32, height: u32) -> Self {
        let left = rect.x.floor().clamp(0.0, f64::from(width)) as u32;
        let top = rect.y.floor().clamp(0.0, f64::from(height)) as u32;
        let right = (rect.x + rect.width).ceil().clamp(0.0, f64::from(width)) as u32;
        let bottom = (rect.y + rect.height).ceil().clamp(0.0, f64::from(height)) as u32;
        Self {
            x: left,
            y: top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        }
    }
}

impl RectFacts {
    /// A rectangle from a centre and half extents.
    pub fn from_center(center: Vec2, half: Vec2) -> Self {
        Self {
            x: canonical_float(center.x - half.x),
            y: canonical_float(center.y - half.y),
            width: canonical_float(half.x * 2.0),
            height: canonical_float(half.y * 2.0),
        }
    }

    /// Whether the rectangle lies entirely inside a viewport of `size`.
    pub fn is_inside(&self, size: (u32, u32)) -> bool {
        self.x >= 0.0
            && self.y >= 0.0
            && self.x + self.width <= f64::from(size.0)
            && self.y + self.height <= f64::from(size.1)
    }
}

/// One queue row exactly as the HUD drew it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HudRowFacts {
    /// Slot index, top to bottom.
    pub slot: usize,
    /// The ticket the row shows.
    pub ticket: u64,
    /// The rack that ticket belongs to.
    pub rack: usize,
    /// The severity label.
    pub severity: String,
    /// The rack state label.
    pub state: String,
    /// Dwell progress, rounded to the canonical grid.
    pub progress: f64,
    /// The rendered short label.
    pub label: String,
}

/// One diegetic badge exactly as the HUD placed it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BadgeFacts {
    /// Stable rack index.
    pub rack: usize,
    /// The badge kind, or `none`.
    pub kind: String,
    /// Why the badge is or is not drawn.
    pub visibility: String,
    /// The badge rectangle, when it is drawn.
    pub rect: Option<RectFacts>,
}

/// One active ticket at capture time.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TicketFacts {
    /// Stable ticket identifier.
    pub id: u64,
    /// The rack the ticket belongs to.
    pub rack: usize,
    /// The severity.
    pub severity: String,
    /// The simulation tick the ticket was created on.
    pub created_tick: u64,
}

/// One authored equipment prop, projected onto one captured frame.
///
/// Every number here comes from the real spawned meshes and the real camera:
/// the world bounds are the union of the prop's own [`Aabb`]s and those of
/// every descendant the generated scene spawned, and the rectangles are those
/// bounds pushed through [`Camera::world_to_viewport`].
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentFacts {
    /// The authored prop identifier.
    pub id: String,
    /// The [`EquipmentCategory`] this prop belongs to.
    pub category: String,
    /// World-space bounds of the spawned meshes: `[min, max]`, in metres.
    pub world_bounds: [[f64; 3]; 2],
    /// Unclipped projected bounds: `[min_x, min_y, max_x, max_y]`, in pixels.
    pub projected_bounds: [f64; 4],
    /// Whether those projected bounds intersect the viewport at all. A prop
    /// that is false here is the only kind the contracts may skip.
    pub on_screen: bool,
    /// The measurable projected segments, clipped to the viewport, in segment
    /// order. Empty when nothing survived [`EQUIPMENT_REGION_MIN_PIXELS`].
    pub regions: Vec<RectFacts>,
}

/// The render settings the one game camera actually carried.
///
/// Recorded from the live camera entity so the contract that verification only
/// changes multisampling is checked against the running product rather than
/// against the source that configured it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CameraRenderFacts {
    /// The display transform, as `{:?}` of [`Tonemapping`].
    pub tonemapping: String,
    /// The deband dither, as `{:?}` of [`DebandDither`].
    pub deband_dither: String,
    /// Multisample count; `1` is [`Msaa::Off`].
    pub msaa_samples: u32,
    /// The clear colour, as `#RRGGBB`.
    pub clear_color: String,
}

/// One observed ticket lifecycle event.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TicketEventFacts {
    /// `opened` or `removed`.
    pub event: String,
    /// Stable ticket identifier.
    pub ticket: u64,
    /// The rack the ticket belongs to.
    pub rack: usize,
    /// The severity.
    pub severity: String,
    /// The simulation tick the event happened on.
    pub tick: u64,
}

/// One real `Space` press and exactly what the game did with it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionFacts {
    /// The stage the press was made in.
    pub stage: String,
    /// The documented outcome.
    pub outcome: String,
    /// The ticket the outcome named, if any.
    pub ticket: Option<u64>,
    /// The rack the outcome named, if any.
    pub rack: Option<usize>,
    /// The measured distance the outcome carried, if any.
    pub distance: Option<f64>,
}

/// One real key message the harness injected.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeyFacts {
    /// The stage the message was written in.
    pub stage: String,
    /// The key.
    pub key: String,
    /// `pressed` or `released`.
    pub state: String,
}

/// Everything one captured frame recorded about the live game.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrameFacts {
    /// The file, relative to the output directory.
    pub path: String,
    /// Required width.
    pub width: u32,
    /// Required height.
    pub height: u32,
    /// The stage that captured it.
    pub stage: String,
    /// The settled heading the camera was turning towards.
    pub heading: String,
    /// The interpolated camera yaw, in degrees.
    pub camera_yaw_degrees: f64,
    /// Whether the orbit had settled.
    pub camera_settled: bool,
    /// Eased orbit progress.
    pub camera_progress: f64,
    /// The clamped ground point the camera followed.
    pub camera_target: [f64; 2],
    /// The ground quadrilateral the viewport casts onto `Y = 0`.
    pub ground_quadrilateral: [[f64; 2]; 4],
    /// The technician's ground position.
    pub player_position: [f64; 2],
    /// The clip the technician was playing.
    pub player_clip: String,
    /// Whether a running repair held the controls.
    pub movement_locked: bool,
    /// The projected technician crop, in pixels.
    pub worker_crop: RectFacts,
    /// The status line the HUD showed.
    pub hud_status: String,
    /// The queue rows the HUD drew, in priority order.
    pub hud_rows: Vec<HudRowFacts>,
    /// The laid-out HUD panel rectangles, by stable name.
    pub hud_panels: BTreeMap<String, RectFacts>,
    /// Every rack badge, in stable rack order.
    pub badges: Vec<BadgeFacts>,
    /// Every active ticket, in priority order.
    pub tickets: Vec<TicketFacts>,
    /// Every rack's state, in stable rack order.
    pub rack_states: Vec<String>,
    /// Every authored equipment prop, projected onto this frame, sorted by
    /// prop identifier.
    pub equipment: Vec<EquipmentFacts>,
}

/// The authored hall the run actually validated.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BlueprintFacts {
    /// Walkable room size, in metres.
    pub room: [f64; 2],
    /// Rendered coverage size, in metres.
    pub coverage: [f64; 2],
    /// Authored visual count.
    pub visuals: usize,
    /// Authored collider count.
    pub colliders: usize,
    /// Authored rack rows.
    pub rack_rows: usize,
    /// Authored aisles.
    pub aisles: usize,
    /// The player spawn point.
    pub player_spawn: [f64; 2],
    /// Whether every aisle checkpoint shares one walkable component.
    pub walkable_connected: bool,
    /// Blueprint validation errors, which must be empty.
    pub validation_errors: Vec<String>,
}

/// The deterministic simulation the run drove.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GameplayFacts {
    /// The fault seed, in hexadecimal.
    pub fault_seed: String,
    /// The fixed simulation step, in seconds.
    pub fixed_step_seconds: f64,
    /// Every ticket lifecycle event, in order.
    pub ticket_history: Vec<TicketEventFacts>,
    /// Every real `Space` press, in order.
    pub interactions: Vec<InteractionFacts>,
    /// Every real key message, in order.
    pub keys: Vec<KeyFacts>,
    /// How many tickets the scheduler emitted.
    pub tickets_emitted: u64,
    /// How many times a matured opportunity paused at capacity.
    pub capacity_pauses: u64,
    /// How many times a drawn candidate paused on a duplicate rack.
    pub duplicate_pauses: u64,
    /// How many times a drawn candidate paused on a busy rack.
    pub busy_pauses: u64,
    /// The rig nodes the technician bound, sorted.
    pub rig_parts: Vec<String>,
    /// The rack the journey repaired.
    pub repaired_rack: usize,
}

/// The canonical semantic report.
///
/// Every map is sorted, every float is rounded to the canonical 1e-6 grid,
/// every path is relative, and no field carries a wall clock, a host name, an
/// environment value, or an absolute path.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReport {
    /// Report schema version.
    pub schema_version: u32,
    /// `success` or `failure`.
    pub result: String,
    /// The stage a failed run failed in.
    pub failed_stage: Option<String>,
    /// The first recorded cause of a failed run.
    pub failure_reason: Option<String>,
    /// Every stage the run entered, in order.
    pub stages: Vec<String>,
    /// SHA-256 of every generated asset, by repository-relative path.
    pub assets: BTreeMap<String, String>,
    /// SHA-256 of every declarative asset source, by repository-relative path.
    pub asset_sources: BTreeMap<String, String>,
    /// SHA-256 of every approved reference, by repository-relative path.
    pub references: BTreeMap<String, String>,
    /// SHA-256 of every verification source file, by repository-relative path.
    pub sources: BTreeMap<String, String>,
    /// The authored hall.
    pub blueprint: BlueprintFacts,
    /// The render settings the one game camera carried.
    pub camera: CameraRenderFacts,
    /// The deterministic simulation.
    pub gameplay: GameplayFacts,
    /// Every captured frame, by frame file name.
    pub frames: BTreeMap<String, FrameFacts>,
}

/// Serializes one report to its canonical form: sorted keys, two-space indent,
/// and a trailing newline.
pub fn canonical_json(report: &VerificationReport) -> String {
    let mut text = serde_json::to_string_pretty(report)
        .expect("the verification report is plain serializable data");
    text.push('\n');
    text
}

/// The SHA-256 of one canonical document.
pub fn semantic_hash(canonical: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(canonical.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// Live run state
// ---------------------------------------------------------------------------

/// Whether a capture is outstanding, and which frame it belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PendingCapture {
    frame: FrameName,
    /// The simulated frame the request was made on, for diagnostics.
    requested_on: u64,
    /// When the request was made, which is what the readback budget measures.
    requested_at: Instant,
    /// How many zero-time render pumps this capture has already cost.
    pumps: u64,
    /// The pump the observer's record was first seen on.
    landed_on: Option<u64>,
    completed: bool,
    /// Whether this capture's wait has already been folded into
    /// [`VerificationRun::capture_excluded`].
    ///
    /// A wait is excluded from the active-work watchdog twice over: while the
    /// capture is outstanding the *current* wait is subtracted live, and the
    /// moment the capture resolves — landed, timed out, or rejected — that
    /// same wait is banked once and this flag stops it being counted both
    /// ways or banked twice.
    charged: bool,
}

/// The screenshot observers' mailbox. The observer runs
/// [`save_to_disk`](bevy::render::view::screenshot::save_to_disk) first and
/// only then records the frame, so a recorded frame is always already on disk.
#[derive(Resource, Clone, Debug, Default)]
struct CaptureInbox {
    completed: Vec<FrameName>,
}

/// Everything the run observed, accumulated frame by frame.
#[derive(Clone, Debug, Default)]
struct Observations {
    ticket_history: Vec<TicketEventFacts>,
    interactions: Vec<InteractionFacts>,
    keys: Vec<KeyFacts>,
    frames: BTreeMap<String, FrameFacts>,
    active: Vec<TicketFacts>,
    blueprint: Option<BlueprintFacts>,
    rig_parts: Vec<String>,
    presses_seen: u64,
}

/// The live verification run.
#[derive(Resource)]
pub struct VerificationRun {
    machine: StageMachine,
    output: VerifyOutput,
    started: Instant,
    watchdog: Duration,
    /// Wall time already spent waiting on captures that have since resolved.
    ///
    /// This is what makes [`APP_WATCHDOG`] a measure of active work rather
    /// than of the run's whole lifetime.
    capture_excluded: Duration,
    capture_timeout_override: Option<Duration>,
    capture_delay: u64,
    frame: u64,
    stage_frame: u64,
    held: BTreeSet<KeyCode>,
    release_next: Vec<KeyCode>,
    pending: Option<PendingCapture>,
    /// The facts measured for the outstanding capture, held back until that
    /// readback lands at the contracted size.
    staged_facts: Option<(FrameName, FrameFacts)>,
    capture_index: usize,
    probe_index: usize,
    resize_frame: Option<u64>,
    placed_frame: Option<u64>,
    observations: Observations,
    finished: bool,
    fault: Option<VerificationFault>,
}

impl VerificationRun {
    /// A run that writes into `output` and gives itself [`APP_WATCHDOG`] of
    /// active, non-capture work.
    pub fn new(output: VerifyOutput, fault: Option<VerificationFault>) -> Self {
        Self {
            machine: StageMachine::default(),
            output,
            fault,
            started: Instant::now(),
            watchdog: APP_WATCHDOG,
            capture_excluded: Duration::ZERO,
            capture_timeout_override: None,
            capture_delay: 0,
            frame: 0,
            stage_frame: 0,
            held: BTreeSet::new(),
            release_next: Vec::new(),
            pending: None,
            staged_facts: None,
            capture_index: 0,
            probe_index: 0,
            resize_frame: None,
            placed_frame: None,
            observations: Observations::default(),
            finished: false,
        }
    }

    /// The same run, holding every readback open for `pumps` further render
    /// pumps after the observer has already recorded it.
    ///
    /// This is the injected slow GPU. It changes only how many zero-time pumps
    /// a capture costs, which is exactly the quantity the canonical report is
    /// required to be independent of, so two runs that differ only in this
    /// value must produce byte-identical evidence.
    #[must_use]
    pub const fn with_capture_delay(mut self, pumps: u64) -> Self {
        self.capture_delay = pumps;
        self
    }

    /// The current stage.
    pub fn stage(&self) -> VerificationStage {
        self.machine.stage()
    }

    /// The same run with a different active-work watchdog.
    ///
    /// This is a test override and production never calls it: the only caller
    /// is the injected [`VerificationFault::Stall`], which proves the
    /// inactivity timeout in seconds instead of re-measuring
    /// [`APP_WATCHDOG`] in full.
    #[must_use]
    pub const fn with_watchdog(mut self, watchdog: Duration) -> Self {
        self.watchdog = watchdog;
        self
    }

    /// The same run with a different readback budget.
    ///
    /// This is a test override and production never calls it: the only caller
    /// is the injected [`VerificationFault::DropCapture`], which proves the
    /// lost-callback failure in seconds instead of re-measuring
    /// [`CAPTURE_TIMEOUT`] in full.
    #[must_use]
    pub const fn with_capture_timeout(mut self, timeout: Duration) -> Self {
        self.capture_timeout_override = Some(timeout);
        self
    }

    fn capture_timeout_for(&self, frame: FrameName) -> Duration {
        if let Some(timeout) = self.capture_timeout_override {
            return timeout;
        }
        match frame {
            FrameName::LowResolutionQueue => LOW_RESOLUTION_CAPTURE_TIMEOUT,
            _ => CAPTURE_TIMEOUT,
        }
    }

    /// Wall time this run has spent doing anything other than waiting for a
    /// screenshot readback.
    ///
    /// This is the quantity [`APP_WATCHDOG`] is measured against. Resolved
    /// captures have already been banked into `capture_excluded`; a capture
    /// that is outstanding right now has its wait subtracted live, so a
    /// readback in progress can never push the run over the line while it is
    /// still inside its own [`CAPTURE_TIMEOUT`].
    fn active_elapsed(&self) -> Duration {
        let outstanding = self
            .pending
            .filter(|pending| !pending.charged)
            .map_or(Duration::ZERO, |pending| pending.requested_at.elapsed());
        self.started
            .elapsed()
            .saturating_sub(self.capture_excluded)
            .saturating_sub(outstanding)
    }
}

/// A read-only view of everything the driver needs from the live game.
struct Snapshot {
    assets: AssetLoadState,
    hall: HallState,
    rig: PlayerRigState,
    rig_healthy: bool,
    parts: Vec<String>,
    roster: RackRoster,
    queue: TicketQueue,
    tick: u64,
    orbit: CameraOrbit,
    lock: MovementLock,
    last: LastInteraction,
    hud: HudReport,
    player: Vec2,
    clip: PlayerClip,
    rack_states: Vec<RackState>,
    scheduler: (u64, u64, u64, u64),
    viewport: Option<UVec2>,
}

fn snapshot(world: &mut World) -> Snapshot {
    let assets = world
        .get_resource::<State<AssetLoadState>>()
        .map_or(AssetLoadState::Loading, |state| *state.get());
    let hall = world
        .get_resource::<State<HallState>>()
        .map_or(HallState::Unbuilt, |state| *state.get());
    let rig = world
        .get_resource::<State<PlayerRigState>>()
        .map_or(PlayerRigState::Pending, |state| *state.get());
    let rig_healthy = world
        .get_resource::<PlayerRigReport>()
        .is_some_and(PlayerRigReport::is_healthy)
        && world.get_resource::<PlayerParts>().is_some()
        && world.get_resource::<PlayerAnimations>().is_some();
    let mut parts = world
        .get_resource::<PlayerParts>()
        .map(|parts| {
            parts
                .all()
                .iter()
                .map(|part| part.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    parts.sort();
    let roster = world
        .get_resource::<RackRoster>()
        .cloned()
        .unwrap_or_default();
    let queue = world
        .get_resource::<TicketQueue>()
        .cloned()
        .unwrap_or_default();
    let tick = world
        .get_resource::<OperationsClock>()
        .map_or(0, OperationsClock::tick);
    let orbit = world
        .get_resource::<CameraOrbit>()
        .copied()
        .unwrap_or_default();
    let lock = world
        .get_resource::<MovementLock>()
        .copied()
        .unwrap_or_default();
    let last = world
        .get_resource::<LastInteraction>()
        .copied()
        .unwrap_or_default();
    let hud = world
        .get_resource::<HudReport>()
        .cloned()
        .unwrap_or_default();
    let clip = world
        .get_resource::<PlayerAnimationState>()
        .map_or(PlayerClip::Idle, PlayerAnimationState::current);

    let player = world
        .query_filtered::<&Transform, With<Technician>>()
        .iter(world)
        .next()
        .map(|transform| Vec2::new(transform.translation.x, transform.translation.z))
        .unwrap_or(Vec2::ZERO);

    let mut rack_states = vec![RackState::Healthy; roster.len()];
    for entry in roster.all() {
        if let Some(operations) = world.get::<RackOperations>(entry.entity) {
            rack_states[entry.rack] = operations.state();
        }
    }

    let scheduler = world
        .get_resource::<FaultScheduler>()
        .map(|scheduler| {
            (
                scheduler.emitted(),
                scheduler.capacity_pauses(),
                scheduler.duplicate_pauses(),
                scheduler.busy_pauses(),
            )
        })
        .unwrap_or_default();

    let viewport = world
        .query_filtered::<&Camera, With<CellShiftCamera>>()
        .iter(world)
        .next()
        .and_then(Camera::physical_target_size);

    Snapshot {
        assets,
        hall,
        rig,
        rig_healthy,
        parts,
        roster,
        queue,
        tick,
        orbit,
        lock,
        last,
        hud,
        player,
        clip,
        rack_states,
        scheduler,
        viewport,
    }
}

fn clip_name(clip: PlayerClip) -> &'static str {
    match clip {
        PlayerClip::Idle => "idle",
        PlayerClip::Walk => "walk",
        PlayerClip::Repair => "repair",
    }
}

fn heading_name(heading: CameraHeading) -> &'static str {
    match heading {
        CameraHeading::NorthEast => "north-east",
        CameraHeading::SouthEast => "south-east",
        CameraHeading::SouthWest => "south-west",
        CameraHeading::NorthWest => "north-west",
    }
}

fn rack_state_name(state: RackState) -> &'static str {
    match state {
        RackState::Healthy => "healthy",
        RackState::Faulted => "faulted",
        RackState::Repairing => "repairing",
        RackState::Resolved => "resolved",
        RackState::Cooldown => "cooldown",
    }
}

fn severity_name(severity: TicketSeverity) -> &'static str {
    match severity {
        TicketSeverity::Critical => "critical",
        TicketSeverity::Warning => "warning",
    }
}

fn status_name(status: HudStatus) -> &'static str {
    match status {
        HudStatus::AllHealthy => "all-healthy",
        HudStatus::TicketsOpen => "tickets-open",
        HudStatus::Repairing => "repairing",
        HudStatus::MoveCloser => "move-closer",
        HudStatus::NoOpenTickets => "no-open-tickets",
    }
}

fn badge_kind_name(kind: Option<BadgeKind>) -> &'static str {
    match kind {
        None => "none",
        Some(BadgeKind::Fault) => "fault",
        Some(BadgeKind::Repairing) => "repairing",
        Some(BadgeKind::Resolved) => "resolved",
    }
}

fn badge_visibility_name(visibility: BadgeVisibility) -> &'static str {
    match visibility {
        BadgeVisibility::Shown => "shown",
        BadgeVisibility::NoTicket => "no-ticket",
        BadgeVisibility::OffScreen => "off-screen",
        BadgeVisibility::ProjectionFailed => "projection-failed",
        BadgeVisibility::NoCamera => "no-camera",
        BadgeVisibility::NoViewport => "no-viewport",
        BadgeVisibility::MissingRack => "missing-rack",
        BadgeVisibility::MissingBadgeNode => "missing-badge-node",
    }
}

fn key_name(key: KeyCode) -> String {
    match key {
        KeyCode::ArrowUp => "arrow-up".to_owned(),
        KeyCode::ArrowDown => "arrow-down".to_owned(),
        KeyCode::ArrowLeft => "arrow-left".to_owned(),
        KeyCode::ArrowRight => "arrow-right".to_owned(),
        KeyCode::KeyQ => "q".to_owned(),
        KeyCode::KeyE => "e".to_owned(),
        KeyCode::Space => "space".to_owned(),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn interaction_facts(stage: VerificationStage, outcome: InteractionOutcome) -> InteractionFacts {
    let (name, ticket, rack, distance) = match outcome {
        InteractionOutcome::None => ("none", None, None, None),
        InteractionOutcome::Started { ticket, rack } => {
            ("started", Some(ticket.value()), Some(rack), None)
        }
        InteractionOutcome::OutOfRange {
            nearest_rack,
            nearest_distance,
        } => (
            "out-of-range",
            None,
            nearest_rack,
            Some(canonical_float(nearest_distance)),
        ),
        InteractionOutcome::NoOpenTickets => ("no-open-tickets", None, None, None),
        InteractionOutcome::AlreadyRepairing { ticket } => {
            ("already-repairing", Some(ticket.value()), None, None)
        }
    };
    InteractionFacts {
        stage: stage.name().to_owned(),
        outcome: name.to_owned(),
        ticket,
        rack,
        distance,
    }
}

// ---------------------------------------------------------------------------
// Real keyboard injection
// ---------------------------------------------------------------------------

fn primary_window(world: &mut World) -> Option<Entity> {
    world
        .query_filtered::<Entity, With<Window>>()
        .iter(world)
        .next()
}

/// Writes one real `KeyboardInput` message, the only path that makes
/// `ButtonInput<KeyCode>` report a `just_pressed` frame.
fn write_key(world: &mut World, window: Entity, key: KeyCode, state: ButtonState) {
    world.write_message(KeyboardInput {
        key_code: key,
        logical_key: Key::Unidentified(NativeKey::Unidentified),
        state,
        text: None,
        repeat: false,
        window,
    });
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// One frame of the scripted journey.
///
/// This is an exclusive system so the whole documented sequence lives in one
/// readable place and can spawn screenshot observers directly. It runs last, in
/// [`CellShiftSet::VerificationProbe`], so everything it observes is this
/// frame's finished state.
fn drive_verification(world: &mut World) {
    let Some(mut run) = world.remove_resource::<VerificationRun>() else {
        return;
    };
    if run.finished {
        world.insert_resource(run);
        return;
    }

    // An outstanding screenshot readback owns the frame. The app keeps
    // rendering — that is the only thing that moves a readback along — but no
    // simulated time passes, no counter moves, and nothing is recorded, so the
    // number of pumps a particular GPU needs can never reach the report.
    if run.pending.is_some_and(|pending| !pending.completed) {
        if let Err(reason) = pump_pending_capture(world, &mut run) {
            run.machine.fail(reason);
            finish(world, &mut run);
        }
        world.insert_resource(run);
        return;
    }

    run.frame += 1;
    run.stage_frame += 1;

    let Some(window) = primary_window(world) else {
        run.machine.fail("the app never opened a primary window");
        finish(world, &mut run);
        world.insert_resource(run);
        return;
    };

    release_tapped_keys(&mut run, world, window);

    if watchdog_expired(&run) {
        let stage = run.machine.stage().name();
        run.machine
            .fail(format!("the app watchdog expired in stage {stage}"));
        finish(world, &mut run);
        world.insert_resource(run);
        return;
    }

    let state = snapshot(world);
    record_ticket_events(&mut run, &state);
    record_interactions(&mut run, &state);

    if let Err(reason) = step_stage(world, &mut run, &state, window) {
        run.machine.fail(reason);
    }

    if run.machine.stage() == VerificationStage::Failure
        || run.machine.stage() == VerificationStage::Success
    {
        finish(world, &mut run);
    }
    world.insert_resource(run);
}

fn record_ticket_events(run: &mut VerificationRun, state: &Snapshot) {
    let live = state
        .queue
        .ordered()
        .iter()
        .map(|ticket| TicketFacts {
            id: ticket.id.value(),
            rack: ticket.rack,
            severity: severity_name(ticket.severity).to_owned(),
            created_tick: ticket.created_tick,
        })
        .collect::<Vec<_>>();

    for ticket in &live {
        if !run
            .observations
            .active
            .iter()
            .any(|held| held.id == ticket.id)
        {
            run.observations.ticket_history.push(TicketEventFacts {
                event: "opened".to_owned(),
                ticket: ticket.id,
                rack: ticket.rack,
                severity: ticket.severity.clone(),
                tick: ticket.created_tick,
            });
        }
    }
    for held in &run.observations.active {
        if !live.iter().any(|ticket| ticket.id == held.id) {
            run.observations.ticket_history.push(TicketEventFacts {
                event: "removed".to_owned(),
                ticket: held.id,
                rack: held.rack,
                severity: held.severity.clone(),
                tick: state.tick,
            });
        }
    }
    run.observations.active = live;
}

fn record_interactions(run: &mut VerificationRun, state: &Snapshot) {
    if state.last.presses > run.observations.presses_seen {
        run.observations.presses_seen = state.last.presses;
        run.observations
            .interactions
            .push(interaction_facts(run.machine.stage(), state.last.outcome));
    }
}

/// Moves the machine on, resetting the per-stage counters.
fn advance(run: &mut VerificationRun) -> Result<(), String> {
    run.machine.advance().map_err(|error| error.to_string())?;
    run.stage_frame = 0;
    run.capture_index = 0;
    run.probe_index = 0;
    run.resize_frame = None;
    run.placed_frame = None;
    Ok(())
}

/// Everything a stalled stage was looking at, in one line.
///
/// A stage that burns its frame budget has always been waiting on some piece of
/// game state, and the bare stage name never says which. This lands in
/// `failure_reason`, which only a failing report carries, so the canonical
/// semantics of a successful report are untouched.
fn stall_facts(run: &VerificationRun, state: &Snapshot) -> String {
    let held = run
        .held
        .iter()
        .map(|key| key_name(*key))
        .collect::<Vec<_>>()
        .join("+");
    let queue = state
        .queue
        .ordered()
        .iter()
        .map(|ticket| format!("{}@{}", ticket.id.value(), ticket.rack))
        .collect::<Vec<_>>()
        .join(",");
    let racks = state
        .rack_states
        .iter()
        .map(|rack| format!("{rack:?}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "player ({:.3}, {:.3}); clip {}; lock {}; outcome {:?}; held [{}]; queue [{}]; racks [{}]; \
         tick {}; frame {}",
        state.player.x,
        state.player.y,
        clip_name(state.clip),
        state
            .lock
            .ticket()
            .map_or_else(|| "released".to_owned(), |ticket| ticket.to_string()),
        state.last.outcome,
        held,
        queue,
        racks,
        state.tick,
        run.frame,
    )
}

/// Whether the current stage has burned its frame budget.
fn budget_exhausted(run: &VerificationRun) -> bool {
    if matches!(
        run.fault,
        Some(VerificationFault::Stall | VerificationFault::Hang)
    ) {
        return false;
    }
    run.stage_frame > STAGE_FRAME_BUDGET
}

fn finish(world: &mut World, run: &mut VerificationRun) {
    if run.finished {
        return;
    }
    run.finished = true;
    let report = build_report(world, run);
    let canonical = canonical_json(&report);
    let path = run.output.report();
    if let Err(error) = fs::write(&path, canonical.as_bytes()) {
        error!("could not write the verification report: {error}");
        world.write_message(AppExit::error());
        return;
    }
    if report.result == "success" {
        info!(
            "verification succeeded; {} frames captured",
            report.frames.len()
        );
        world.write_message(AppExit::Success);
    } else {
        error!(
            "verification failed in stage {:?}: {:?}",
            report.failed_stage, report.failure_reason
        );
        world.write_message(AppExit::error());
    }
}

/// Holds exactly `keys` down, writing only the real press and release messages
/// the change requires.
fn hold_keys(run: &mut VerificationRun, world: &mut World, window: Entity, keys: &[KeyCode]) {
    let desired = keys.iter().copied().collect::<BTreeSet<_>>();
    let stage = run.machine.stage().name().to_owned();
    for key in desired.difference(&run.held).copied().collect::<Vec<_>>() {
        write_key(world, window, key, ButtonState::Pressed);
        run.observations.keys.push(KeyFacts {
            stage: stage.clone(),
            key: key_name(key),
            state: "pressed".to_owned(),
        });
    }
    for key in run.held.difference(&desired).copied().collect::<Vec<_>>() {
        write_key(world, window, key, ButtonState::Released);
        run.observations.keys.push(KeyFacts {
            stage: stage.clone(),
            key: key_name(key),
            state: "released".to_owned(),
        });
    }
    run.held = desired;
}

/// Presses one key for exactly the next frame.
fn tap_key(run: &mut VerificationRun, world: &mut World, window: Entity, key: KeyCode) {
    write_key(world, window, key, ButtonState::Pressed);
    run.observations.keys.push(KeyFacts {
        stage: run.machine.stage().name().to_owned(),
        key: key_name(key),
        state: "pressed".to_owned(),
    });
    run.release_next.push(key);
}

/// Releases every key [`tap_key`] pressed on the previous driver frame.
///
/// A tap is one real press followed by one real release a frame later, and
/// both halves are recorded, because the recorded key sequence is the evidence
/// that the journey was driven by the game's own input path. This runs at the
/// top of every driver frame, before any stage looks at the world, so
/// `release_next` is always empty by the time a stage decides what to press
/// next — which is why no stage guards on it.
fn release_tapped_keys(run: &mut VerificationRun, world: &mut World, window: Entity) {
    let stage = run.machine.stage().name().to_owned();
    for key in std::mem::take(&mut run.release_next) {
        write_key(world, window, key, ButtonState::Released);
        run.observations.keys.push(KeyFacts {
            stage: stage.clone(),
            key: key_name(key),
            state: "released".to_owned(),
        });
    }
}

/// The arrow keys that walk towards `target` through the live camera basis.
///
/// This is the real screen-relative control path: the world direction is
/// projected back onto the current [`ViewBasis`] axes, so the harness presses
/// exactly the keys a player looking at this heading would press.
fn arrows_towards(basis: &ViewBasis, from: Vec2, target: Vec2, tolerance: f32) -> Vec<KeyCode> {
    let delta = target - from;
    let screen = Vec2::new(basis.right().dot(delta), basis.forward().dot(delta));
    let mut keys = Vec::new();
    if screen.x > tolerance {
        keys.push(KeyCode::ArrowRight);
    } else if screen.x < -tolerance {
        keys.push(KeyCode::ArrowLeft);
    }
    if screen.y > tolerance {
        keys.push(KeyCode::ArrowUp);
    } else if screen.y < -tolerance {
        keys.push(KeyCode::ArrowDown);
    }
    keys
}

/// Requests one capture, or reports whether the outstanding one has landed.
///
/// A capture costs the journey no simulated time at all. The request frame
/// measures the frame's facts, spawns the real screenshot observer, and stops
/// the clock; the readback is then waited out on the condition that actually
/// matters — the observer's own record plus a complete file on disk — for as
/// many zero-time render pumps as the GPU needs. That is what makes the
/// canonical report reproducible: readback latency is a property of the
/// machine, and it never becomes simulated time. A callback that has not
/// arrived inside [`CAPTURE_TIMEOUT`] of wall clock really is lost, and is a
/// hard failure that names what it was waiting for.
///
/// The measured facts are *staged*, not reported, until the readback lands at
/// the contracted size. A frame's facts are evidence about a captured frame,
/// and a run that failed because the callback never arrived has no captured
/// frame to describe: reporting the facts anyway would put a full record of a
/// photograph nobody ever took into the failure evidence. A successful run
/// lands all fourteen, so the canonical report of a passing run is unchanged.
fn capture(
    world: &mut World,
    run: &mut VerificationRun,
    state: &Snapshot,
    frame: FrameName,
) -> Result<bool, String> {
    match run.pending {
        None => {
            let facts = frame_facts(world, run, state, frame)?;
            run.staged_facts = Some((frame, facts));
            request_capture(world, run, frame);
            set_simulated_step(world, Duration::ZERO);
            Ok(false)
        }
        Some(pending) => {
            // A checked failure, not a `debug_assert`: the release binary is
            // the one CI runs, and a stage waiting on somebody else's readback
            // would otherwise file the wrong frame's facts in silence.
            if pending.frame != frame {
                return Err(format!(
                    "stage {} waited on {} while {} was still outstanding",
                    run.machine.stage().name(),
                    frame.file_name(),
                    pending.frame.file_name()
                ));
            }
            if !poll_capture(world, run)? {
                return Ok(false);
            }
            // The readback landed, and the facts that describe it must be the
            // facts this stage measured for this frame. Neither branch below
            // may report success: a landed capture with nothing staged would
            // publish a frame the report says nothing about, and a landed
            // capture holding somebody else's facts would publish a frame
            // described by the wrong photograph. Both leave `pending` exactly
            // as it was, so nothing advances on a failure.
            match run.staged_facts.take() {
                Some((staged, facts)) if staged == frame => {
                    run.observations
                        .frames
                        .insert(frame.file_name().to_owned(), facts);
                    run.pending = None;
                    Ok(true)
                }
                Some((staged, _)) => Err(format!(
                    "{} landed, but the staged facts belong to {}",
                    frame.file_name(),
                    staged.file_name()
                )),
                None => Err(format!(
                    "{} landed with no staged facts; the stage never measured the frame it \
                     photographed",
                    frame.file_name()
                )),
            }
        }
    }
}

/// Runs one zero-time render pump for the outstanding capture.
///
/// This is the whole of a pumped frame: the readback is polled, and the
/// watchdog is still consulted — it just has nothing to charge, because a pump
/// only ever happens while a capture is outstanding and that whole wait is
/// excluded. What the check really catches here is the run that was already
/// over its active budget when it asked for the capture. Nothing else the
/// driver does happens here, which is what keeps an arbitrary number of pumps
/// invisible to the report.
fn pump_pending_capture(world: &mut World, run: &mut VerificationRun) -> Result<(), String> {
    if watchdog_expired(run) {
        let stage = run.machine.stage().name();
        return Err(format!("the app watchdog expired in stage {stage}"));
    }
    let callback_landed = run.pending.is_some_and(|pending| {
        pending.landed_on.is_some()
            || world
                .get_resource::<CaptureInbox>()
                .is_some_and(|inbox| inbox.completed.contains(&pending.frame))
    });
    if !callback_landed {
        std::thread::sleep(CAPTURE_PUMP_INTERVAL);
    }
    poll_capture(world, run)?;
    Ok(())
}

/// Whether the app's active-work watchdog has expired.
///
/// The comparison is against [`VerificationRun::active_elapsed`], not against
/// the run's lifetime: readback waiting belongs to [`CAPTURE_TIMEOUT`], and
/// charging it here as well is what turned a slow CI renderer into a false
/// "stuck state machine".
fn watchdog_expired(run: &VerificationRun) -> bool {
    run.fault != Some(VerificationFault::Hang) && run.active_elapsed() > run.watchdog
}

/// Banks an outstanding capture's wait into the excluded total, exactly once.
///
/// Every path that resolves a capture calls this, whether the readback landed,
/// timed out, or came back as something the contract refuses. Together with
/// the live subtraction in [`VerificationRun::active_elapsed`] it means a wait
/// is excluded from the watchdog continuously from request to resolution and
/// never counted twice.
fn bank_capture_wait(run: &mut VerificationRun, pending: &mut PendingCapture) {
    if !pending.charged {
        pending.charged = true;
        run.capture_excluded += pending.requested_at.elapsed();
    }
}

/// Polls the outstanding readback once, returning whether it has landed.
///
/// "Landed" is the real condition and nothing weaker: the observer recorded the
/// frame — which it only does after Bevy's own `save_to_disk` has returned —
/// and the file that observer wrote is on disk and not empty.
fn poll_capture(world: &mut World, run: &mut VerificationRun) -> Result<bool, String> {
    let Some(mut pending) = run.pending else {
        return Ok(true);
    };
    if pending.completed {
        return Ok(true);
    }

    pending.pumps += 1;
    if pending.landed_on.is_none()
        && world
            .get_resource::<CaptureInbox>()
            .is_some_and(|inbox| inbox.completed.contains(&pending.frame))
    {
        pending.landed_on = Some(pending.pumps);
    }

    let path = run.output.frame(pending.frame);
    let served = pending
        .landed_on
        .is_some_and(|landed| pending.pumps >= landed + run.capture_delay);
    if served && fs::metadata(&path).is_ok_and(|file| file.len() > 0) {
        // The observer only records after `save_to_disk` has returned, so a
        // recorded frame is a complete file. What it is *not* guaranteed to be
        // is the contracted size: a window server that hands back a
        // half-resolution surface writes a perfectly valid short PNG, and
        // nothing downstream would say so, because the report records the size
        // the frame was asked for rather than the size it came back at.
        let expected = pending.frame.size();
        match png_dimensions(&path) {
            Some(size) if size == expected => {}
            Some((width, height)) => {
                bank_capture_wait(run, &mut pending);
                run.pending = Some(pending);
                set_simulated_step(world, Duration::from_secs_f64(FIXED_STEP_SECONDS));
                return Err(format!(
                    "{} came back {width}x{height}, the contract needs {}x{}",
                    pending.frame.file_name(),
                    expected.0,
                    expected.1
                ));
            }
            None => {
                bank_capture_wait(run, &mut pending);
                run.pending = Some(pending);
                set_simulated_step(world, Duration::from_secs_f64(FIXED_STEP_SECONDS));
                return Err(format!(
                    "{} is not a readable PNG: {}",
                    pending.frame.file_name(),
                    artifact_state(&path)
                ));
            }
        }
        pending.completed = true;
        bank_capture_wait(run, &mut pending);
        run.pending = Some(pending);
        set_simulated_step(world, Duration::from_secs_f64(FIXED_STEP_SECONDS));
        return Ok(true);
    }

    let capture_timeout = run.capture_timeout_for(pending.frame);
    if pending.requested_at.elapsed() > capture_timeout {
        let reason = format!(
            "the screenshot callback for {} never fired within {:?}: stage {}, simulated frame {}, \
             {} zero-time render pumps, artifact {}",
            pending.frame.file_name(),
            capture_timeout,
            run.machine.stage().name(),
            pending.requested_on,
            pending.pumps,
            artifact_state(&path),
        );
        error!("{reason}; the artifact path was {}", path.display());
        bank_capture_wait(run, &mut pending);
        run.pending = Some(pending);
        set_simulated_step(world, Duration::from_secs_f64(FIXED_STEP_SECONDS));
        return Err(reason);
    }

    run.pending = Some(pending);
    Ok(false)
}

/// The pixel size a PNG declares, read from its header alone.
///
/// This really is a header read, not a decode and not a whole-file load: it
/// runs once per capture on the critical path, the frames are megabytes, and
/// the only question is whether the surface the window server handed back is
/// the one the contract names. Twenty-four bytes carry the signature, the
/// `IHDR` chunk name, and both dimensions, so twenty-four bytes are read.
fn png_dimensions(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;

    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut header = [0u8; 24];
    fs::File::open(path).ok()?.read_exact(&mut header).ok()?;
    if header[..8] != SIGNATURE || &header[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
    Some((width, height))
}

/// What one capture artifact looks like on disk, in one phrase.
fn artifact_state(path: &Path) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    match fs::metadata(path) {
        Ok(file) if file.len() > 0 => format!("{name} holds {} bytes", file.len()),
        Ok(_) => format!("{name} was written empty"),
        Err(error) => format!("{name} was never written ({error})"),
    }
}

/// Sets the simulated step every following frame advances by.
fn set_simulated_step(world: &mut World, step: Duration) {
    world.insert_resource(TimeUpdateStrategy::ManualDuration(step));
}

fn remaining_fault_step(elapsed: Duration) -> Duration {
    FAULT_INTERVAL
        .saturating_sub(elapsed)
        .max(Duration::from_secs_f64(FIXED_STEP_SECONDS))
}

fn fast_forward_to_next_fault(world: &mut World) {
    let elapsed = world
        .get_resource::<FaultScheduler>()
        .map_or(Duration::ZERO, FaultScheduler::elapsed);
    set_simulated_step(world, remaining_fault_step(elapsed));
}

fn verification_ticks(delta: Duration) -> u64 {
    ((delta.as_secs_f64() / FIXED_STEP_SECONDS).round() as u64).max(1)
}

fn advance_verification_clock(time: Res<Time>, mut clock: ResMut<OperationsClock>) {
    if time.delta().is_zero() {
        return;
    }
    clock.skip_verification_ticks(verification_ticks(time.delta()).saturating_sub(1));
}

/// Spawns the real screenshot entity with the mandated observers.
///
/// The first observer is Bevy's own `save_to_disk`; the second records the
/// frame only after that observer has returned, so a recorded frame is always
/// already a complete file on disk.
fn request_capture(world: &mut World, run: &mut VerificationRun, frame: FrameName) {
    let path = run.output.frame(frame);
    let mut save = save_to_disk(path);
    let record = run.fault != Some(VerificationFault::DropCapture);
    let mut commands = world.commands();
    commands.spawn(Screenshot::primary_window()).observe(
        move |captured: On<ScreenshotCaptured>, mut inbox: ResMut<CaptureInbox>| {
            save(captured);
            if record {
                inbox.completed.push(frame);
            }
        },
    );
    world.flush();
    run.pending = Some(PendingCapture {
        frame,
        requested_on: run.frame,
        requested_at: Instant::now(),
        pumps: 0,
        landed_on: None,
        completed: false,
        charged: false,
    });
}

/// Projects the technician's box onto the viewport and returns the crop the
/// worker analyzers run inside.
fn worker_crop(world: &mut World, player: Vec2, viewport: UVec2) -> RectFacts {
    let camera = world
        .query_filtered::<(&Camera, &GlobalTransform), With<CellShiftCamera>>()
        .iter(world)
        .next()
        .map(|(camera, transform)| (camera.clone(), *transform));
    let Some((camera, transform)) = camera else {
        return RectFacts::default();
    };

    let center = Vec3::new(player.x, WORKER_CENTER_HEIGHT, player.y);
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for corner in 0..8u8 {
        let offset = Vec3::new(
            if corner & 1 == 0 { -1.0 } else { 1.0 } * WORKER_HALF_EXTENTS.x,
            if corner & 2 == 0 { -1.0 } else { 1.0 } * WORKER_HALF_EXTENTS.y,
            if corner & 4 == 0 { -1.0 } else { 1.0 } * WORKER_HALF_EXTENTS.z,
        );
        let Ok(point) = camera.world_to_viewport(&transform, center + offset) else {
            return RectFacts::default();
        };
        min = min.min(point);
        max = max.max(point);
    }
    min -= Vec2::splat(WORKER_CROP_MARGIN);
    max += Vec2::splat(WORKER_CROP_MARGIN);
    let limit = viewport.as_vec2();
    min = min.max(Vec2::ZERO).min(limit);
    max = max.max(Vec2::ZERO).min(limit);
    RectFacts {
        x: canonical_float(min.x),
        y: canonical_float(min.y),
        width: canonical_float((max.x - min.x).max(0.0)),
        height: canonical_float((max.y - min.y).max(0.0)),
    }
}

/// Every authored equipment prop, projected onto the current viewport.
///
/// The bounds are the real ones: for each [`HallProp`] in an
/// [`EquipmentCategory`], the world-space union of the [`Aabb`] Bevy computed
/// for its own mesh and for every mesh the generated scene spawned beneath it.
/// Those bounds are split along their longest horizontal axis into segments no
/// longer than [`EQUIPMENT_SEGMENT_METRES`], and each segment's eight corners
/// are pushed through the live [`Camera`] projection.
fn equipment_facts(world: &mut World, viewport: UVec2) -> Vec<EquipmentFacts> {
    let camera = world
        .query_filtered::<(&Camera, &GlobalTransform), With<CellShiftCamera>>()
        .iter(world)
        .next()
        .map(|(camera, transform)| (camera.clone(), *transform));
    let Some((camera, view)) = camera else {
        return Vec::new();
    };

    let props = world
        .query::<(Entity, &HallProp)>()
        .iter(world)
        .filter_map(|(entity, prop)| {
            EquipmentCategory::of(prop.asset)
                .map(|category| (entity, prop.id.as_str().to_owned(), category))
        })
        .collect::<Vec<_>>();

    let limit = viewport.as_vec2();
    let mut facts = Vec::with_capacity(props.len());
    for (entity, id, category) in props {
        let Some((world_min, world_max)) = mesh_bounds(world, entity) else {
            continue;
        };

        let mut bounds_min = Vec2::splat(f32::INFINITY);
        let mut bounds_max = Vec2::splat(f32::NEG_INFINITY);
        let mut regions = Vec::new();
        for (segment_min, segment_max) in segment_bounds(world_min, world_max) {
            let Some((min, max)) = project_box(&camera, &view, segment_min, segment_max) else {
                continue;
            };
            bounds_min = bounds_min.min(min);
            bounds_max = bounds_max.max(max);

            let clipped_min = min.max(Vec2::ZERO).min(limit);
            let clipped_max = max.max(Vec2::ZERO).min(limit);
            let size = (clipped_max - clipped_min).max(Vec2::ZERO);
            let rect = RectFacts {
                x: canonical_float(clipped_min.x),
                y: canonical_float(clipped_min.y),
                width: canonical_float(size.x),
                height: canonical_float(size.y),
            };
            if PixelRect::snap(rect, viewport.x, viewport.y).area() >= EQUIPMENT_REGION_MIN_PIXELS {
                regions.push(rect);
            }
        }
        if !bounds_min.is_finite() || !bounds_max.is_finite() {
            continue;
        }

        let on_screen = bounds_max.x > 0.0
            && bounds_max.y > 0.0
            && bounds_min.x < limit.x
            && bounds_min.y < limit.y;
        facts.push(EquipmentFacts {
            id,
            category: category.name().to_owned(),
            world_bounds: [
                [
                    canonical_float(world_min.x),
                    canonical_float(world_min.y),
                    canonical_float(world_min.z),
                ],
                [
                    canonical_float(world_max.x),
                    canonical_float(world_max.y),
                    canonical_float(world_max.z),
                ],
            ],
            projected_bounds: [
                canonical_float(bounds_min.x),
                canonical_float(bounds_min.y),
                canonical_float(bounds_max.x),
                canonical_float(bounds_max.y),
            ],
            on_screen,
            regions: if on_screen { regions } else { Vec::new() },
        });
    }
    facts.sort_by(|left, right| left.id.cmp(&right.id));
    facts
}

/// The world-space bounds of every mesh spawned at or under one entity.
fn mesh_bounds(world: &World, root: Entity) -> Option<(Vec3, Vec3)> {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter());
        }
        let (Some(aabb), Some(global)) = (
            world.get::<Aabb>(entity),
            world.get::<GlobalTransform>(entity),
        ) else {
            continue;
        };
        let center = Vec3::from(aabb.center);
        let half = Vec3::from(aabb.half_extents);
        for corner in 0..8u8 {
            let local = center + half * corner_sign(corner);
            let point = global.transform_point(local);
            min = min.min(point);
            max = max.max(point);
        }
    }
    (min.is_finite() && max.is_finite()).then_some((min, max))
}

/// The `-1`/`+1` sign vector of one axis-aligned box corner.
fn corner_sign(corner: u8) -> Vec3 {
    Vec3::new(
        if corner & 1 == 0 { -1.0 } else { 1.0 },
        if corner & 2 == 0 { -1.0 } else { 1.0 },
        if corner & 4 == 0 { -1.0 } else { 1.0 },
    )
}

/// Splits one world-space box along its longest horizontal axis.
fn segment_bounds(min: Vec3, max: Vec3) -> Vec<(Vec3, Vec3)> {
    let span = max - min;
    let (axis, length) = if span.x >= span.z {
        (0usize, span.x)
    } else {
        (2usize, span.z)
    };
    let count =
        ((length / EQUIPMENT_SEGMENT_METRES).ceil() as usize).clamp(1, EQUIPMENT_MAX_SEGMENTS);
    let step = length / count as f32;
    (0..count)
        .map(|index| {
            let low = min[axis] + step * index as f32;
            let high = if index + 1 == count {
                max[axis]
            } else {
                low + step
            };
            let mut segment_min = min;
            let mut segment_max = max;
            segment_min[axis] = low;
            segment_max[axis] = high;
            (segment_min, segment_max)
        })
        .collect()
}

/// Projects one world-space box onto the viewport, as unclipped pixel bounds.
fn project_box(
    camera: &Camera,
    view: &GlobalTransform,
    min: Vec3,
    max: Vec3,
) -> Option<(Vec2, Vec2)> {
    let center = (min + max) * 0.5;
    let half = (max - min) * 0.5;
    let mut low = Vec2::splat(f32::INFINITY);
    let mut high = Vec2::splat(f32::NEG_INFINITY);
    for corner in 0..8u8 {
        let point = camera
            .world_to_viewport(view, center + half * corner_sign(corner))
            .ok()?;
        low = low.min(point);
        high = high.max(point);
    }
    Some((low, high))
}

/// The laid-out rectangle of one UI node, in logical pixels.
fn ui_rect(world: &World, entity: Entity) -> Option<RectFacts> {
    let node = world.get::<ComputedNode>(entity)?;
    let transform = world.get::<UiGlobalTransform>(entity)?;
    let scale = node.inverse_scale_factor;
    let center = transform.translation * scale;
    let half = node.size * 0.5 * scale;
    Some(RectFacts::from_center(center, half))
}

fn hud_panels(world: &mut World) -> BTreeMap<String, RectFacts> {
    let mut panels = BTreeMap::new();
    if let Some(entity) = world
        .query_filtered::<Entity, With<TicketQueuePanel>>()
        .iter(world)
        .next()
        && let Some(rect) = ui_rect(world, entity)
    {
        panels.insert("queue".to_owned(), rect);
    }
    if let Some(entity) = world
        .query_filtered::<Entity, With<ControlsPanel>>()
        .iter(world)
        .next()
        && let Some(rect) = ui_rect(world, entity)
    {
        panels.insert("controls".to_owned(), rect);
    }
    let rows = world
        .query::<(Entity, &QueueRowNode)>()
        .iter(world)
        .map(|(entity, row)| (entity, row.slot))
        .collect::<Vec<_>>();
    for (entity, slot) in rows {
        if world
            .get::<Node>(entity)
            .is_some_and(|node| node.display == Display::None)
        {
            continue;
        }
        if let Some(rect) = ui_rect(world, entity) {
            panels.insert(format!("queue-row-{slot}"), rect);
        }
    }
    panels
}

/// Everything one capture records about the live game.
fn frame_facts(
    world: &mut World,
    run: &VerificationRun,
    state: &Snapshot,
    frame: FrameName,
) -> Result<FrameFacts, String> {
    let Some(viewport) = state.viewport else {
        return Err(format!(
            "the camera had no viewport when {} was captured",
            frame.file_name()
        ));
    };
    let expected = frame.size();
    if (viewport.x, viewport.y) != expected {
        return Err(format!(
            "{} needs a {}x{} viewport, the camera reported {}x{}",
            frame.file_name(),
            expected.0,
            expected.1,
            viewport.x,
            viewport.y
        ));
    }
    if !state.hud.errors.is_empty() {
        return Err(format!(
            "the HUD refused to draw {} cleanly: {:?}",
            frame.file_name(),
            state.hud.errors
        ));
    }

    let yaw = state.orbit.yaw_radians();
    let target = clamp_follow_target(state.player, RENDER_COVERAGE_SIZE, yaw);
    let quad = ground_quadrilateral(yaw, target);

    Ok(FrameFacts {
        path: frame.file_name().to_owned(),
        width: expected.0,
        height: expected.1,
        stage: run.machine.stage().name().to_owned(),
        heading: heading_name(state.orbit.heading()).to_owned(),
        camera_yaw_degrees: canonical_float(state.orbit.yaw_degrees()),
        camera_settled: state.orbit.is_settled(),
        camera_progress: canonical_float(state.orbit.progress()),
        camera_target: [canonical_float(target.x), canonical_float(target.y)],
        ground_quadrilateral: [
            [canonical_float(quad[0].x), canonical_float(quad[0].y)],
            [canonical_float(quad[1].x), canonical_float(quad[1].y)],
            [canonical_float(quad[2].x), canonical_float(quad[2].y)],
            [canonical_float(quad[3].x), canonical_float(quad[3].y)],
        ],
        player_position: [
            canonical_float(state.player.x),
            canonical_float(state.player.y),
        ],
        player_clip: clip_name(state.clip).to_owned(),
        movement_locked: state.lock.is_locked(),
        worker_crop: worker_crop(world, state.player, viewport),
        hud_status: status_name(state.hud.status).to_owned(),
        hud_rows: state
            .hud
            .rows
            .iter()
            .map(|row| HudRowFacts {
                slot: row.slot,
                ticket: row.ticket.value(),
                rack: row.rack,
                severity: severity_name(row.severity).to_owned(),
                state: rack_state_name(row.state).to_owned(),
                progress: canonical_float(row.progress),
                label: row.label.clone(),
            })
            .collect(),
        hud_panels: hud_panels(world),
        badges: state
            .hud
            .badges
            .iter()
            .map(|badge| BadgeFacts {
                rack: badge.rack,
                kind: badge_kind_name(badge.kind).to_owned(),
                visibility: badge_visibility_name(badge.visibility).to_owned(),
                rect: badge
                    .center
                    .map(|center| RectFacts::from_center(center, badge_half_extents())),
            })
            .collect(),
        tickets: run.observations.active.clone(),
        rack_states: state
            .rack_states
            .iter()
            .map(|rack| rack_state_name(*rack).to_owned())
            .collect(),
        equipment: equipment_facts(world, viewport),
    })
}

/// Places the technician at one exact ground point.
///
/// The corner probes use this rather than walking twenty metres four times:
/// what they prove is the camera clamp and the rendered-coverage sentinel at
/// the extremes of the room, and the real arrow-key path is already driven for
/// the whole journey by [`VerificationStage::KeyboardJourney`].
fn place_player(world: &mut World, position: Vec2) {
    let entity = world
        .query_filtered::<Entity, With<Technician>>()
        .iter(world)
        .next();
    if let Some(entity) = entity
        && let Some(mut transform) = world.get_mut::<Transform>(entity)
    {
        transform.translation.x = position.x;
        transform.translation.z = position.y;
    }
}

/// Returns the operations model to its documented origin.
///
/// Loading generated assets takes a wall-clock-dependent number of frames, so
/// without this the seeded scheduler would start part way through an interval
/// and every recorded tick would drift between runs. After the reset, every
/// simulated frame advances exactly [`FIXED_STEP_SECONDS`], so ticket 1 always
/// opens on tick 240.
fn reset_operations(world: &mut World) {
    let roster = world
        .get_resource::<RackRoster>()
        .cloned()
        .unwrap_or_default();
    for entry in roster.all() {
        if let Some(mut operations) = world.get_mut::<RackOperations>(entry.entity) {
            *operations = RackOperations::new(entry.rack, entry.id.clone());
        }
    }
    world.insert_resource(FaultScheduler::new(roster.len()));
    world.insert_resource(TicketQueue::default());
    world.insert_resource(OperationsClock::default());
    world.insert_resource(MovementLock::default());
    world.insert_resource(LastInteraction::default());
    world.insert_resource(CameraOrbit::default());

    let spawn = world
        .get_resource::<PlayerSpawnPoint>()
        .map_or(Vec2::ZERO, |spawn| spawn.0);
    place_player(world, spawn);

    let animations = world.get_resource::<PlayerAnimations>().map(|animations| {
        (
            animations.player,
            PlayerClip::ALL.map(|clip| animations.node(clip)),
        )
    });
    if let Some((player, nodes)) = animations
        && let Some(mut animation) = world.get_mut::<AnimationPlayer>(player)
    {
        for node in nodes {
            if let Some(active) = animation.animation_mut(node) {
                active.seek_to(0.0);
            }
        }
    }
}

/// Runs one frame of the current stage.
#[allow(clippy::too_many_lines)]
fn step_stage(
    world: &mut World,
    run: &mut VerificationRun,
    state: &Snapshot,
    window: Entity,
) -> Result<(), String> {
    if budget_exhausted(run) {
        return Err(format!(
            "stage {} exceeded {STAGE_FRAME_BUDGET} frames; {}",
            run.machine.stage().name(),
            stall_facts(run, state)
        ));
    }

    if matches!(
        run.fault,
        Some(VerificationFault::Stall | VerificationFault::Hang)
    ) && run.machine.stage() == VerificationStage::SeedThreeFaults
    {
        return Ok(());
    }

    match run.machine.stage() {
        VerificationStage::Boot => {
            hold_keys(run, world, window, &[]);
            advance(run)
        }

        VerificationStage::WaitForAssets => {
            if state.assets == AssetLoadState::Failed {
                return Err("the generated assets failed to load".to_owned());
            }
            if state.rig == PlayerRigState::Failed {
                return Err("the technician rig failed to bind".to_owned());
            }
            let ready = state.assets == AssetLoadState::Ready
                && state.hall == HallState::Ready
                && state.rig == PlayerRigState::Ready
                && state.rig_healthy
                && !state.roster.is_empty()
                && state.viewport.is_some();
            if ready { advance(run) } else { Ok(()) }
        }

        VerificationStage::ValidateBlueprint => {
            // The hall spawns from an inserted `HallBlueprint` when one exists
            // and otherwise from the authored `v0`, exactly as `spawn_hall`
            // does, so verification validates the hall that actually spawned.
            let blueprint = world
                .get_resource::<HallBlueprint>()
                .map_or_else(SceneBlueprint::v0, |blueprint| blueprint.0.clone());
            let errors = blueprint.validate();
            if !errors.is_empty() {
                return Err(format!("the authored hall failed validation: {errors:?}"));
            }
            let walkable = blueprint.walkable_report();
            if !walkable.is_connected() {
                return Err("the authored hall has unreachable aisle checkpoints".to_owned());
            }
            if state.roster.len() != blueprint.rack_row_count() {
                return Err(format!(
                    "the roster holds {} racks, the blueprint authored {}",
                    state.roster.len(),
                    blueprint.rack_row_count()
                ));
            }
            let missing = required_player_parts()
                .into_iter()
                .filter(|name| !state.parts.iter().any(|part| part == name))
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(format!("the technician rig is missing {missing:?}"));
            }
            check_camera_render_settings(world)?;

            run.observations.rig_parts = state.parts.clone();
            run.observations.blueprint = Some(BlueprintFacts {
                room: [
                    canonical_float(blueprint.room.size.x),
                    canonical_float(blueprint.room.size.y),
                ],
                coverage: [
                    canonical_float(blueprint.room.coverage.x),
                    canonical_float(blueprint.room.coverage.y),
                ],
                visuals: blueprint.visuals.len(),
                colliders: blueprint.colliders.len(),
                rack_rows: blueprint.rack_row_count(),
                aisles: blueprint.aisles.len(),
                player_spawn: [
                    canonical_float(blueprint.player_spawn.x),
                    canonical_float(blueprint.player_spawn.y),
                ],
                walkable_connected: walkable.is_connected(),
                validation_errors: Vec::new(),
            });

            reset_operations(world);
            run.observations.active.clear();
            run.observations.ticket_history.clear();
            run.observations.presses_seen = 0;
            advance(run)
        }

        VerificationStage::HealthyCapture => {
            if run.pending.is_none() && !state.queue.is_empty() {
                return Err("the healthy capture ran with an open ticket".to_owned());
            }
            if capture(world, run, state, FrameName::HealthyCenterNorthEast)? {
                fast_forward_to_next_fault(world);
                advance(run)
            } else {
                Ok(())
            }
        }

        VerificationStage::SeedThreeFaults => {
            if state.queue.len() >= MAX_ACTIVE_TICKETS {
                set_simulated_step(world, Duration::from_secs_f64(FIXED_STEP_SECONDS));
                advance(run)
            } else {
                fast_forward_to_next_fault(world);
                Ok(())
            }
        }

        VerificationStage::FaultQueueCapture => {
            if capture(world, run, state, FrameName::FaultQueueNorthEast)? {
                advance(run)
            } else {
                Ok(())
            }
        }

        VerificationStage::KeyboardJourney => {
            // One real out-of-range Space press, recorded as a rejection.
            if run.stage_frame == 1 {
                tap_key(run, world, window, REPAIR_KEY);
                return Ok(());
            }
            // One real simultaneous Q and E, which the documented orbit rule
            // cancels exactly. This is the only place the harness presses Q,
            // and it is checked rather than merely recorded.
            if run.stage_frame == 3 {
                tap_key(run, world, window, KeyCode::KeyQ);
                tap_key(run, world, window, KeyCode::KeyE);
                return Ok(());
            }
            if run.stage_frame == 5 && !state.orbit.is_settled() {
                return Err(format!(
                    "simultaneous Q and E must cancel, the orbit was turning towards {}",
                    heading_name(state.orbit.heading())
                ));
            }
            // Walk north up the aisle towards the repair spot and hand over to
            // the walk capture while the arrow keys are still held, so the
            // captured frame really is a moving technician.
            let spot = journey_target(state)?;
            let basis = world
                .get_resource::<ViewBasis>()
                .copied()
                .unwrap_or_default();
            let along = Vec2::new(state.player.x, spot.y);
            let keys = arrows_towards(&basis, state.player, along, ARRIVAL_TOLERANCE);
            hold_keys(run, world, window, &keys);
            if keys.is_empty() {
                return Err("the journey stopped before it reached the walk capture".to_owned());
            }
            if state.player.y >= WALK_CAPTURE_Z && state.clip == PlayerClip::Walk {
                advance(run)
            } else {
                Ok(())
            }
        }

        VerificationStage::WalkCapture => {
            if run.pending.is_none() && state.clip != PlayerClip::Walk {
                return Err(format!(
                    "the walk capture needs the Walk clip, the technician was playing {}",
                    clip_name(state.clip)
                ));
            }
            let spot = journey_target(state)?;
            let basis = world
                .get_resource::<ViewBasis>()
                .copied()
                .unwrap_or_default();
            let along = Vec2::new(state.player.x, spot.y);
            let keys = arrows_towards(&basis, state.player, along, ARRIVAL_TOLERANCE);
            hold_keys(run, world, window, &keys);
            if capture(world, run, state, FrameName::WalkNorthEast)? {
                advance(run)
            } else {
                Ok(())
            }
        }

        VerificationStage::BeginRepair => {
            let spot = journey_target(state)?;
            let basis = world
                .get_resource::<ViewBasis>()
                .copied()
                .unwrap_or_default();
            let keys = arrows_towards(&basis, state.player, spot, ARRIVAL_TOLERANCE);
            match begin_repair_action(state, keys.is_empty(), run.stage_frame) {
                BeginRepairAction::HandOver => {
                    hold_keys(run, world, window, &[]);
                    advance(run)
                }
                BeginRepairAction::Walk => {
                    hold_keys(run, world, window, &keys);
                    Ok(())
                }
                BeginRepairAction::Tap => {
                    hold_keys(run, world, window, &[]);
                    tap_key(run, world, window, REPAIR_KEY);
                    Ok(())
                }
                BeginRepairAction::Settle => {
                    hold_keys(run, world, window, &[]);
                    Ok(())
                }
            }
        }

        VerificationStage::RepairCapture => {
            if run.pending.is_none() && !state.lock.is_locked() {
                return Err("the repair capture ran with no repair holding the controls".to_owned());
            }
            if capture(world, run, state, FrameName::RepairingNorthEast)? {
                advance(run)
            } else {
                Ok(())
            }
        }

        VerificationStage::CompleteRepair => {
            if state.rack_states.get(JOURNEY_RACK) == Some(&RackState::Resolved) {
                advance(run)
            } else if state.lock.is_locked()
                || state.rack_states.get(JOURNEY_RACK) == Some(&RackState::Repairing)
            {
                Ok(())
            } else {
                Err(format!(
                    "rack {JOURNEY_RACK} left the repair without resolving: {:?}",
                    state.rack_states.get(JOURNEY_RACK)
                ))
            }
        }

        VerificationStage::ResolvedCapture => {
            if capture(world, run, state, FrameName::ResolvedNorthEast)? {
                advance(run)
            } else {
                Ok(())
            }
        }

        VerificationStage::OrbitSouthEast => {
            orbit_to(run, world, window, state, CameraHeading::SouthEast)
        }
        VerificationStage::SettledSouthEastCapture => {
            settled_capture(world, run, state, FrameName::SettledSouthEast)
        }
        VerificationStage::OrbitSouthWest => {
            orbit_to(run, world, window, state, CameraHeading::SouthWest)
        }
        VerificationStage::SettledSouthWestCapture => {
            settled_capture(world, run, state, FrameName::SettledSouthWest)
        }
        VerificationStage::OrbitNorthWest => {
            orbit_to(run, world, window, state, CameraHeading::NorthWest)
        }
        VerificationStage::SettledNorthWestCapture => {
            settled_capture(world, run, state, FrameName::SettledNorthWest)
        }

        VerificationStage::MidOrbitCapture => {
            if run.pending.is_none() && run.stage_frame == 1 {
                tap_key(run, world, window, KeyCode::KeyE);
                return Ok(());
            }
            if run.pending.is_none() {
                if state.orbit.is_settled() {
                    return Err("the mid-orbit capture never caught a running tween".to_owned());
                }
                if state.orbit.progress() < 0.5 {
                    return Ok(());
                }
            }
            if capture(world, run, state, FrameName::MidOrbit)? {
                advance(run)
            } else {
                Ok(())
            }
        }

        VerificationStage::CornerProbes => {
            const PROBES: [(CameraHeading, FrameName); 4] = [
                (CameraHeading::NorthEast, FrameName::CornerNorthEast),
                (CameraHeading::SouthEast, FrameName::CornerSouthEast),
                (CameraHeading::SouthWest, FrameName::CornerSouthWest),
                (CameraHeading::NorthWest, FrameName::CornerNorthWest),
            ];
            let Some((heading, frame)) = PROBES.get(run.probe_index).copied() else {
                return advance(run);
            };
            if state.orbit.heading() != heading || !state.orbit.is_settled() {
                if state.orbit.is_settled() {
                    tap_key(run, world, window, KeyCode::KeyE);
                }
                return Ok(());
            }
            let corner = probe_corner(heading);
            place_player(world, corner);
            // The camera follows before this system runs, so the placement
            // needs its own settle window: capturing on the placement frame
            // would photograph the previous corner's camera.
            let placed = *run.placed_frame.get_or_insert(run.frame);
            if run.pending.is_none()
                && (run.frame - placed < PROBE_SETTLE_FRAMES
                    || state.player.distance(corner) > 1.0e-3)
            {
                return Ok(());
            }
            if capture(world, run, state, frame)? {
                run.probe_index += 1;
                run.stage_frame = 0;
                run.placed_frame = None;
                if run.probe_index == PROBES.len() {
                    return advance(run);
                }
            }
            Ok(())
        }

        VerificationStage::LowResolutionCapture => {
            if run.resize_frame.is_none() {
                let entity = primary_window(world);
                if let Some(entity) = entity
                    && let Some(mut settings) = world.get_mut::<Window>(entity)
                {
                    settings.resolution.set(
                        VERIFICATION_WINDOW_WIDTH as f32,
                        VERIFICATION_WINDOW_HEIGHT as f32,
                    );
                }
                run.resize_frame = Some(run.frame);
                return Ok(());
            }
            let waited = run.frame - run.resize_frame.unwrap_or(run.frame);
            if waited < RESIZE_FRAMES {
                return Ok(());
            }
            if state.viewport
                != Some(UVec2::new(
                    VERIFICATION_WINDOW_WIDTH,
                    VERIFICATION_WINDOW_HEIGHT,
                ))
            {
                return Err(format!(
                    "the window never resized to {VERIFICATION_WINDOW_WIDTH}x{VERIFICATION_WINDOW_HEIGHT}, the camera reported {:?}",
                    state.viewport
                ));
            }
            if run.pending.is_none() && state.queue.len() < MAX_ACTIVE_TICKETS {
                return Ok(());
            }
            if capture(world, run, state, FrameName::LowResolutionQueue)? {
                advance(run)
            } else {
                Ok(())
            }
        }

        VerificationStage::AnalyzeReady => {
            let missing = FrameName::ALL
                .into_iter()
                .filter(|frame| !run.output.frame(*frame).is_file())
                .map(FrameName::file_name)
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(format!("these frames were never written: {missing:?}"));
            }
            advance(run)
        }

        VerificationStage::WriteReport => advance(run),
        VerificationStage::Success | VerificationStage::Failure => Ok(()),
    }
}

/// What one observed frame of [`VerificationStage::BeginRepair`] should do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BeginRepairAction {
    /// Release every held key and move on to the repair capture.
    HandOver,
    /// Keep holding the arrow keys that walk towards the repair spot.
    Walk,
    /// Standing on the spot and idle: press the real repair key.
    Tap,
    /// Standing on the spot, waiting out the tap cadence.
    Settle,
}

/// Decides one frame of the repair hand-over.
///
/// Order matters here, and it is the whole point of this function. Accepting a
/// repair is irreversible: the movement lock takes the controls and the
/// interaction records [`InteractionOutcome::Started`]. Once either is true the
/// harness must stop navigating, because the lock it just asked for is exactly
/// what stops the technician reaching the arrival tolerance. Checking arrival
/// first lets a sub-centimetre drift past the accepted repair edge mask the
/// success forever: the stage holds a movement key against a locked
/// technician, the repair runs to completion underneath it, and the stage burns
/// its whole frame budget without ever looking at the state it caused.
fn begin_repair_action(state: &Snapshot, arrived: bool, stage_frame: u64) -> BeginRepairAction {
    if state.lock.is_locked() || matches!(state.last.outcome, InteractionOutcome::Started { .. }) {
        return BeginRepairAction::HandOver;
    }
    if !arrived {
        return BeginRepairAction::Walk;
    }
    if state.clip == PlayerClip::Idle && stage_frame.is_multiple_of(4) {
        BeginRepairAction::Tap
    } else {
        BeginRepairAction::Settle
    }
}

/// The ground point the scripted journey repairs [`JOURNEY_RACK`] from.
fn journey_target(state: &Snapshot) -> Result<Vec2, String> {
    let entry = state
        .roster
        .get(JOURNEY_RACK)
        .ok_or_else(|| format!("rack {JOURNEY_RACK} is not on the roster"))?;
    Ok(journey_repair_spot(entry.center, entry.half_extents))
}

/// Presses the real `E` key until the orbit settles on `heading`.
///
/// A settled orbit is the whole guard: the tween starts on the frame the game
/// consumes the press, so the next driver frame already reads unsettled and
/// cannot tap again.
fn orbit_to(
    run: &mut VerificationRun,
    world: &mut World,
    window: Entity,
    state: &Snapshot,
    heading: CameraHeading,
) -> Result<(), String> {
    if state.orbit.heading() == heading && state.orbit.is_settled() {
        return advance(run);
    }
    if state.orbit.heading() != heading && state.orbit.is_settled() {
        tap_key(run, world, window, KeyCode::KeyE);
    }
    Ok(())
}

/// Captures one settled heading, refusing to capture a running tween.
fn settled_capture(
    world: &mut World,
    run: &mut VerificationRun,
    state: &Snapshot,
    frame: FrameName,
) -> Result<(), String> {
    if run.pending.is_none() && !state.orbit.is_settled() {
        return Ok(());
    }
    if capture(world, run, state, frame)? {
        advance(run)
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Report assembly
// ---------------------------------------------------------------------------

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("{} is unreadable: {error}", path.display()))?;
    let mut digest = Sha256::new();
    digest.update(&bytes);
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn hash_map(paths: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    for path in paths {
        map.insert(path.clone(), sha256_file(Path::new(path))?);
    }
    Ok(map)
}

/// Every repository-relative path whose exact bytes the report pins.
fn report_inputs() -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let assets = ASSET_NAMES
        .iter()
        .map(|name| format!("assets/generated/{name}.glb"))
        .collect();
    let sources = ASSET_NAMES
        .iter()
        .map(|name| format!("assets/source/{name}.ron"))
        .collect();
    // The references the report pins come from the manifest rather than from
    // a pair of constants, so adding an approved reference to the manifest is
    // enough to make every run pin it.
    let references = crate::reference::approved_references()
        .assets
        .iter()
        .map(|asset| asset.public_path.clone())
        .collect();
    let code = [
        "src/verification.rs",
        "src/metrics.rs",
        "src/design.rs",
        "src/camera.rs",
        "src/hud.rs",
        "src/operations.rs",
        "src/player.rs",
        "src/world.rs",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    (assets, sources, references, code)
}

fn build_report(world: &mut World, run: &mut VerificationRun) -> VerificationReport {
    let state = snapshot(world);
    let (assets, sources, references, code) = report_inputs();
    let mut failure = run
        .machine
        .failure()
        .map(|(stage, reason)| (stage.to_owned(), reason.to_owned()));

    let hashes = hash_map(&assets)
        .and_then(|assets| Ok((assets, hash_map(&sources)?)))
        .and_then(|(assets, sources)| Ok((assets, sources, hash_map(&references)?)))
        .and_then(|(assets, sources, references)| {
            Ok((assets, sources, references, hash_map(&code)?))
        });
    let (assets, asset_sources, reference_hashes, source_hashes) = match hashes {
        Ok(hashes) => hashes,
        Err(reason) => {
            failure.get_or_insert(("write-report".to_owned(), reason));
            (
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
            )
        }
    };

    let blueprint = run
        .observations
        .blueprint
        .clone()
        .unwrap_or(BlueprintFacts {
            room: [0.0, 0.0],
            coverage: [0.0, 0.0],
            visuals: 0,
            colliders: 0,
            rack_rows: 0,
            aisles: 0,
            player_spawn: [0.0, 0.0],
            walkable_connected: false,
            validation_errors: vec!["the blueprint was never validated".to_owned()],
        });

    let result = if failure.is_none() && run.machine.stage() == VerificationStage::Success {
        "success"
    } else {
        "failure"
    };
    let camera = camera_render_facts(world);

    VerificationReport {
        schema_version: 1,
        result: result.to_owned(),
        failed_stage: failure.as_ref().map(|(stage, _)| stage.clone()),
        failure_reason: failure.as_ref().map(|(_, reason)| reason.clone()),
        stages: run
            .machine
            .visited()
            .iter()
            .map(|stage| stage.name().to_owned())
            .collect(),
        assets,
        asset_sources,
        references: reference_hashes,
        sources: source_hashes,
        blueprint,
        camera,
        gameplay: GameplayFacts {
            fault_seed: format!("{FAULT_SCHEDULER_SEED:#018x}"),
            fixed_step_seconds: canonical_f64(FIXED_STEP_SECONDS),
            ticket_history: run.observations.ticket_history.clone(),
            interactions: run.observations.interactions.clone(),
            keys: run.observations.keys.clone(),
            tickets_emitted: state.scheduler.0,
            capacity_pauses: state.scheduler.1,
            duplicate_pauses: state.scheduler.2,
            busy_pauses: state.scheduler.3,
            rig_parts: run.observations.rig_parts.clone(),
            repaired_rack: JOURNEY_RACK,
        },
        frames: run.observations.frames.clone(),
    }
}

/// Reads the render settings off the one live game camera.
fn camera_render_facts(world: &mut World) -> CameraRenderFacts {
    let clear_color = world
        .get_resource::<ClearColor>()
        .map_or(Srgba::NONE, |clear| clear.0.to_srgba());
    let settings = world
        .query_filtered::<(Option<&Tonemapping>, Option<&DebandDither>, Option<&Msaa>), With<CellShiftCamera>>()
        .iter(world)
        .next()
        .map(|(tonemapping, dither, msaa)| (tonemapping.copied(), dither.copied(), msaa.copied()));
    let (tonemapping, dither, msaa) = settings.unwrap_or((None, None, None));
    CameraRenderFacts {
        tonemapping: tonemapping.map_or_else(|| "absent".to_owned(), |value| format!("{value:?}")),
        deband_dither: dither.map_or_else(|| "absent".to_owned(), |value| format!("{value:?}")),
        msaa_samples: msaa.map_or(0, |value| value.samples()),
        clear_color: format!("#{}", clear_color.to_hex().trim_start_matches('#')),
    }
}

/// Fails the run unless the live camera renders the production cel-shift
/// display contract, with multisampling as the single allowed difference.
///
/// This is the point of the whole harness: the analyzed frame has to be the
/// frame the shipped game draws. If the display transform or the dither were
/// ever set here rather than in [`crate::camera`], every palette contract
/// below would be measuring a picture no player ever sees.
fn check_camera_render_settings(world: &mut World) -> Result<(), String> {
    let facts = camera_render_facts(world);
    let expected_tonemapping = format!("{CEL_SHIFT_TONEMAPPING:?}");
    let expected_dither = format!("{CEL_SHIFT_DEBAND_DITHER:?}");
    if facts.tonemapping != expected_tonemapping {
        return Err(format!(
            "the game camera must carry the production display transform {expected_tonemapping}, it carried {}",
            facts.tonemapping
        ));
    }
    if facts.deband_dither != expected_dither {
        return Err(format!(
            "the game camera must carry the production dither {expected_dither}, it carried {}",
            facts.deband_dither
        ));
    }
    if facts.msaa_samples != VERIFICATION_MSAA.samples() {
        return Err(format!(
            "the verification camera must render with {} samples, it carried {}",
            VERIFICATION_MSAA.samples(),
            facts.msaa_samples
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Drives the scripted verification journey over the real game.
pub struct VerificationPlugin {
    output: VerifyOutput,
    fault: Option<VerificationFault>,
    capture_delay: u64,
}

impl VerificationPlugin {
    /// A plugin that writes into a prepared output directory.
    ///
    /// `capture_delay` holds every screenshot readback open for that many
    /// further zero-time render pumps after the observer already recorded it.
    /// It is a test instrument for the reproducibility contract and is zero for
    /// every real run.
    pub fn new(output: VerifyOutput, fault: Option<VerificationFault>, capture_delay: u64) -> Self {
        Self {
            output,
            fault,
            capture_delay,
        }
    }

    /// The live run this plugin drives.
    ///
    /// A production run — no fault on the command line — gets the derived
    /// [`APP_WATCHDOG`] and [`CAPTURE_TIMEOUT`]. An injected fault may name a
    /// shorter budget of either kind so its fixture can be waited out in
    /// seconds; those overrides are the fault's own, and there is no way to
    /// reach one without asking for the fault.
    fn run(&self) -> VerificationRun {
        let mut run = VerificationRun::new(self.output.clone(), self.fault)
            .with_capture_delay(self.capture_delay);
        if let Some(watchdog) = self.fault.and_then(VerificationFault::watchdog_override) {
            run = run.with_watchdog(watchdog);
        }
        if let Some(timeout) = self
            .fault
            .and_then(VerificationFault::capture_timeout_override)
        {
            run = run.with_capture_timeout(timeout);
        }
        run
    }
}

impl Plugin for VerificationPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            FIXED_STEP_SECONDS,
        )))
        .insert_resource(ClearColor(SENTINEL_CLEAR.into()))
        .init_resource::<CaptureInbox>()
        .insert_resource(self.run())
        .add_systems(
            Update,
            advance_verification_clock.before(CellShiftSet::UpdateOperations),
        )
        .add_systems(
            Update,
            (
                configure_verification_camera,
                set_verification_camera_activity,
            )
                .chain()
                .in_set(CellShiftSet::AssetReady),
        )
        .add_systems(
            Update,
            drive_verification.in_set(CellShiftSet::VerificationProbe),
        );
    }
}

/// The one render setting the verification camera is allowed to change.
///
/// Multisampling resolves neighbouring authored fills into blended in-between
/// colours that belong to no [`PaletteRole`], which would make every palette
/// ratio a function of how many edges happened to be in shot. Everything else
/// the camera renders with — [`CEL_SHIFT_TONEMAPPING`] and
/// [`CEL_SHIFT_DEBAND_DITHER`] — is the production camera's own contract.
pub const VERIFICATION_MSAA: Msaa = Msaa::Off;

/// Turns off multisampling for the captured frames.
///
/// This is the *only* render setting verification is allowed to change on the
/// camera, and it is changed because MSAA resolves neighbouring authored fills
/// into blended in-between colours that belong to no palette role. Every other
/// display setting — [`CEL_SHIFT_TONEMAPPING`] and [`CEL_SHIFT_DEBAND_DITHER`]
/// — is the production camera's own contract, so the analyzed frame is the
/// frame the shipped game draws.
fn configure_verification_camera(
    mut commands: Commands,
    cameras: Query<Entity, (With<CellShiftCamera>, Without<VerificationCamera>)>,
) {
    for entity in &cameras {
        commands
            .entity(entity)
            .insert((VerificationCamera, VERIFICATION_MSAA));
    }
}

fn set_verification_camera_activity(
    run: Res<VerificationRun>,
    mut cameras: Query<&mut Camera, With<CellShiftCamera>>,
) {
    let active = if run.pending.is_some() {
        false
    } else if run.stage() == VerificationStage::LowResolutionCapture {
        low_resolution_camera_active(run.frame, run.resize_frame)
    } else {
        stage_camera_active(run.stage())
    };
    for mut camera in &mut cameras {
        camera.is_active = active;
    }
}

const fn low_resolution_camera_active(frame: u64, resize_frame: Option<u64>) -> bool {
    let Some(resize_frame) = resize_frame else {
        return true;
    };
    frame.saturating_sub(resize_frame) + 1 >= RESIZE_FRAMES
}

const fn stage_camera_active(stage: VerificationStage) -> bool {
    !matches!(
        stage,
        VerificationStage::SeedThreeFaults
            | VerificationStage::KeyboardJourney
            | VerificationStage::BeginRepair
            | VerificationStage::CompleteRepair
            | VerificationStage::OrbitSouthEast
            | VerificationStage::OrbitSouthWest
            | VerificationStage::OrbitNorthWest
            | VerificationStage::AnalyzeReady
            | VerificationStage::WriteReport
    )
}

/// Marks a camera that has already been configured for capture.
#[derive(Component, Clone, Copy, Debug)]
struct VerificationCamera;

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

/// A fault the verification harness injects into itself.
///
/// These exist so the failure registry's "screenshot callback lost" and
/// "watchdog expiry" rows are proven end to end rather than argued. Nothing
/// selects one unless it is asked for on the command line.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VerificationFault {
    /// Never record a completed capture, so the callback appears lost.
    DropCapture,
    /// Never leave the current stage, so the app watchdog has to fire.
    Stall,
    /// Never leave the current stage and never check the watchdog, so the
    /// parent process has to kill the child.
    Hang,
}

impl VerificationFault {
    /// Every injectable fault, in declaration order.
    pub const ALL: [Self; 3] = [Self::DropCapture, Self::Stall, Self::Hang];

    /// The active-work watchdog this fault runs with instead of the derived
    /// [`APP_WATCHDOG`], when it needs a different one.
    ///
    /// Only [`Self::Stall`] does, and only because the thing it proves — that
    /// an inactive state machine is failed with its stage name — is the same
    /// proof at twenty seconds as at forty-five, and the fixture has to be
    /// waited out in full every time the gate runs. Nothing selects a fault
    /// unless it is asked for on the command line, so a production run always
    /// gets [`APP_WATCHDOG`].
    pub const fn watchdog_override(self) -> Option<Duration> {
        match self {
            Self::Stall => Some(STALL_WATCHDOG),
            Self::DropCapture | Self::Hang => None,
        }
    }

    /// The readback budget this fault runs with instead of
    /// [`CAPTURE_TIMEOUT`], when it needs a different one.
    ///
    /// Only [`Self::DropCapture`] does. That fixture never records a readback
    /// at all, so the *only* way it can end is by waiting out the budget — and
    /// what it proves is that a lost callback fails the run naming its frame,
    /// stage, and artifact, which is the same proof at one second as at ten.
    /// Charging every gate run ten seconds of deliberate idling to re-measure
    /// a constant buys nothing. Nothing selects a fault unless it is asked for
    /// on the command line, so a production run always gets
    /// [`CAPTURE_TIMEOUT`].
    pub const fn capture_timeout_override(self) -> Option<Duration> {
        match self {
            Self::DropCapture => Some(DROP_CAPTURE_TIMEOUT),
            Self::Stall | Self::Hang => None,
        }
    }

    /// The stable command-line name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::DropCapture => "drop-capture",
            Self::Stall => "stall",
            Self::Hang => "hang",
        }
    }

    /// Parses one command-line name.
    pub fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|fault| fault.name() == value)
            .ok_or_else(|| {
                let names = Self::ALL
                    .into_iter()
                    .map(Self::name)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("unknown --verify-fault {value}; expected one of {names}")
            })
    }
}

/// What the command line asked the binary to do.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VerificationRequest {
    /// The output directory, when a verification run was requested.
    pub output: Option<PathBuf>,
    /// The fault to inject into that run.
    pub fault: Option<VerificationFault>,
    /// How many further zero-time render pumps every screenshot readback is
    /// held open for after the observer has already recorded it.
    ///
    /// This is the injected slow GPU: it changes only how many pumps a capture
    /// costs, which is exactly the quantity the canonical report must be
    /// independent of. It exists so that independence is provable by running
    /// the same journey twice with different values.
    pub capture_delay: Option<u64>,
    /// How many bytes the flood fixture writes to each of stdout and stderr
    /// before exiting successfully, when one was requested.
    pub flood: Option<u64>,
    /// The image to measure, when a measurement was requested.
    pub measure: Option<PathBuf>,
    /// What the operator declared that image to be.
    pub measure_source: MeasureSource,
}

/// Largest flood the fixture will produce, so a typo cannot fill a disk.
pub const FLOOD_LIMIT_BYTES: u64 = 64 * 1024 * 1024;

/// One line the flood fixture writes, including its newline.
const FLOOD_LINE: &str = "the parent must drain this pipe while the child is still running\n";

/// Writes `bytes` to each of stdout and stderr, interleaved, then returns.
///
/// This exists for exactly one contract: a parent that waits for a child
/// before reading its pipes deadlocks the moment the child writes more than
/// one pipe buffer, and a parent that reads one pipe to the end before the
/// other deadlocks as soon as the child fills the one it is not reading. The
/// fixture writes far more than any platform's pipe capacity on *both*
/// streams, so a parent that gets this wrong hangs until its watchdog instead
/// of passing quietly.
pub fn run_flood(bytes: u64) -> std::process::ExitCode {
    use std::io::Write;

    let capped = bytes.min(FLOOD_LIMIT_BYTES);
    let line = FLOOD_LINE.as_bytes();
    let lines = capped.div_ceil(line.len() as u64);
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    for _ in 0..lines {
        if out.write_all(line).is_err() || err.write_all(line).is_err() {
            return std::process::ExitCode::from(3);
        }
    }
    if out.flush().is_err() || err.flush().is_err() {
        return std::process::ExitCode::from(3);
    }
    std::process::ExitCode::SUCCESS
}

/// How many bytes [`run_flood`] really writes to each stream for a request.
pub fn flood_bytes(requested: u64) -> u64 {
    let line = FLOOD_LINE.len() as u64;
    requested.min(FLOOD_LIMIT_BYTES).div_ceil(line) * line
}

/// Parses the supported flags.
///
/// Returns the requested output directory, or `None` when the binary was asked
/// to just play the game. Every other shape of argument is a usage error.
pub fn parse_verification_args(
    arguments: impl IntoIterator<Item = String>,
) -> Result<VerificationRequest, String> {
    const OUTPUT: &str = "--verify-output";
    const FAULT: &str = "--verify-fault";
    const DELAY: &str = "--verify-capture-delay";
    const FLOOD: &str = "--verify-flood";
    const MEASURE: &str = "--measure";
    const REFERENCE: &str = "--reference";
    const FLAGS: [&str; 6] = [OUTPUT, FAULT, DELAY, FLOOD, MEASURE, REFERENCE];
    let mut arguments = arguments.into_iter().peekable();
    let mut request = VerificationRequest::default();
    while let Some(argument) = arguments.next() {
        let (flag, value) = match argument.split_once('=') {
            Some((flag, value)) => (flag.to_owned(), Some(value.to_owned())),
            None => (argument.clone(), None),
        };
        match flag.as_str() {
            OUTPUT => {
                let value =
                    option_value(&mut arguments, value, OUTPUT, "a directory path", &FLAGS)?;
                if request.output.is_some() {
                    return Err(format!("{OUTPUT} was given more than once"));
                }
                request.output = Some(PathBuf::from(value));
            }
            MEASURE => {
                let value = option_value(&mut arguments, value, MEASURE, "an image path", &FLAGS)?;
                if request.measure.is_some() {
                    return Err(format!("{MEASURE} was given more than once"));
                }
                request.measure = Some(PathBuf::from(value));
            }
            REFERENCE => {
                if value.is_some() {
                    return Err(format!("{REFERENCE} takes no value"));
                }
                if request.measure_source == MeasureSource::Reference {
                    return Err(format!("{REFERENCE} was given more than once"));
                }
                request.measure_source = MeasureSource::Reference;
            }
            FAULT => {
                let value = option_value(&mut arguments, value, FAULT, "a fault name", &FLAGS)?;
                if request.fault.is_some() {
                    return Err(format!("{FAULT} was given more than once"));
                }
                request.fault = Some(VerificationFault::parse(&value)?);
            }
            DELAY => {
                let value = option_value(&mut arguments, value, DELAY, "a pump count", &FLAGS)?;
                if request.capture_delay.is_some() {
                    return Err(format!("{DELAY} was given more than once"));
                }
                let pumps = value
                    .parse::<u64>()
                    .map_err(|_| format!("{DELAY} requires a pump count, got {value}"))?;
                if pumps > CAPTURE_DELAY_LIMIT {
                    return Err(format!(
                        "{DELAY} accepts at most {CAPTURE_DELAY_LIMIT} pumps, got {pumps}"
                    ));
                }
                request.capture_delay = Some(pumps);
            }
            FLOOD => {
                let value = option_value(&mut arguments, value, FLOOD, "a byte count", &FLAGS)?;
                if request.flood.is_some() {
                    return Err(format!("{FLOOD} was given more than once"));
                }
                let bytes = value
                    .parse::<u64>()
                    .map_err(|_| format!("{FLOOD} requires a byte count, got {value}"))?;
                if bytes == 0 {
                    return Err(format!("{FLOOD} requires a positive byte count"));
                }
                request.flood = Some(bytes);
            }
            _ => {
                return Err(format!(
                    "unknown argument {argument}; usage: midcreek-cs-1 [{OUTPUT} <directory>] [{FAULT} <fault>] [{DELAY} <pumps>] [{FLOOD} <bytes>]"
                ));
            }
        }
    }
    if request.fault.is_some() && request.output.is_none() {
        return Err(format!("{FAULT} only applies to a {OUTPUT} run"));
    }
    if request.capture_delay.is_some() && request.output.is_none() {
        return Err(format!("{DELAY} only applies to a {OUTPUT} run"));
    }
    // The delay holds a readback open for further pumps *after the observer
    // recorded it*, and `drop-capture` is defined as never recording one. The
    // combination therefore asks for a delay that can never be applied, and
    // silently produces an ordinary lost-callback run under a name that
    // suggests something else was measured.
    if request.capture_delay.is_some() && request.fault == Some(VerificationFault::DropCapture) {
        return Err(format!(
            "{DELAY} needs a recorded readback, and {FAULT} {} never records one",
            VerificationFault::DropCapture.name()
        ));
    }
    if request.flood.is_some()
        && (request.output.is_some() || request.fault.is_some() || request.capture_delay.is_some())
    {
        return Err(format!("{FLOOD} is a fixture and runs on its own"));
    }
    // Silently ignoring a flag that changes what gets reported would let a
    // mistyped command look like it worked.
    if request.measure_source == MeasureSource::Reference && request.measure.is_none() {
        return Err(format!("{REFERENCE} only means something with {MEASURE}"));
    }
    if request.measure.is_some()
        && (request.output.is_some() || request.flood.is_some() || request.fault.is_some())
    {
        return Err(format!("{MEASURE} reads one image and runs on its own"));
    }
    Ok(request)
}

fn option_value(
    arguments: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    inline: Option<String>,
    flag: &str,
    value_name: &str,
    flags: &[&str],
) -> Result<String, String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    let Some(next) = arguments.peek() else {
        return Err(format!("{flag} requires {value_name}"));
    };
    if flags.contains(&next.as_str()) {
        return Err(format!("{flag} requires {value_name}"));
    }
    Ok(arguments
        .next()
        .expect("peek confirmed a positional value is present"))
}

/// Largest readback delay the harness will inject.
///
/// A capture is bounded by [`CAPTURE_TIMEOUT`] whatever this says, so the limit
/// exists to turn a typo into a usage error rather than a timed-out run.
pub const CAPTURE_DELAY_LIMIT: u64 = 600;

/// One mandatory contract a frame did not meet.
#[derive(Clone, Debug, PartialEq)]
pub struct MetricFailure {
    /// The frame file the failure belongs to.
    pub frame: String,
    /// The metric that failed.
    pub metric: String,
    /// The measured value.
    pub value: f64,
    /// The bound it had to satisfy.
    pub expected: String,
}

impl fmt::Display for MetricFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {} was {:.6}, expected {}",
            self.frame, self.metric, self.value, self.expected
        )
    }
}

fn failure(frame: &str, metric: &str, value: f64, expected: impl Into<String>) -> MetricFailure {
    MetricFailure {
        frame: frame.to_owned(),
        metric: metric.to_owned(),
        value,
        expected: expected.into(),
    }
}

/// The stable region name of one rack's badge.
pub fn badge_region(rack: usize) -> String {
    format!("badge-{rack}")
}

/// The stable region name of one HUD panel.
pub fn hud_region(panel: &str) -> String {
    format!("hud-{panel}")
}

/// The stable region name of one projected equipment segment.
pub fn equipment_region(id: &str, segment: usize) -> String {
    format!("equipment-{id}-{segment:02}")
}

/// Every region one frame's mandatory contracts measure, derived from the
/// canonical report rather than from the image.
pub fn frame_regions(facts: &FrameFacts) -> BTreeMap<String, PixelRect> {
    let mut regions = BTreeMap::new();
    regions.insert(
        WORKER_REGION.to_owned(),
        PixelRect::snap(facts.worker_crop, facts.width, facts.height),
    );
    for (name, rect) in &facts.hud_panels {
        regions.insert(
            hud_region(name),
            PixelRect::snap(*rect, facts.width, facts.height),
        );
    }
    for badge in &facts.badges {
        if badge.visibility != "shown" {
            continue;
        }
        if let Some(rect) = badge.rect {
            regions.insert(
                badge_region(badge.rack),
                PixelRect::snap(rect, facts.width, facts.height),
            );
        }
    }
    for prop in &facts.equipment {
        for (segment, rect) in prop.regions.iter().enumerate() {
            regions.insert(
                equipment_region(&prop.id, segment),
                PixelRect::snap(*rect, facts.width, facts.height),
            );
        }
    }
    regions
}

/// The palette role one badge kind is filled with.
fn badge_role(kind: &str) -> Option<PaletteRole> {
    match kind {
        "fault" => Some(PaletteRole::FaultRed),
        "repairing" => Some(PaletteRole::WorkerHardHat),
        "resolved" => Some(PaletteRole::HealthyGreen),
        _ => None,
    }
}

/// The palette role the queue draws one rack state in.
fn state_role_name(state: &str) -> Option<PaletteRole> {
    match state {
        "faulted" => Some(PaletteRole::FaultRed),
        "repairing" => Some(PaletteRole::WorkerHardHat),
        "resolved" => Some(PaletteRole::HealthyGreen),
        _ => None,
    }
}

/// Smallest share of a qualifying region each required equipment role group
/// must cover.
///
/// This is deliberately low and blunt. A projected segment is an axis-aligned
/// box around a diagonal solid, so even a rack cabinet that fills its segment
/// leaves the corners to the floor behind it; what the number has to separate
/// is "the equipment is drawn here" from "there is nothing here but floor",
/// and the measured margin on every real frame is recorded in the Task 8
/// report.
pub const EQUIPMENT_ROLE_MIN: f64 = 0.04;

/// One authored prop's projected regions, resolved against measured metrics.
struct EquipmentEvidence<'a> {
    facts: &'a EquipmentFacts,
    /// Every measurable region of this prop, with the share of each required
    /// role group inside it.
    measured: Vec<(String, Vec<f64>)>,
}

impl EquipmentEvidence<'_> {
    /// Whether at least one region carried every required role group.
    fn qualifies(&self) -> bool {
        self.measured
            .iter()
            .any(|(_, shares)| shares.iter().all(|share| *share >= EQUIPMENT_ROLE_MIN))
    }

    /// The best region, and the weakest group inside it, for a failure message.
    fn best(&self) -> Option<(&str, f64)> {
        self.measured
            .iter()
            .filter_map(|(name, shares)| {
                shares
                    .iter()
                    .copied()
                    .fold(None::<f64>, |low, share| {
                        Some(low.map_or(share, |value: f64| value.min(share)))
                    })
                    .map(|weakest| (name.as_str(), weakest))
            })
            .max_by(|left, right| left.1.total_cmp(&right.1))
    }
}

/// Resolves one category's projected regions against the measured frame.
fn equipment_evidence<'a>(
    facts: &'a FrameFacts,
    metrics: &FrameMetrics,
    category: EquipmentCategory,
) -> Vec<EquipmentEvidence<'a>> {
    facts
        .equipment
        .iter()
        .filter(|prop| prop.category == category.name())
        .map(|prop| {
            let measured = prop
                .regions
                .iter()
                .enumerate()
                .filter_map(|(segment, _)| {
                    let name = equipment_region(&prop.id, segment);
                    let region = metrics.region(&name)?;
                    let shares = category
                        .role_groups()
                        .iter()
                        .map(|group| group.iter().map(|role| region.near(*role)).sum::<f64>())
                        .collect::<Vec<_>>();
                    Some((name, shares))
                })
                .collect();
            EquipmentEvidence {
                facts: prop,
                measured,
            }
        })
        .collect()
}

/// Checks that every authored equipment family really drew itself into the
/// screen rectangle its own 3D bounds project onto.
///
/// A category whose every prop projects clear of the viewport is skipped, and
/// only then: the report carries the unclipped projected bounds of each prop,
/// so every skip is auditable, and `rack-rows` additionally has to be present
/// in full whatever the camera is looking at.
fn evaluate_equipment(
    name: &str,
    facts: &FrameFacts,
    metrics: &FrameMetrics,
    failures: &mut Vec<MetricFailure>,
) {
    for category in EquipmentCategory::ALL {
        let evidence = equipment_evidence(facts, metrics, category);
        let expected = if category == EquipmentCategory::RackRows {
            RACK_ROW_X.len()
        } else {
            1
        };
        if evidence.len() < expected {
            failures.push(failure(
                name,
                &format!("equipment-{}-authored", category.name()),
                evidence.len() as f64,
                format!(
                    "at least {expected} projected {} prop(s); the hall spawned {}",
                    category.name(),
                    evidence.len()
                ),
            ));
            continue;
        }

        let on_screen = evidence
            .iter()
            .filter(|prop| prop.facts.on_screen)
            .collect::<Vec<_>>();
        if on_screen.is_empty() {
            // Every prop of this family projects clear of the viewport, which
            // is the one documented exclusion. The projected bounds in the
            // report are what prove it.
            continue;
        }

        let measurable = on_screen
            .iter()
            .filter(|prop| !prop.measured.is_empty())
            .copied()
            .collect::<Vec<_>>();
        if measurable.is_empty() {
            failures.push(failure(
                name,
                &format!("equipment-{}-measurable", category.name()),
                0.0,
                format!(
                    "at least one {} region of {EQUIPMENT_REGION_MIN_PIXELS} pixels on screen",
                    category.name()
                ),
            ));
            continue;
        }

        if !measurable.iter().any(|prop| prop.qualifies()) {
            let (region, weakest) = measurable
                .iter()
                .filter_map(|prop| prop.best())
                .max_by(|left, right| left.1.total_cmp(&right.1))
                .unwrap_or(("none", 0.0));
            failures.push(failure(
                name,
                &format!("equipment-{}", category.name()),
                weakest,
                format!(
                    "at least {EQUIPMENT_ROLE_MIN} of every authored role group in one projected region; the best was {region}"
                ),
            ));
        }

        if !category.requires_every_prop() {
            continue;
        }
        // Every prop of this family carries its own evidence, so the loop runs
        // over everything that projects into the viewport rather than over
        // whatever happened to be measurable. An empty region set is not proof
        // that a prop is out of shot — `on_screen` is the projection's own
        // answer to that — so a prop that is in shot and measured nothing is a
        // failure in its own right, not a prop to quietly skip. Skipping it is
        // exactly how a rack row that stopped projecting a usable region would
        // have vanished from the contract while every other row kept it green.
        for prop in on_screen {
            if prop.measured.is_empty() {
                failures.push(failure(
                    name,
                    &format!("equipment-{}-unmeasured", prop.facts.id),
                    0.0,
                    format!(
                        "{} projects into the viewport, so it must keep at least one region of \
                         {EQUIPMENT_REGION_MIN_PIXELS} pixels to be judged on",
                        prop.facts.id
                    ),
                ));
                continue;
            }
            if prop.qualifies() {
                continue;
            }
            let (region, weakest) = prop.best().unwrap_or(("none", 0.0));
            failures.push(failure(
                name,
                &format!("equipment-{}", prop.facts.id),
                weakest,
                format!(
                    "at least {EQUIPMENT_ROLE_MIN} of every authored role group in one projected region; the best was {region}"
                ),
            ));
        }
    }
}

/// Checks every mandatory contract one frame has to meet on its own.
#[allow(clippy::too_many_lines)]
pub fn evaluate_frame(
    frame: FrameName,
    facts: &FrameFacts,
    metrics: &FrameMetrics,
    reference: &FrameMetrics,
) -> Vec<MetricFailure> {
    let name = frame.file_name();
    let mut failures = Vec::new();
    let (width, height) = frame.size();

    // Every bound below is read from the committed fidelity contract rather
    // than from a constant in `metrics`, which measures images and holds no
    // policy. They are bound to locals so the failure messages can keep naming
    // the number they were judged against inline.
    let bounds = crate::reference::bounds();
    let sentinel_max = bounds.sentinel.max();
    let luminance_range = bounds.luminance.range();
    let luminance_reference_tolerance = bounds.luminance_reference_tolerance.max();
    let palette_min = bounds.palette.min();
    let floor_min = bounds.floor.min();
    let rack_min = bounds.rack.min();
    let yellow_min = bounds.yellow.min();
    let ink_range = bounds.ink.range();
    let diagonal_band_min = bounds.diagonal_band.min();
    let histogram_max = bounds.histogram.max();
    let edge_density_range = bounds.edge_density.range();
    let worker_role_min = bounds.worker_role.min();
    let badge_role_min = bounds.badge_role.min();
    let hud_state_min = bounds.hud_state.min();

    if metrics.width != width || metrics.height != height {
        failures.push(failure(
            name,
            "dimensions",
            f64::from(metrics.width),
            format!("{width}x{height}, got {}x{}", metrics.width, metrics.height),
        ));
        return failures;
    }
    if facts.path != name {
        failures.push(failure(name, "artifact-name", 0.0, name.to_owned()));
    }

    if metrics.sentinel_ratio > sentinel_max {
        failures.push(failure(
            name,
            "sentinel-ratio",
            metrics.sentinel_ratio,
            format!("at most {sentinel_max}; the ground quadrilateral left the rendered apron"),
        ));
    }

    let luminance = metrics.mean_linear_luminance;
    if luminance < luminance_range.0 || luminance > luminance_range.1 {
        failures.push(failure(
            name,
            "mean-linear-luminance",
            luminance,
            format!("between {} and {}", luminance_range.0, luminance_range.1),
        ));
    }
    let drift = (luminance - reference.mean_linear_luminance).abs();
    if drift > luminance_reference_tolerance {
        failures.push(failure(
            name,
            "luminance-drift-from-key-art",
            drift,
            format!("at most {luminance_reference_tolerance}"),
        ));
    }

    if metrics.palette_ratio < palette_min {
        failures.push(failure(
            name,
            "palette-ratio",
            metrics.palette_ratio,
            format!("at least {palette_min}"),
        ));
    }

    let floor = metrics.nearest_of(&[PaletteRole::FloorLight, PaletteRole::FloorShadow]);
    if floor < floor_min {
        failures.push(failure(
            name,
            "floor-ratio",
            floor,
            format!("at least {floor_min}"),
        ));
    }
    let rack = metrics.nearest_of(&[PaletteRole::RackWhite, PaletteRole::RackShadow]);
    if rack < rack_min {
        failures.push(failure(
            name,
            "rack-ratio",
            rack,
            format!("at least {rack_min}"),
        ));
    }
    let yellow = metrics.nearest(PaletteRole::SignatureYellow);
    if yellow < yellow_min {
        failures.push(failure(
            name,
            "signature-yellow-ratio",
            yellow,
            format!("at least {yellow_min}"),
        ));
    }
    let ink = metrics.nearest_of(&[PaletteRole::Ink, PaletteRole::HoseCharcoal]);
    if ink < ink_range.0 || ink > ink_range.1 {
        failures.push(failure(
            name,
            "ink-and-hose-ratio",
            ink,
            format!("between {} and {}", ink_range.0, ink_range.1),
        ));
    }

    if frame.is_settled() {
        if metrics.diagonal_band_low < diagonal_band_min {
            failures.push(failure(
                name,
                "diagonal-edge-band-low",
                metrics.diagonal_band_low,
                format!("at least {diagonal_band_min}"),
            ));
        }
        if metrics.diagonal_band_high < diagonal_band_min {
            failures.push(failure(
                name,
                "diagonal-edge-band-high",
                metrics.diagonal_band_high,
                format!("at least {diagonal_band_min}"),
            ));
        }
    }

    if frame.is_center_settled() {
        evaluate_equipment(name, facts, metrics, &mut failures);
    }

    let distance = metrics.histogram_distance(reference);
    if distance > histogram_max {
        failures.push(failure(
            name,
            "key-art-histogram-distance",
            distance,
            format!("at most {histogram_max}"),
        ));
    }
    let density = metrics.edge_density / reference.edge_density;
    if density < edge_density_range.0 || density > edge_density_range.1 {
        failures.push(failure(
            name,
            "edge-density-vs-key-art",
            density,
            format!(
                "between {} and {} times the key art",
                edge_density_range.0, edge_density_range.1
            ),
        ));
    }

    match metrics.region(WORKER_REGION) {
        None => failures.push(failure(name, "worker-crop", 0.0, "a measured crop")),
        Some(worker) => {
            for role in [PaletteRole::WorkerHardHat, PaletteRole::WorkerHiVis] {
                let share = worker.near(role);
                if share < worker_role_min {
                    failures.push(failure(
                        name,
                        &format!("worker-crop-{role:?}"),
                        share,
                        format!("at least {worker_role_min} of the projected worker crop"),
                    ));
                }
            }
        }
    }

    for badge in &facts.badges {
        if badge.visibility != "shown" {
            continue;
        }
        let Some(role) = badge_role(&badge.kind) else {
            continue;
        };
        let region = badge_region(badge.rack);
        match metrics.region(&region) {
            None => failures.push(failure(name, &region, 0.0, "a measured badge rectangle")),
            Some(measured) => {
                let share = measured.near(role);
                if share < badge_role_min {
                    failures.push(failure(
                        name,
                        &format!("{region}-{role:?}"),
                        share,
                        format!("at least {badge_role_min} of the badge rectangle"),
                    ));
                }
            }
        }
    }

    for (panel, rect) in &facts.hud_panels {
        if !rect.is_inside((width, height)) {
            failures.push(failure(
                name,
                &format!("hud-{panel}-on-screen"),
                rect.x,
                format!("inside {width}x{height}, got {rect:?}"),
            ));
        }
    }
    if let Some(queue) = metrics.region(&hud_region("queue")) {
        let mut states = facts
            .hud_rows
            .iter()
            .filter_map(|row| state_role_name(&row.state))
            .collect::<Vec<_>>();
        states.sort_unstable();
        states.dedup();
        for role in states {
            let share = queue.near(role);
            if share < hud_state_min {
                failures.push(failure(
                    name,
                    &format!("hud-queue-{role:?}"),
                    share,
                    format!("at least {hud_state_min} of the queue panel"),
                ));
            }
        }
    } else if !facts.hud_rows.is_empty() {
        failures.push(failure(name, "hud-queue", 0.0, "a measured queue panel"));
    }

    failures
}

// ---------------------------------------------------------------------------
// Cross-frame contracts
// ---------------------------------------------------------------------------

/// Whether one pixel belongs to the technician's own palette.
fn is_worker_pixel(red: u8, green: u8, blue: u8) -> bool {
    const WORKER_ROLES: [PaletteRole; 6] = [
        PaletteRole::WorkerHardHat,
        PaletteRole::WorkerHiVis,
        PaletteRole::WorkerSlate,
        PaletteRole::WorkerTrousers,
        PaletteRole::WorkerBoots,
        PaletteRole::WorkerSkin,
    ];
    let palette = palette_table();
    WORKER_ROLES.iter().any(|role| {
        let slot = PaletteRole::ALL
            .iter()
            .position(|candidate| candidate == role)
            .expect("every role is in the palette");
        squared_distance(red, green, blue, palette[slot]) <= PALETTE_TOLERANCE * PALETTE_TOLERANCE
    })
}

/// Samples one crop into a fixed grid of worker-colour occupancy.
///
/// Sampling in the crop's own normalized coordinates makes the comparison
/// independent of where the technician stood, so what it measures is the pose
/// rather than the position.
fn worker_mask(image: &RgbImage, crop: PixelRect, cells: u32) -> Vec<bool> {
    let mut mask = vec![false; (cells * cells) as usize];
    if crop.width == 0 || crop.height == 0 {
        return mask;
    }
    for row in 0..cells {
        for column in 0..cells {
            let x = crop.x + (column * crop.width) / cells;
            let y = crop.y + (row * crop.height) / cells;
            if x >= image.width() || y >= image.height() {
                continue;
            }
            let pixel = image.get_pixel(x, y).0;
            mask[(row * cells + column) as usize] = is_worker_pixel(pixel[0], pixel[1], pixel[2]);
        }
    }
    mask
}

/// How different two poses are, as the symmetric difference of their worker
/// masks over the number of sampled cells.
pub fn clip_difference(
    left: &RgbImage,
    left_crop: PixelRect,
    right: &RgbImage,
    right_crop: PixelRect,
) -> f64 {
    const CELLS: u32 = 48;
    let a = worker_mask(left, left_crop, CELLS);
    let b = worker_mask(right, right_crop, CELLS);
    let differing = a
        .iter()
        .zip(b.iter())
        .filter(|(left, right)| left != right)
        .count();
    differing as f64 / a.len() as f64
}

/// Share of pixels outside `exclude` that differ between two frames.
pub fn outside_crop_change(left: &RgbImage, right: &RgbImage, exclude: &[PixelRect]) -> f64 {
    if left.dimensions() != right.dimensions() {
        return 1.0;
    }
    let mut counted = 0u64;
    let mut changed = 0u64;
    for y in 0..left.height() {
        for x in 0..left.width() {
            if exclude.iter().any(|rect| rect.contains(x, y)) {
                continue;
            }
            counted += 1;
            if left.get_pixel(x, y) != right.get_pixel(x, y) {
                changed += 1;
            }
        }
    }
    if counted == 0 {
        return 0.0;
    }
    changed as f64 / counted as f64
}

// ---------------------------------------------------------------------------
// Generated fixtures
// ---------------------------------------------------------------------------

fn rgb(role: PaletteRole) -> image::Rgb<u8> {
    let colour = role.color().to_u8_array_no_alpha();
    image::Rgb([colour[0], colour[1], colour[2]])
}

fn fill(image: &mut RgbImage, rect: PixelRect, colour: image::Rgb<u8>) {
    for y in rect.y..(rect.y + rect.height).min(image.height()) {
        for x in rect.x..(rect.x + rect.width).min(image.width()) {
            image.put_pixel(x, y, colour);
        }
    }
}

/// The synthetic frame the negative fixtures are cut from.
///
/// It is deliberately not a substitute for a real capture: nothing in the
/// rendered contract ever accepts it as a game frame. It exists so every
/// analyzer family can be proven to reject its target without a GPU, by
/// showing the analyzer accepts this frame and rejects the mutation of it.
pub fn synthetic_frame(width: u32, height: u32) -> RgbImage {
    let mut image = RgbImage::from_pixel(width, height, rgb(PaletteRole::FloorLight));
    for y in 0..height {
        for x in 0..width {
            // Four-metre bays, then both diagonal seam families at the same
            // 40 and 140 degrees the real isometric floor projects to.
            if (x / 64 + y / 64).is_multiple_of(2) {
                image.put_pixel(x, y, rgb(PaletteRole::FloorShadow));
            }
            if (x + y).is_multiple_of(37) || x.abs_diff(y).is_multiple_of(41) {
                image.put_pixel(x, y, rgb(PaletteRole::Ink));
            }
        }
    }
    for row in 0..4u32 {
        fill(
            &mut image,
            PixelRect {
                x: 40,
                y: 80 + row * 130,
                width: width - 80,
                height: 46,
            },
            rgb(PaletteRole::RackShadow),
        );
        fill(
            &mut image,
            PixelRect {
                x: 40,
                y: 80 + row * 130,
                width: width - 80,
                height: 12,
            },
            rgb(PaletteRole::RackWhite),
        );
        fill(
            &mut image,
            PixelRect {
                x: 40,
                y: 126 + row * 130,
                width: width - 80,
                height: 6,
            },
            rgb(PaletteRole::SignatureYellow),
        );
    }
    fill(
        &mut image,
        synthetic_worker_crop(),
        rgb(PaletteRole::WorkerSlate),
    );
    fill(
        &mut image,
        PixelRect {
            x: synthetic_worker_crop().x + 6,
            y: synthetic_worker_crop().y + 4,
            width: 28,
            height: 14,
        },
        rgb(PaletteRole::WorkerHardHat),
    );
    fill(
        &mut image,
        PixelRect {
            x: synthetic_worker_crop().x + 4,
            y: synthetic_worker_crop().y + 26,
            width: 32,
            height: 30,
        },
        rgb(PaletteRole::WorkerHiVis),
    );
    for (rect, role) in synthetic_badges() {
        fill(&mut image, rect, rgb(role));
    }
    fill(&mut image, synthetic_hud_panel(), rgb(PaletteRole::Ink));
    fill(
        &mut image,
        PixelRect {
            x: synthetic_hud_panel().x + 8,
            y: synthetic_hud_panel().y + 8,
            width: 24,
            height: 24,
        },
        rgb(PaletteRole::FaultRed),
    );
    image
}

/// The projected worker crop of [`synthetic_frame`].
pub fn synthetic_worker_crop() -> PixelRect {
    PixelRect {
        x: 600,
        y: 300,
        width: 40,
        height: 90,
    }
}

/// The drawn badges of [`synthetic_frame`], with the role each is filled with.
pub fn synthetic_badges() -> Vec<(PixelRect, PaletteRole)> {
    vec![
        (
            PixelRect {
                x: 700,
                y: 240,
                width: 34,
                height: 22,
            },
            PaletteRole::FaultRed,
        ),
        (
            PixelRect {
                x: 780,
                y: 240,
                width: 34,
                height: 22,
            },
            PaletteRole::WorkerHardHat,
        ),
        (
            PixelRect {
                x: 860,
                y: 240,
                width: 34,
                height: 22,
            },
            PaletteRole::HealthyGreen,
        ),
    ]
}

/// The HUD queue panel of [`synthetic_frame`].
pub fn synthetic_hud_panel() -> PixelRect {
    PixelRect {
        x: 16,
        y: 16,
        width: 216,
        height: 96,
    }
}

/// An all-black frame: no palette, no luminance, no structure at all.
pub fn black_fixture(width: u32, height: u32) -> RgbImage {
    RgbImage::from_pixel(width, height, image::Rgb([0, 0, 0]))
}

/// A smooth gradient with deterministic dither: bright and busy, but nothing
/// in it is an approved palette colour.
pub fn gradient_noise_fixture(width: u32, height: u32) -> RgbImage {
    let mut image = RgbImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let jitter = ((x * 7919 + y * 104_729) % 61) as u8;
            let red = ((x * 255) / width.max(1)) as u8;
            let green = ((y * 255) / height.max(1)) as u8;
            let blue = red / 2 + green / 2;
            image.put_pixel(
                x,
                y,
                image::Rgb([
                    red.wrapping_add(jitter),
                    green.wrapping_add(jitter / 2),
                    blue.wrapping_add(jitter / 3),
                ]),
            );
        }
    }
    image
}

/// A good frame with a magenta sentinel border, as if the camera had rendered
/// past the apron.
pub fn magenta_border_fixture(width: u32, height: u32) -> RgbImage {
    let mut image = synthetic_frame(width, height);
    let band = height / 20;
    fill(
        &mut image,
        PixelRect {
            x: 0,
            y: 0,
            width,
            height: band,
        },
        image::Rgb([255, 0, 255]),
    );
    image
}

/// Palette colours arranged only in horizontal and vertical bands, so no
/// diagonal edge family exists at all.
pub fn axis_aligned_fixture(width: u32, height: u32) -> RgbImage {
    let mut image = RgbImage::from_pixel(width, height, rgb(PaletteRole::FloorLight));
    for y in 0..height {
        for x in 0..width {
            if (y / 24).is_multiple_of(2) {
                image.put_pixel(x, y, rgb(PaletteRole::FloorShadow));
            }
            if x.is_multiple_of(32) || y.is_multiple_of(32) {
                image.put_pixel(x, y, rgb(PaletteRole::Ink));
            }
        }
    }
    fill(
        &mut image,
        synthetic_worker_crop(),
        rgb(PaletteRole::WorkerHiVis),
    );
    image
}

/// A good frame with every worker colour painted out of the worker crop.
pub fn missing_worker_fixture(base: &RgbImage, crop: PixelRect) -> RgbImage {
    let mut image = base.clone();
    fill(&mut image, crop, rgb(PaletteRole::FloorLight));
    image
}

/// A good frame with every badge painted out.
pub fn missing_badge_fixture(base: &RgbImage, badges: &[PixelRect]) -> RgbImage {
    let mut image = base.clone();
    for rect in badges {
        fill(&mut image, *rect, rgb(PaletteRole::FloorLight));
    }
    image
}

/// A good frame with the HUD panels painted out.
pub fn blank_hud_fixture(base: &RgbImage, panels: &[PixelRect]) -> RgbImage {
    let mut image = base.clone();
    for rect in panels {
        fill(&mut image, *rect, rgb(PaletteRole::FloorLight));
    }
    image
}

// ---------------------------------------------------------------------------
// Stage driver tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::{
        design::PropId,
        operations::{RackEntry, TicketId},
    };

    #[test]
    fn fault_seeding_fast_forwards_only_the_unelapsed_interval() {
        let fixed = Duration::from_secs_f64(FIXED_STEP_SECONDS);
        assert_eq!(remaining_fault_step(Duration::ZERO), FAULT_INTERVAL);
        assert_eq!(remaining_fault_step(fixed * 2), FAULT_INTERVAL - fixed * 2);
        assert_eq!(remaining_fault_step(FAULT_INTERVAL), fixed);
        assert_eq!(verification_ticks(fixed), 1);
        assert_eq!(verification_ticks(Duration::from_millis(250)), 15);
        assert_eq!(verification_ticks(FAULT_INTERVAL), 240);
    }

    #[test]
    fn capture_pumps_are_throttled_but_remain_inside_the_readback_budget() {
        assert!(CAPTURE_PUMP_INTERVAL > Duration::ZERO);
        assert!(CAPTURE_PUMP_INTERVAL < CAPTURE_TIMEOUT);
        assert!(CAPTURE_PUMP_INTERVAL <= DROP_CAPTURE_TIMEOUT / 4);
    }

    #[test]
    fn resized_capture_has_its_own_timeout_without_weakening_fault_fixtures() {
        let scratch = Scratch::new();
        let run = VerificationRun::new(scratch.output(), None);
        assert_eq!(
            run.capture_timeout_for(FrameName::LowResolutionQueue),
            LOW_RESOLUTION_CAPTURE_TIMEOUT
        );
        assert_eq!(
            run.capture_timeout_for(FrameName::HealthyCenterNorthEast),
            CAPTURE_TIMEOUT
        );

        let overridden = run.with_capture_timeout(DROP_CAPTURE_TIMEOUT);
        assert_eq!(
            overridden.capture_timeout_for(FrameName::LowResolutionQueue),
            DROP_CAPTURE_TIMEOUT
        );
    }

    #[test]
    fn transition_only_stages_suppress_the_camera() {
        for stage in [
            VerificationStage::SeedThreeFaults,
            VerificationStage::KeyboardJourney,
            VerificationStage::BeginRepair,
            VerificationStage::CompleteRepair,
            VerificationStage::OrbitSouthEast,
            VerificationStage::OrbitSouthWest,
            VerificationStage::OrbitNorthWest,
            VerificationStage::AnalyzeReady,
            VerificationStage::WriteReport,
        ] {
            assert!(!stage_camera_active(stage), "{stage:?} only advances state");
        }
        for stage in [
            VerificationStage::HealthyCapture,
            VerificationStage::FaultQueueCapture,
            VerificationStage::WalkCapture,
            VerificationStage::RepairCapture,
            VerificationStage::ResolvedCapture,
            VerificationStage::SettledSouthEastCapture,
            VerificationStage::SettledSouthWestCapture,
            VerificationStage::SettledNorthWestCapture,
            VerificationStage::MidOrbitCapture,
            VerificationStage::CornerProbes,
            VerificationStage::LowResolutionCapture,
        ] {
            assert!(stage_camera_active(stage), "{stage:?} owns captured pixels");
        }
    }

    #[test]
    fn low_resolution_camera_reactivates_on_the_capture_update() {
        assert!(low_resolution_camera_active(100, None));
        assert!(!low_resolution_camera_active(143, Some(100)));
        assert!(low_resolution_camera_active(144, Some(100)));
    }

    /// A scratch output directory, removed when the test that owns it ends.
    ///
    /// The unit tests prepare real directories because [`VerifyOutput`] is the
    /// only way to name an artifact path, and a few of them write real PNGs
    /// into one. Holding the guard for the length of the test is what stops
    /// the suite scattering directories through the system temp directory,
    /// one per run, forever.
    ///
    /// Both ends of that fail closed. A stale directory that cannot be cleared
    /// means the fixture is about to run against somebody else's leftovers, and
    /// a scratch directory that cannot be removed means the leak this guard
    /// exists to prevent has happened anyway; a discarded `Result` would report
    /// neither.
    struct Scratch {
        output: VerifyOutput,
    }

    /// Removes `path` and everything under it, treating "it was not there" as
    /// success and every other failure as a failure.
    ///
    /// This is split out so the distinction is testable: the whole point is
    /// that `NotFound` is the one error a pre-clean may swallow.
    fn clear_scratch(path: &Path) -> io::Result<()> {
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("midcreek-stage-{}-{unique}", std::process::id()));
            clear_scratch(&path).unwrap_or_else(|error| {
                panic!(
                    "a stale scratch directory {} could not be cleared, so this fixture would \
                     have run against somebody else's leftovers: {error}",
                    path.display()
                )
            });
            Self {
                output: VerifyOutput::prepare(&path).expect("the scratch directory must prepare"),
            }
        }

        fn output(&self) -> VerifyOutput {
            self.output.clone()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let root = self.output.root();
            let Err(error) = clear_scratch(root) else {
                return;
            };
            // Panicking inside a `drop` that is already unwinding aborts the
            // process and would bury the real failure, so the teardown failure
            // is reported the only other way it can be.
            assert!(
                std::thread::panicking(),
                "the scratch directory {} could not be removed: {error}",
                root.display()
            );
            eprintln!(
                "the scratch directory {} could not be removed while unwinding: {error}",
                root.display()
            );
        }
    }

    /// The two racks the journey reasons about, laid out like the real hall.
    fn journey_roster() -> RackRoster {
        RackRoster::from_entries(
            RACK_ROW_X
                .into_iter()
                .enumerate()
                .map(|(rack, x)| RackEntry {
                    rack,
                    id: PropId::new(format!("rack-row-{:02}", rack + 1)),
                    entity: Entity::PLACEHOLDER,
                    center: Vec2::new(x, 0.0),
                    half_extents: Vec2::new(0.6, 4.0),
                })
                .collect(),
        )
    }

    /// The exact ground point [`VerificationStage::BeginRepair`] walks to.
    fn journey_spot(roster: &RackRoster) -> Vec2 {
        let entry = roster.get(JOURNEY_RACK).expect("the journey rack exists");
        journey_repair_spot(entry.center, entry.half_extents)
    }

    /// A snapshot of a fully booted hall, parameterised only by the state the
    /// repair hand-over actually reads.
    fn begin_repair_snapshot(
        roster: RackRoster,
        player: Vec2,
        clip: PlayerClip,
        lock: MovementLock,
        outcome: InteractionOutcome,
    ) -> Snapshot {
        let racks = roster.len();
        Snapshot {
            assets: AssetLoadState::Ready,
            hall: HallState::Ready,
            rig: PlayerRigState::Ready,
            rig_healthy: true,
            parts: required_player_parts()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            roster,
            queue: TicketQueue::default(),
            tick: 2_192,
            orbit: CameraOrbit::default(),
            lock,
            last: LastInteraction {
                outcome,
                tick: 2_100,
                presses: 2,
                started: 1,
                rejected: 1,
            },
            hud: HudReport::default(),
            player,
            clip,
            rack_states: vec![RackState::Repairing; racks],
            scheduler: (3, 0, 0, 0),
            viewport: Some(UVec2::new(
                VERIFICATION_WINDOW_WIDTH,
                VERIFICATION_WINDOW_HEIGHT,
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Capture waiting
    // -----------------------------------------------------------------------

    /// The one frame `begin_repair_snapshot`'s 960x540 viewport is sized for,
    /// so a capture test never fails on a viewport mismatch it is not about.
    const CAPTURE_TEST_FRAME: FrameName = FrameName::LowResolutionQueue;

    /// The fixed frame budget captures used to be waited out with, before the
    /// wait became conditional. It survives only as the number a callback has
    /// to beat here, because it is the number the CI renderer really missed.
    const RETIRED_CAPTURE_FRAMES: u64 = 24;

    /// A world holding exactly what an outstanding capture reads: the
    /// observers' mailbox and the manual clock the harness drives.
    fn capture_world() -> World {
        let mut world = World::new();
        world.init_resource::<CaptureInbox>();
        world.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            FIXED_STEP_SECONDS,
        )));
        world
    }

    /// A run parked on the stage that captures [`CAPTURE_TEST_FRAME`], with
    /// the scratch directory guard the caller has to keep alive.
    fn capture_run() -> (Scratch, VerificationRun) {
        let scratch = Scratch::new();
        let mut run =
            VerificationRun::new(scratch.output(), None).with_capture_timeout(CAPTURE_TIMEOUT);
        while run.stage() != VerificationStage::LowResolutionCapture {
            run.machine
                .advance()
                .expect("the documented order reaches low-resolution-capture");
        }
        (scratch, run)
    }

    /// A snapshot a capture stage is happy to photograph.
    fn capture_snapshot() -> Snapshot {
        let roster = journey_roster();
        let spot = journey_spot(&roster);
        begin_repair_snapshot(
            roster,
            spot,
            PlayerClip::Idle,
            MovementLock::default(),
            InteractionOutcome::None,
        )
    }

    /// Lands the outstanding readback exactly as the real observer does: the
    /// complete file reaches disk first, and only then is the frame recorded.
    fn land_capture(world: &mut World, run: &VerificationRun, frame: FrameName) {
        land_capture_sized(world, run, frame, frame.size());
    }

    /// The same landing, but writing a frame of an arbitrary pixel size.
    fn land_capture_sized(
        world: &mut World,
        run: &VerificationRun,
        frame: FrameName,
        size: (u32, u32),
    ) {
        RgbImage::new(size.0, size.1)
            .save(run.output.frame(frame))
            .expect("the scratch frame is writable");
        world.resource_mut::<CaptureInbox>().completed.push(frame);
    }

    /// The simulated step the manual clock is currently set to.
    fn simulated_step(world: &World) -> Duration {
        match world.resource::<TimeUpdateStrategy>() {
            TimeUpdateStrategy::ManualDuration(step) => *step,
            _ => panic!("the verification clock is always driven manually"),
        }
    }

    /// The readback is a property of the GPU, not of the journey. A callback
    /// that lands after more render pumps than the old fixed frame budget
    /// allowed is late, not lost, and the capture must still complete.
    #[test]
    fn a_capture_completes_when_the_callback_lands_long_after_the_old_frame_budget() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();

        run.frame += 1;
        assert!(
            !capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
                .expect("requesting a capture must not fail"),
            "the request frame never completes the capture"
        );

        for pump in 0..(RETIRED_CAPTURE_FRAMES * 4) {
            run.frame += 1;
            let landed = capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).unwrap_or_else(
                |reason| {
                    panic!("pump {pump} must keep waiting for the readback, it failed: {reason}")
                },
            );
            assert!(
                !landed,
                "pump {pump} must not claim a readback that never landed"
            );
        }

        land_capture(&mut world, &run, CAPTURE_TEST_FRAME);
        run.frame += 1;
        assert!(
            capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
                .expect("a landed readback completes the capture"),
            "a late callback must still complete the capture"
        );
        assert!(
            run.pending.is_none(),
            "a completed capture is no longer outstanding"
        );
    }

    /// A capture must cost the journey no simulated time at all, so the number
    /// of render pumps one machine needs can never reach the report.
    #[test]
    fn a_pending_capture_freezes_the_clock_and_the_landing_restores_the_fixed_step() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();

        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        assert_eq!(
            simulated_step(&world),
            Duration::ZERO,
            "an outstanding capture must stop simulated time"
        );

        for _ in 0..8 {
            capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("pumps do not fail");
            assert_eq!(
                simulated_step(&world),
                Duration::ZERO,
                "every pump of an outstanding capture stays frozen"
            );
        }

        land_capture(&mut world, &run, CAPTURE_TEST_FRAME);
        assert!(
            capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the readback landed"),
            "the landed readback completes the capture"
        );
        assert_eq!(
            simulated_step(&world),
            Duration::from_secs_f64(FIXED_STEP_SECONDS),
            "the frame after the readback must advance exactly one fixed step"
        );
    }

    /// Any number of zero-time pumps must leave the journey exactly where the
    /// request frame left it: same frame counters, same recorded facts.
    #[test]
    fn zero_time_pumps_leave_every_recorded_journey_fact_untouched() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();

        run.frame = 617;
        run.stage_frame = 9;
        run.observations.keys.push(KeyFacts {
            stage: run.machine.stage().name().to_owned(),
            key: key_name(REPAIR_KEY),
            state: "pressed".to_owned(),
        });
        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");

        let frame = run.frame;
        let stage_frame = run.stage_frame;
        let stage = run.stage();
        let keys = run.observations.keys.clone();
        let history = run.observations.ticket_history.clone();
        let interactions = run.observations.interactions.clone();
        let frames = run.observations.frames.clone();

        for _ in 0..(RETIRED_CAPTURE_FRAMES * 3) {
            pump_pending_capture(&mut world, &mut run)
                .expect("a pump before the budget cannot fail");
        }

        assert_eq!(run.frame, frame, "a pump is not a simulated frame");
        assert_eq!(
            run.stage_frame, stage_frame,
            "a pump does not spend the stage budget"
        );
        assert_eq!(run.stage(), stage, "a pump never moves the machine");
        assert_eq!(run.observations.keys, keys, "a pump writes no key history");
        assert_eq!(
            run.observations.ticket_history, history,
            "a pump opens no ticket"
        );
        assert_eq!(
            run.observations.interactions, interactions,
            "a pump records no interaction"
        );
        assert_eq!(
            run.observations.frames, frames,
            "a pump re-records no frame facts"
        );
    }

    /// A readback that really is lost has to say which frame, which stage, and
    /// what state the artifact was left in, or the CI log proves nothing.
    #[test]
    fn a_lost_callback_names_its_frame_stage_and_artifact_state() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();
        run.capture_timeout_override = Some(Duration::ZERO);

        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        let reason = capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
            .expect_err("a callback past its wall-clock budget is lost");

        for fact in [
            "screenshot callback",
            CAPTURE_TEST_FRAME.file_name(),
            "low-resolution-capture",
            "never written",
        ] {
            assert!(
                reason.contains(fact),
                "the lost-callback failure must name {fact}: {reason}"
            );
        }
        assert_eq!(
            simulated_step(&world),
            Duration::from_secs_f64(FIXED_STEP_SECONDS),
            "a failed capture still hands the clock back"
        );
    }

    /// The injected readback delay is what makes "different pump counts, same
    /// evidence" provable: it must hold the capture open for exactly the
    /// requested number of extra pumps and then complete normally.
    #[test]
    fn an_injected_readback_delay_holds_the_capture_open_for_exactly_its_pumps() {
        const DELAY: u64 = 7;
        let mut world = capture_world();
        let (_scratch, run) = capture_run();
        let mut run = run.with_capture_delay(DELAY);
        let state = capture_snapshot();

        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        land_capture(&mut world, &run, CAPTURE_TEST_FRAME);

        for pump in 0..DELAY {
            assert!(
                !capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
                    .expect("a delayed pump cannot fail"),
                "pump {pump} of the injected delay must still be outstanding"
            );
        }
        assert!(
            capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
                .expect("the delayed readback completes"),
            "the capture must complete on the pump after its injected delay"
        );
    }

    /// A readback that lands at the wrong resolution is a real failure the
    /// report would otherwise never mention: the recorded frame facts carry
    /// the size the frame was *asked* for, so a short surface would reach the
    /// gate as an unexplained pixel difference.
    #[test]
    fn a_capture_that_comes_back_the_wrong_size_fails_naming_both_sizes() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();
        let (width, height) = CAPTURE_TEST_FRAME.size();

        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        land_capture_sized(
            &mut world,
            &run,
            CAPTURE_TEST_FRAME,
            (width / 2, height / 2),
        );

        let reason = capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
            .expect_err("a half-resolution surface is not the contracted frame");
        assert!(
            reason.contains(&format!("{}x{}", width / 2, height / 2))
                && reason.contains(&format!("{width}x{height}")),
            "the failure must name what came back and what was needed: {reason}"
        );
    }

    // -----------------------------------------------------------------------
    // Frame facts follow the frame
    // -----------------------------------------------------------------------

    /// A failed run may not describe a photograph nobody took.
    ///
    /// The facts of a capture are measured on the request frame, long before
    /// the readback resolves. Reporting them there put a complete record of a
    /// frame — its worker crop, its HUD rectangles, its equipment projections
    /// — into the evidence of a run that never got that frame back.
    #[test]
    fn a_frame_that_never_landed_contributes_no_facts_to_the_report() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();

        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        assert!(
            run.observations.frames.is_empty(),
            "a requested capture reports nothing until it lands"
        );
        assert!(
            run.staged_facts
                .as_ref()
                .is_some_and(|(frame, _)| *frame == CAPTURE_TEST_FRAME),
            "the measured facts are staged against the frame they belong to"
        );

        let timeout = run.capture_timeout_for(CAPTURE_TEST_FRAME);
        backdate(&mut run, timeout + Duration::from_secs(1));
        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
            .expect_err("a readback past its budget is lost");
        assert!(
            run.observations.frames.is_empty(),
            "a lost callback's frame must not appear in the report: {:?}",
            run.observations.frames.keys().collect::<Vec<_>>()
        );
    }

    /// The other half: a readback that really lands files its facts under its
    /// own name, so a passing run's report is exactly what it always was.
    #[test]
    fn a_landed_frame_files_its_facts_under_its_own_name() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();

        let _ = take_capture_lasting(&mut world, &mut run, &state, Duration::from_secs(1));
        assert_eq!(
            run.observations.frames.keys().collect::<Vec<_>>(),
            vec![CAPTURE_TEST_FRAME.file_name()],
            "a landed readback reports exactly its own frame"
        );
        assert!(
            run.staged_facts.is_none(),
            "the staging slot is emptied by the frame that claimed it"
        );
    }

    /// Waiting on somebody else's readback is a checked failure in the binary
    /// CI actually runs, not a debug assertion that evaporates in release.
    #[test]
    fn a_stage_that_waits_on_another_frames_readback_fails_naming_both() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();

        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        let other = FrameName::HealthyCenterNorthEast;
        assert_ne!(other, CAPTURE_TEST_FRAME);

        let reason = capture(&mut world, &mut run, &state, other)
            .expect_err("a stage may only wait on the capture it asked for");
        assert!(
            reason.contains(other.file_name()) && reason.contains(CAPTURE_TEST_FRAME.file_name()),
            "the failure must name what was waited on and what was outstanding: {reason}"
        );
    }

    /// The staging slot and the outstanding capture have to agree, and a
    /// landed readback with nothing staged is not a success.
    #[test]
    fn a_landed_capture_with_no_staged_facts_fails_and_leaves_the_capture_outstanding() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();

        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        land_capture(&mut world, &run, CAPTURE_TEST_FRAME);
        run.staged_facts = None;

        let reason = capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
            .expect_err("a landed frame nobody measured cannot be reported");
        assert!(
            reason.contains(CAPTURE_TEST_FRAME.file_name()) && reason.contains("no staged facts"),
            "the failure must name the frame it could not describe: {reason}"
        );
        assert!(
            run.observations.frames.is_empty(),
            "nothing may be reported for a frame with no measured facts"
        );
        assert!(
            run.pending.is_some(),
            "a failure must not clear the capture as though it had succeeded"
        );
    }

    /// The same, for facts staged against a different frame.
    #[test]
    fn a_landed_capture_holding_another_frames_facts_fails_without_reporting_either() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();

        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        land_capture(&mut world, &run, CAPTURE_TEST_FRAME);
        let other = FrameName::HealthyCenterNorthEast;
        assert_ne!(other, CAPTURE_TEST_FRAME);
        let (_, facts) = run
            .staged_facts
            .take()
            .expect("the request staged this frame's facts");
        run.staged_facts = Some((other, facts));

        let reason = capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
            .expect_err("one frame's photograph may not be described by another's facts");
        assert!(
            reason.contains(CAPTURE_TEST_FRAME.file_name()) && reason.contains(other.file_name()),
            "the failure must name both frames: {reason}"
        );
        assert!(
            run.observations.frames.is_empty(),
            "neither frame may be reported"
        );
        assert!(
            run.pending.is_some(),
            "a failure must not clear the capture as though it had succeeded"
        );
    }

    // -----------------------------------------------------------------------
    // Scratch hygiene
    // -----------------------------------------------------------------------

    /// The scratch pre-clean swallows exactly one error and no others: a
    /// directory that was never there is nothing to clear, and anything else
    /// means the fixture is about to run on top of somebody else's state.
    #[test]
    fn clearing_scratch_forgives_a_missing_directory_and_nothing_else() {
        let root = std::env::temp_dir().join(format!(
            "midcreek-scratch-clear-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&root);
        clear_scratch(&root).expect("a directory that was never there is already clear");

        fs::create_dir_all(root.join("nested")).expect("the test owns this directory");
        fs::write(root.join("nested/frame.png"), b"x").expect("the test owns this file");
        clear_scratch(&root).expect("a real scratch tree clears");
        assert!(!root.exists());

        // A regular file cannot be removed as a directory, and every operating
        // system reports that as something other than `NotFound`.
        let blocker = root.with_extension("file");
        let _ = fs::remove_file(&blocker);
        fs::write(&blocker, b"not a directory").expect("the test owns this file");
        let error = clear_scratch(&blocker)
            .expect_err("a regular file is not a clearable scratch directory");
        assert_ne!(
            error.kind(),
            io::ErrorKind::NotFound,
            "this fixture only proves anything if the error is not the forgiven one: {error}"
        );
        fs::remove_file(&blocker).expect("the test owns this file");
    }

    /// The guard really does remove what it made, so the fail-closed teardown
    /// is not firing on every test.
    #[test]
    fn a_scratch_guard_removes_its_own_directory() {
        let root = {
            let scratch = Scratch::new();
            let root = scratch.output.root().to_path_buf();
            assert!(root.is_dir(), "the guard prepared a real directory");
            root
        };
        assert!(
            !root.exists(),
            "the guard must remove {} when it drops",
            root.display()
        );
    }

    // -----------------------------------------------------------------------
    // The active-work watchdog
    // -----------------------------------------------------------------------

    /// Ages both clocks by `wait`, exactly as a real readback of that length
    /// does: the run gets `wait` older, and every second of it belongs to the
    /// outstanding readback.
    ///
    /// Backdating rather than sleeping is the whole point. The quantity under
    /// test is a wall-clock duration, and a test that really slept the
    /// fourteen readbacks of a CI run would take minutes.
    fn backdate(run: &mut VerificationRun, wait: Duration) {
        run.started -= wait;
        run.pending
            .as_mut()
            .expect("a capture is outstanding")
            .requested_at = Instant::now() - wait;
    }

    /// Requests one capture, holds the readback open for `wait` of wall clock,
    /// lands it, and returns the fixture's own observation overhead.
    ///
    /// That returned duration is what makes the banking assertions exact
    /// instead of approximate. Banking records `requested_at.elapsed()`, so
    /// the banked amount is `wait` plus however long this fixture took to get
    /// from backdating to the bank — which is bounded above by the elapsed
    /// time of a window that opens strictly before the backdate and closes
    /// strictly after the bank. The PNG is written *before* the clock is
    /// backdated, so an encode that takes a fifth of a second is not mistaken
    /// for readback wait.
    #[must_use]
    fn take_capture_lasting(
        world: &mut World,
        run: &mut VerificationRun,
        state: &Snapshot,
        wait: Duration,
    ) -> Duration {
        capture(world, run, state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        land_capture(world, run, CAPTURE_TEST_FRAME);
        let observing = Instant::now();
        backdate(run, wait);
        assert!(
            capture(world, run, state, CAPTURE_TEST_FRAME).expect("the landed readback completes"),
            "a landed readback inside the capture timeout completes the capture"
        );
        observing.elapsed()
    }

    /// The first half of [`take_capture_lasting`]: a capture requested `wait`
    /// ago and still outstanding.
    fn request_capture_waiting(
        world: &mut World,
        run: &mut VerificationRun,
        state: &Snapshot,
        wait: Duration,
    ) {
        capture(world, run, state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        backdate(run, wait);
    }

    /// The regression this whole watchdog rework exists for.
    ///
    /// CI failed the real run in `keyboard-journey` with "the app watchdog
    /// expired", having captured two frames successfully: the watchdog was
    /// charging the fourteen asynchronous readbacks that [`CAPTURE_TIMEOUT`]
    /// already governs, so a merely slow renderer read as a stuck state
    /// machine. The watchdog measures *active, non-capture* wall time, so a
    /// run whose clock ran past it only because of readbacks must survive.
    #[test]
    fn capture_waiting_never_expires_the_active_work_watchdog() {
        /// Comfortably inside [`CAPTURE_TIMEOUT`], so every readback here is
        /// late rather than lost. Deriving this from [`APP_WATCHDOG`] instead
        /// silently produced a wait longer than the readback budget the moment
        /// the watchdog was raised for software adapters, which made the
        /// fixture prove the opposite of what its name claims.
        const WAIT: Duration = Duration::from_secs(10);
        const CAPTURES: u32 = APP_WATCHDOG.as_secs().div_ceil(WAIT.as_secs()) as u32 + 1;

        assert!(
            WAIT < CAPTURE_TIMEOUT,
            "a readback this fixture waits out is a lost callback, not a late one"
        );

        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();
        let waited = WAIT * CAPTURES;
        assert!(
            waited > APP_WATCHDOG,
            "the fixture has to out-run the watchdog on wall clock alone"
        );

        for _ in 0..CAPTURES {
            let _ = take_capture_lasting(&mut world, &mut run, &state, WAIT);
        }
        assert!(
            run.started.elapsed() > APP_WATCHDOG,
            "the run really has been alive longer than the watchdog: {:?}",
            run.started.elapsed()
        );
        assert!(
            !watchdog_expired(&run),
            "{CAPTURES} completed readbacks of {WAIT:?} are the capture timeout's business, \
             not the watchdog's"
        );

        // The readback that is outstanding *right now* is excluded too.
        request_capture_waiting(&mut world, &mut run, &state, WAIT);
        assert!(
            !watchdog_expired(&run),
            "the wait a capture is in the middle of is excluded while it is outstanding"
        );

        // Active time, and only active time, still expires it.
        run.started -= APP_WATCHDOG + Duration::from_secs(1);
        assert!(
            watchdog_expired(&run),
            "wall clock that is not capture waiting is exactly what the watchdog measures"
        );
    }

    /// The watchdog still has to fire: a state machine that stops moving with
    /// no capture outstanding is the stuck run it exists to name.
    #[test]
    fn active_time_past_the_watchdog_still_expires_it() {
        let (_scratch, mut run) = capture_run();
        assert!(!watchdog_expired(&run), "a fresh run has not expired");

        run.started = Instant::now() - (APP_WATCHDOG + Duration::from_secs(1));
        assert!(
            run.pending.is_none(),
            "this run never asked for a capture, so nothing is excluded"
        );
        assert!(
            watchdog_expired(&run),
            "{APP_WATCHDOG:?} of active work with no capture outstanding must expire"
        );
    }

    /// The exclusion has to be the capture's *own* wall duration: bank less
    /// and a slow renderer still creeps towards expiry, bank more and the
    /// watchdog stops measuring anything at all.
    ///
    /// The bound is measured, not guessed. Banking records the readback's
    /// `requested_at.elapsed()`, so the banked total can only exceed the
    /// simulated wait by the time this test itself spent between backdating
    /// the clock and observing the bank — which is exactly what the fixture
    /// hands back. A fixed slack constant here is what made this assertion
    /// fail on a machine whose PNG encoder took two hundred milliseconds.
    #[test]
    fn a_completed_capture_banks_exactly_its_own_wall_duration() {
        const WAIT: Duration = Duration::from_secs(6);
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();

        assert_eq!(
            run.capture_excluded,
            Duration::ZERO,
            "a run that has taken no captures has excluded nothing"
        );
        let first = take_capture_lasting(&mut world, &mut run, &state, WAIT);
        assert!(
            (WAIT..=WAIT + first).contains(&run.capture_excluded),
            "one {WAIT:?} readback must bank {WAIT:?} and no more than the {first:?} this test \
             took to look, banked {:?}",
            run.capture_excluded
        );

        let second = take_capture_lasting(&mut world, &mut run, &state, WAIT);
        assert!(
            (WAIT * 2..=WAIT * 2 + first + second).contains(&run.capture_excluded),
            "a second {WAIT:?} readback accumulates onto the first, banked {:?}",
            run.capture_excluded
        );
    }

    /// A wait is excluded continuously from request to resolution, and exactly
    /// once: the live subtraction while the capture is outstanding must not
    /// survive the banking, or the run would be credited twice for one wait.
    ///
    /// Every bound here is measured rather than guessed. The run's clock is
    /// restarted so that the only active work it can possibly have done is
    /// this test's own, and each ceiling is read strictly after the quantity
    /// it bounds.
    #[test]
    fn an_outstanding_wait_is_excluded_live_and_banked_once() {
        const WAIT: Duration = Duration::from_secs(6);
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();

        let setup = Instant::now();
        run.started = Instant::now();
        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        let waiting = Instant::now();
        backdate(&mut run, WAIT);
        let live = run.active_elapsed();
        let setup_cost = setup.elapsed();
        assert_eq!(
            run.capture_excluded,
            Duration::ZERO,
            "an outstanding wait is subtracted live, not banked early"
        );
        assert!(
            live <= setup_cost,
            "a run that has only ever waited on this capture has done no active work beyond the \
             {setup_cost:?} this test spent setting it up, it measured {live:?}"
        );

        land_capture(&mut world, &run, CAPTURE_TEST_FRAME);
        assert!(
            capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the readback landed"),
            "the landed readback completes the capture"
        );
        let banked = run.capture_excluded;
        let settled = run.active_elapsed();
        let waited = waiting.elapsed();
        assert!(
            (WAIT..=WAIT + waited).contains(&banked),
            "the resolved wait is banked once, banked {banked:?}"
        );
        assert!(
            settled <= setup_cost + waited,
            "banking must replace the live subtraction, not double it: {settled:?}"
        );
    }

    /// A capture that fails hands back the fixed step *and* leaves the
    /// watchdog coherent: its wait is banked, so the failure the run reports
    /// is the capture failure that names the frame, never a watchdog expiry
    /// that names only a stage.
    #[test]
    fn a_timed_out_capture_restores_the_clock_and_banks_its_wait() {
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();
        let wait = run.capture_timeout_for(CAPTURE_TEST_FRAME) + Duration::from_secs(1);

        run.started = Instant::now() - wait;
        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        let observing = Instant::now();
        backdate(&mut run, wait);

        let reason = capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
            .expect_err("a readback past its wall-clock budget is lost");
        let overhead = observing.elapsed();
        assert!(
            reason.contains("screenshot callback"),
            "the lost readback names itself: {reason}"
        );
        assert_eq!(
            simulated_step(&world),
            Duration::from_secs_f64(FIXED_STEP_SECONDS),
            "a failed capture still hands the clock back"
        );
        assert!(
            (wait..=wait + overhead).contains(&run.capture_excluded),
            "a lost readback's wait is still the capture timeout's, banked {:?}",
            run.capture_excluded
        );
        assert!(
            !watchdog_expired(&run),
            "the run failed on the capture timeout; the watchdog must have nothing to add"
        );
        assert!(
            run.pending
                .is_some_and(|pending| pending.charged && !pending.completed),
            "the lost capture stays outstanding, and its wait stays banked exactly once"
        );
        assert!(
            run.staged_facts.is_some(),
            "the lost frame's facts stay staged, so they never reach the report"
        );
    }

    /// A frame that comes back at the wrong size resolves the capture too, so
    /// its wait is banked on that path as well.
    #[test]
    fn a_rejected_capture_banks_its_wait_before_it_fails() {
        const WAIT: Duration = Duration::from_secs(4);
        let mut world = capture_world();
        let (_scratch, mut run) = capture_run();
        let state = capture_snapshot();
        let (width, height) = CAPTURE_TEST_FRAME.size();

        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME).expect("the request succeeds");
        land_capture_sized(
            &mut world,
            &run,
            CAPTURE_TEST_FRAME,
            (width / 2, height / 2),
        );
        let observing = Instant::now();
        backdate(&mut run, WAIT);

        capture(&mut world, &mut run, &state, CAPTURE_TEST_FRAME)
            .expect_err("a half-resolution surface is not the contracted frame");
        let overhead = observing.elapsed();
        assert!(
            (WAIT..=WAIT + overhead).contains(&run.capture_excluded),
            "a refused readback still waited, banked {:?}",
            run.capture_excluded
        );
    }

    /// A fixture's short budgets are overrides on the fault, and nothing else
    /// can reach them: a production run has no fault, so it gets the derived
    /// budgets.
    #[test]
    fn only_the_injected_faults_shorten_the_production_budgets() {
        assert_eq!(
            VerificationFault::Stall.watchdog_override(),
            Some(STALL_WATCHDOG)
        );
        assert_eq!(VerificationFault::DropCapture.watchdog_override(), None);
        assert_eq!(VerificationFault::Hang.watchdog_override(), None);
        assert_eq!(
            VerificationFault::DropCapture.capture_timeout_override(),
            Some(DROP_CAPTURE_TIMEOUT)
        );
        assert_eq!(VerificationFault::Stall.capture_timeout_override(), None);
        assert_eq!(VerificationFault::Hang.capture_timeout_override(), None);
        assert!(
            STALL_WATCHDOG < APP_WATCHDOG && DROP_CAPTURE_TIMEOUT < CAPTURE_TIMEOUT,
            "the overrides exist to be faster than production, not to weaken it"
        );

        let scratch = Scratch::new();
        let plugin = VerificationPlugin::new(scratch.output(), None, 0);
        let production = plugin.run();
        assert_eq!(
            production.watchdog, APP_WATCHDOG,
            "a run with no injected fault is a production run"
        );
        assert_eq!(
            production.capture_timeout_for(FrameName::HealthyCenterNorthEast),
            CAPTURE_TIMEOUT,
            "a run with no injected fault gets the production readback budget"
        );

        let stall =
            VerificationPlugin::new(scratch.output(), Some(VerificationFault::Stall), 0).run();
        assert_eq!(
            stall.watchdog, STALL_WATCHDOG,
            "the stall fixture runs on its own short budget"
        );
        assert_eq!(
            stall.capture_timeout_for(FrameName::HealthyCenterNorthEast),
            CAPTURE_TIMEOUT,
            "the stall fixture has no business shortening the readback budget"
        );

        let drop =
            VerificationPlugin::new(scratch.output(), Some(VerificationFault::DropCapture), 0)
                .run();
        assert_eq!(
            drop.capture_timeout_for(FrameName::HealthyCenterNorthEast),
            DROP_CAPTURE_TIMEOUT,
            "the drop fixture waits out its own short readback budget"
        );
        assert_eq!(
            drop.watchdog, APP_WATCHDOG,
            "the drop fixture has no business shortening the watchdog"
        );

        assert_eq!(
            VerificationPlugin::new(scratch.output(), Some(VerificationFault::Hang), 0)
                .run()
                .watchdog,
            APP_WATCHDOG,
            "the hang fixture disables the watchdog rather than shortening it"
        );
    }

    /// The parent's absolute cap is derived from the child's own named
    /// budgets, so a fifteenth frame moves it without anyone editing a number.
    #[test]
    fn the_parent_cap_is_derived_from_the_child_budgets() {
        assert_eq!(
            PARENT_WATCHDOG,
            APP_WATCHDOG
                + CAPTURE_TIMEOUT * (FrameName::ALL.len() as u32 - 1)
                + LOW_RESOLUTION_CAPTURE_TIMEOUT
                + LAUNCH_MARGIN,
            "active work + thirteen normal captures + the resized capture + startup and shutdown"
        );
        assert_eq!(PARENT_WATCHDOG, Duration::from_secs(865));
        assert!(
            PARENT_WATCHDOG
                > APP_WATCHDOG
                    + CAPTURE_TIMEOUT * (FrameName::ALL.len() as u32 - 1)
                    + LOW_RESOLUTION_CAPTURE_TIMEOUT,
            "the parent may never kill a child that is still inside its own budgets"
        );
    }

    /// A run parked on [`VerificationStage::BeginRepair`], plus the world and
    /// window the driver writes its real key messages into.
    fn begin_repair_run() -> (Scratch, World, Entity, VerificationRun) {
        let mut world = World::new();
        world.init_resource::<Messages<KeyboardInput>>();
        world.insert_resource(ViewBasis::default());
        let window = world.spawn_empty().id();

        let scratch = Scratch::new();
        let mut run = VerificationRun::new(scratch.output(), None);
        while run.stage() != VerificationStage::BeginRepair {
            run.machine
                .advance()
                .expect("the documented order reaches begin-repair");
        }
        (scratch, world, window, run)
    }

    /// Runs up to `frames` real driver frames, stopping the moment the stage
    /// hands over, so each test observes exactly one stage's behaviour.
    ///
    /// The release of the previous frame's taps goes through the driver's own
    /// [`release_tapped_keys`], not a copy of it: a helper that wrote the real
    /// release message but skipped the [`KeyFacts`] record would leave these
    /// tests looking at a key sequence the shipped driver never produces.
    fn step_frames(
        world: &mut World,
        run: &mut VerificationRun,
        state: &Snapshot,
        window: Entity,
        frames: u64,
    ) -> Result<(), String> {
        let entry = run.stage();
        for _ in 0..frames {
            run.frame += 1;
            run.stage_frame += 1;
            release_tapped_keys(run, world, window);
            step_stage(world, run, state, window)?;
            if run.stage() != entry {
                break;
            }
        }
        Ok(())
    }

    /// The retained failure exactly: the repair is running, the technician is
    /// held still off the arrival spot, and a movement key is down.
    #[test]
    fn begin_repair_hands_over_when_a_running_repair_holds_the_technician_off_the_spot() {
        let (_scratch, mut world, window, mut run) = begin_repair_run();
        let roster = journey_roster();
        let spot = journey_spot(&roster);
        let state = begin_repair_snapshot(
            roster,
            spot + Vec2::new(0.0, -1.5),
            PlayerClip::Repair,
            MovementLock::held_by(TicketId::new(2)),
            InteractionOutcome::Started {
                ticket: TicketId::new(2),
                rack: JOURNEY_RACK,
            },
        );

        step_frames(&mut world, &mut run, &state, window, 8).expect("the stage must not fail");

        assert_eq!(
            run.stage(),
            VerificationStage::RepairCapture,
            "an accepted repair is irreversible: begin-repair must hand over instead of \
             navigating against the movement lock"
        );
        assert!(
            run.held.is_empty(),
            "begin-repair must release every movement key on hand-over, still held: {:?}",
            run.held
        );
    }

    /// The retained report can observe a repair that already finished: the
    /// hand-over must still happen, not stall waiting for a lock that is gone.
    #[test]
    fn begin_repair_hands_over_when_the_started_repair_already_released_the_lock() {
        let (_scratch, mut world, window, mut run) = begin_repair_run();
        let roster = journey_roster();
        let spot = journey_spot(&roster);
        let state = begin_repair_snapshot(
            roster,
            spot + Vec2::new(2.0, 1.0),
            PlayerClip::Idle,
            MovementLock::default(),
            InteractionOutcome::Started {
                ticket: TicketId::new(2),
                rack: JOURNEY_RACK,
            },
        );

        step_frames(&mut world, &mut run, &state, window, 8).expect("the stage must not fail");

        assert_eq!(run.stage(), VerificationStage::RepairCapture);
        assert!(run.held.is_empty(), "still held: {:?}", run.held);

        // The hand-over is not the end of the story the report tells. The
        // repair capture needs a repair that is still holding the controls,
        // and this snapshot's lock has already been released, so the very next
        // frame must fail with that exact reason rather than photograph a hall
        // with nobody repairing anything. Stopping at the hand-over would
        // leave that claim untested.
        let reason = step_stage(&mut world, &mut run, &state, window)
            .expect_err("a repair capture with no live repair must fail immediately");
        assert!(
            reason.contains("no repair holding the controls"),
            "the repair capture must name what it was missing: {reason}"
        );
        assert!(
            run.pending.is_none() && run.staged_facts.is_none(),
            "a refused repair capture must not have asked the GPU for anything"
        );
    }

    /// The lock alone is enough, even with no recorded outcome yet.
    #[test]
    fn begin_repair_hands_over_on_the_movement_lock_alone() {
        let (_scratch, mut world, window, mut run) = begin_repair_run();
        let roster = journey_roster();
        let spot = journey_spot(&roster);
        let state = begin_repair_snapshot(
            roster,
            spot + Vec2::new(-0.9, 0.4),
            PlayerClip::Repair,
            MovementLock::held_by(TicketId::new(2)),
            InteractionOutcome::None,
        );

        step_frames(&mut world, &mut run, &state, window, 4).expect("the stage must not fail");

        assert_eq!(run.stage(), VerificationStage::RepairCapture);
        assert!(run.held.is_empty(), "still held: {:?}", run.held);
    }

    /// Navigation is still the behaviour before any repair starts.
    #[test]
    fn begin_repair_still_walks_to_the_spot_before_any_repair_starts() {
        let (_scratch, mut world, window, mut run) = begin_repair_run();
        let roster = journey_roster();
        let spot = journey_spot(&roster);
        let state = begin_repair_snapshot(
            roster,
            spot + Vec2::new(0.0, -4.0),
            PlayerClip::Walk,
            MovementLock::default(),
            InteractionOutcome::OutOfRange {
                nearest_rack: Some(JOURNEY_RACK),
                nearest_distance: 4.0,
            },
        );

        step_frames(&mut world, &mut run, &state, window, 6).expect("the stage must not fail");

        assert_eq!(
            run.stage(),
            VerificationStage::BeginRepair,
            "an unstarted repair still has to walk to the spot"
        );
        assert!(
            !run.held.is_empty(),
            "the walk to the repair spot must really hold arrow keys down"
        );
    }

    /// Arriving with no repair started still taps the real repair key.
    #[test]
    fn begin_repair_taps_the_repair_key_once_it_has_arrived() {
        let (_scratch, mut world, window, mut run) = begin_repair_run();
        let roster = journey_roster();
        let spot = journey_spot(&roster);
        let state = begin_repair_snapshot(
            roster,
            spot,
            PlayerClip::Idle,
            MovementLock::default(),
            InteractionOutcome::OutOfRange {
                nearest_rack: Some(JOURNEY_RACK),
                nearest_distance: 4.0,
            },
        );

        step_frames(&mut world, &mut run, &state, window, 4).expect("the stage must not fail");

        assert_eq!(run.stage(), VerificationStage::BeginRepair);
        assert!(run.held.is_empty(), "arriving releases the arrow keys");
        assert!(
            run.observations
                .keys
                .iter()
                .any(|key| key.key == key_name(REPAIR_KEY) && key.state == "pressed"),
            "begin-repair must press the real repair key: {:?}",
            run.observations.keys
        );
    }

    /// A stage that burns its budget has to say what the game actually looked
    /// like, so a rare timing failure is diagnosable from the retained report.
    #[test]
    fn a_stage_budget_failure_reports_the_state_that_stalled_it() {
        let (_scratch, mut world, window, mut run) = begin_repair_run();
        let roster = journey_roster();
        let spot = journey_spot(&roster);
        let state = begin_repair_snapshot(
            roster,
            spot + Vec2::new(0.0, -1.5),
            PlayerClip::Repair,
            MovementLock::held_by(TicketId::new(2)),
            InteractionOutcome::Started {
                ticket: TicketId::new(2),
                rack: JOURNEY_RACK,
            },
        );
        run.stage_frame = STAGE_FRAME_BUDGET + 1;

        let reason = step_stage(&mut world, &mut run, &state, window)
            .expect_err("the exhausted budget must fail");

        for fact in [
            "begin-repair",
            "player",
            "clip",
            "lock",
            "outcome",
            "held",
            "queue",
            "racks",
        ] {
            assert!(
                reason.contains(fact),
                "the budget failure must name {fact}: {reason}"
            );
        }
    }
}
