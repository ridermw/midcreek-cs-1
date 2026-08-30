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
    sync::OnceLock,
    time::{Duration, Instant},
};

use bevy::{
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
        CameraHeading, CameraOrbit, CellShiftCamera, clamp_follow_target, ground_quadrilateral,
    },
    design::{
        CHARACTER_SHEET_REFERENCE_PATH, KEY_ART_REFERENCE_PATH, MAX_ACTIVE_TICKETS, PLAYER_RADIUS,
        PaletteRole, RENDER_COVERAGE_SIZE, ROOM_SIZE, SceneBlueprint, VERIFICATION_WINDOW_HEIGHT,
        VERIFICATION_WINDOW_WIDTH,
    },
    hud::{
        BadgeKind, BadgeVisibility, ControlsPanel, HudReport, HudStatus, QueueRowNode,
        TicketQueuePanel, badge_half_extents,
    },
    operations::{
        FAULT_SCHEDULER_SEED, FaultScheduler, InteractionOutcome, LastInteraction, MovementLock,
        OperationsClock, REPAIR_KEY, RackOperations, RackRoster, RackState, TicketQueue,
        TicketSeverity,
    },
    player::{
        PlayerAnimationState, PlayerAnimations, PlayerClip, PlayerParts, PlayerRigReport,
        PlayerRigState, Technician, ViewBasis, required_player_parts,
    },
    world::{HallBlueprint, HallState, PlayerSpawnPoint},
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
}

/// Every file this module may ever write or remove, in stable order.
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

        let probe = path.join(".midcreek-verify-probe");
        fs::write(&probe, b"probe").map_err(|error| VerifyOutputError::Unwritable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
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

/// How many frames one capture is allowed, and always costs.
///
/// A capture always consumes exactly this many simulated frames, whether the
/// readback lands on the first or the last of them. That is what makes the
/// canonical report reproducible: capture latency is a property of the GPU, and
/// it must never leak into simulated time.
pub const CAPTURE_FRAMES: u64 = 24;

/// How many frames a window resize is allowed, and always costs.
pub const RESIZE_FRAMES: u64 = 45;

/// How many frames a corner probe lets the camera settle after placing the
/// technician, and always costs.
pub const PROBE_SETTLE_FRAMES: u64 = 6;

/// How long the app gives itself before it fails with the stage it is stuck in.
pub const APP_WATCHDOG: Duration = Duration::from_secs(45);

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
    requested_on: u64,
    completed: bool,
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
    frame: u64,
    stage_frame: u64,
    held: BTreeSet<KeyCode>,
    release_next: Vec<KeyCode>,
    pending: Option<PendingCapture>,
    capture_index: usize,
    probe_index: usize,
    resize_frame: Option<u64>,
    placed_frame: Option<u64>,
    observations: Observations,
    finished: bool,
    fault: Option<VerificationFault>,
}

impl VerificationRun {
    /// A run that writes into `output` and gives itself [`APP_WATCHDOG`].
    pub fn new(output: VerifyOutput, fault: Option<VerificationFault>) -> Self {
        Self {
            machine: StageMachine::default(),
            output,
            fault,
            started: Instant::now(),
            watchdog: APP_WATCHDOG,
            frame: 0,
            stage_frame: 0,
            held: BTreeSet::new(),
            release_next: Vec::new(),
            pending: None,
            capture_index: 0,
            probe_index: 0,
            resize_frame: None,
            placed_frame: None,
            observations: Observations::default(),
            finished: false,
        }
    }

    /// The current stage.
    pub fn stage(&self) -> VerificationStage {
        self.machine.stage()
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

    run.frame += 1;
    run.stage_frame += 1;

    let Some(window) = primary_window(world) else {
        run.machine.fail("the app never opened a primary window");
        finish(world, &mut run);
        world.insert_resource(run);
        return;
    };

    for key in std::mem::take(&mut run.release_next) {
        write_key(world, window, key, ButtonState::Released);
        run.observations.keys.push(KeyFacts {
            stage: run.machine.stage().name().to_owned(),
            key: key_name(key),
            state: "released".to_owned(),
        });
    }

    if run.fault != Some(VerificationFault::Hang) && run.started.elapsed() > run.watchdog {
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
/// A capture always costs exactly [`CAPTURE_FRAMES`] simulated frames. That is
/// deliberate: the readback latency is a property of the GPU, and letting it
/// change how much simulated time passes would make the canonical report
/// irreproducible. The callback must still have fired inside the budget, so a
/// lost callback is a hard failure rather than a silent pass.
fn capture(
    world: &mut World,
    run: &mut VerificationRun,
    state: &Snapshot,
    frame: FrameName,
) -> Result<bool, String> {
    match run.pending {
        None => {
            let facts = frame_facts(world, run, state, frame)?;
            run.observations
                .frames
                .insert(frame.file_name().to_owned(), facts);
            request_capture(world, run, frame);
            Ok(false)
        }
        Some(mut pending) => {
            if !pending.completed {
                let landed = world
                    .get_resource::<CaptureInbox>()
                    .is_some_and(|inbox| inbox.completed.contains(&frame));
                if landed {
                    pending.completed = true;
                    run.pending = Some(pending);
                }
            }
            let elapsed = run.frame - pending.requested_on;
            if elapsed < CAPTURE_FRAMES {
                return Ok(false);
            }
            if !pending.completed {
                return Err(format!(
                    "the screenshot callback for {} never fired within {CAPTURE_FRAMES} frames",
                    frame.file_name()
                ));
            }
            let path = run.output.frame(frame);
            let size = fs::metadata(&path)
                .map_err(|error| format!("{} was never written: {error}", frame.file_name()))?
                .len();
            if size == 0 {
                return Err(format!("{} was written empty", frame.file_name()));
            }
            run.pending = None;
            Ok(true)
        }
    }
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
        completed: false,
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
            "stage {} exceeded {STAGE_FRAME_BUDGET} frames",
            run.machine.stage().name()
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
                advance(run)
            } else {
                Ok(())
            }
        }

        VerificationStage::SeedThreeFaults => {
            if state.queue.len() >= MAX_ACTIVE_TICKETS {
                advance(run)
            } else {
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
            hold_keys(run, world, window, &keys);
            if !keys.is_empty() {
                return Ok(());
            }
            if state.lock.is_locked() {
                return advance(run);
            }
            match state.last.outcome {
                InteractionOutcome::Started { .. } => advance(run),
                _ if state.clip == PlayerClip::Idle && run.stage_frame.is_multiple_of(4) => {
                    tap_key(run, world, window, REPAIR_KEY);
                    Ok(())
                }
                _ => Ok(()),
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
                if state.orbit.is_settled() && run.release_next.is_empty() {
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

/// The ground point the scripted journey repairs [`JOURNEY_RACK`] from.
fn journey_target(state: &Snapshot) -> Result<Vec2, String> {
    let entry = state
        .roster
        .get(JOURNEY_RACK)
        .ok_or_else(|| format!("rack {JOURNEY_RACK} is not on the roster"))?;
    Ok(journey_repair_spot(entry.center, entry.half_extents))
}

/// Presses the real `E` key until the orbit settles on `heading`.
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
    if state.orbit.heading() != heading && state.orbit.is_settled() && run.release_next.is_empty() {
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
    let references = vec![
        KEY_ART_REFERENCE_PATH.to_owned(),
        CHARACTER_SHEET_REFERENCE_PATH.to_owned(),
    ];
    let code = [
        "src/verification.rs",
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

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Drives the scripted verification journey over the real game.
pub struct VerificationPlugin {
    output: VerifyOutput,
    fault: Option<VerificationFault>,
}

impl VerificationPlugin {
    /// A plugin that writes into a prepared output directory.
    pub fn new(output: VerifyOutput, fault: Option<VerificationFault>) -> Self {
        Self { output, fault }
    }
}

impl Plugin for VerificationPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            FIXED_STEP_SECONDS,
        )))
        .insert_resource(ClearColor(SENTINEL_CLEAR.into()))
        .init_resource::<CaptureInbox>()
        .insert_resource(VerificationRun::new(self.output.clone(), self.fault))
        .add_systems(
            Update,
            configure_verification_camera.in_set(CellShiftSet::AssetReady),
        )
        .add_systems(
            Update,
            drive_verification.in_set(CellShiftSet::VerificationProbe),
        );
    }
}

/// Turns off multisampling and tonemapping for the captured frames.
///
/// The cel-shift contract forbids gradients and noise, so the verification
/// camera renders the authored palette straight through: no MSAA blending, no
/// display transform, and no deband dither.
fn configure_verification_camera(
    mut commands: Commands,
    cameras: Query<Entity, (With<CellShiftCamera>, Without<VerificationCamera>)>,
) {
    for entity in &cameras {
        commands.entity(entity).insert((
            VerificationCamera,
            Msaa::Off,
            Tonemapping::None,
            DebandDither::Disabled,
        ));
    }
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
    let mut arguments = arguments.into_iter();
    let mut request = VerificationRequest::default();
    while let Some(argument) = arguments.next() {
        let (flag, value) = match argument.split_once('=') {
            Some((flag, value)) => (flag.to_owned(), Some(value.to_owned())),
            None => (argument.clone(), None),
        };
        match flag.as_str() {
            OUTPUT => {
                let Some(value) = value.or_else(|| arguments.next()) else {
                    return Err(format!("{OUTPUT} requires a directory path"));
                };
                if request.output.is_some() {
                    return Err(format!("{OUTPUT} was given more than once"));
                }
                request.output = Some(PathBuf::from(value));
            }
            FAULT => {
                let Some(value) = value.or_else(|| arguments.next()) else {
                    return Err(format!("{FAULT} requires a fault name"));
                };
                if request.fault.is_some() {
                    return Err(format!("{FAULT} was given more than once"));
                }
                request.fault = Some(VerificationFault::parse(&value)?);
            }
            _ => {
                return Err(format!(
                    "unknown argument {argument}; usage: midcreek-cs-1 [{OUTPUT} <directory>] [{FAULT} <fault>]"
                ));
            }
        }
    }
    if request.fault.is_some() && request.output.is_none() {
        return Err(format!("{FAULT} only applies to a {OUTPUT} run"));
    }
    Ok(request)
}

// ---------------------------------------------------------------------------
// Frame analysis
// ---------------------------------------------------------------------------

/// A pixel rectangle inside one frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelRect {
    /// Left edge, in pixels.
    pub x: u32,
    /// Top edge, in pixels.
    pub y: u32,
    /// Width, in pixels.
    pub width: u32,
    /// Height, in pixels.
    pub height: u32,
}

impl PixelRect {
    /// Snaps a reported logical rectangle onto the pixel grid of one frame.
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

    /// Whether one pixel is inside.
    pub const fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    /// How many pixels the rectangle covers.
    pub const fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

/// Everything one named region of a frame measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegionMetrics {
    /// How many pixels the region covered.
    pub pixels: u64,
    /// Fraction of the region within [`PALETTE_TOLERANCE`] of each role.
    pub near_role_ratio: BTreeMap<PaletteRole, f64>,
    /// Mean linear luminance inside the region.
    pub mean_linear_luminance: f64,
    /// Fraction of the region holding the magenta sentinel.
    pub sentinel_ratio: f64,
}

impl RegionMetrics {
    /// Fraction of the region within tolerance of one role.
    pub fn near(&self, role: PaletteRole) -> f64 {
        self.near_role_ratio.get(&role).copied().unwrap_or(0.0)
    }
}

/// Euclidean RGB distance at which a pixel still counts as one palette role.
pub const PALETTE_TOLERANCE: f64 = 24.0;

/// Gradient magnitude, in 0..1 grey units, above which an edge counts.
pub const STRONG_EDGE: f64 = 0.10;

/// Everything one frame measured, computed in a single traversal.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameMetrics {
    /// Frame width, in pixels.
    pub width: u32,
    /// Frame height, in pixels.
    pub height: u32,
    /// Total pixels.
    pub pixels: u64,
    /// Mean linear luminance over the whole frame.
    pub mean_linear_luminance: f64,
    /// Fraction of the frame holding the magenta sentinel.
    pub sentinel_ratio: f64,
    /// Fraction of the frame within [`PALETTE_TOLERANCE`] of any approved role.
    pub palette_ratio: f64,
    /// Nearest-role histogram, normalized; sums to one.
    pub nearest_role_ratio: BTreeMap<PaletteRole, f64>,
    /// Fraction of the frame within tolerance of each role.
    pub near_role_ratio: BTreeMap<PaletteRole, f64>,
    /// Fraction of pixels carrying a strong edge.
    pub edge_density: f64,
    /// Share of strong-edge mass whose edge direction lies in 30..50 degrees.
    pub diagonal_band_low: f64,
    /// Share of strong-edge mass whose edge direction lies in 130..150 degrees.
    pub diagonal_band_high: f64,
    /// Per-region measurements, by stable region name.
    pub regions: BTreeMap<String, RegionMetrics>,
}

impl FrameMetrics {
    /// Fraction of the frame within tolerance of one role.
    pub fn near(&self, role: PaletteRole) -> f64 {
        self.near_role_ratio.get(&role).copied().unwrap_or(0.0)
    }

    /// Fraction of the frame classified nearest to one role.
    pub fn nearest(&self, role: PaletteRole) -> f64 {
        self.nearest_role_ratio.get(&role).copied().unwrap_or(0.0)
    }

    /// Combined fraction of several roles, by nearest classification.
    pub fn nearest_of(&self, roles: &[PaletteRole]) -> f64 {
        roles.iter().map(|role| self.nearest(*role)).sum()
    }

    /// L1 distance between two nearest-role histograms.
    pub fn histogram_distance(&self, other: &Self) -> f64 {
        PaletteRole::ALL
            .iter()
            .map(|role| (self.nearest(*role) - other.nearest(*role)).abs())
            .sum()
    }

    /// One measured region.
    pub fn region(&self, name: &str) -> Option<&RegionMetrics> {
        self.regions.get(name)
    }

    /// Measures one decoded frame and every supplied region in one traversal.
    ///
    /// Colour statistics, the sentinel ratio, the nearest-role histogram, the
    /// per-region accumulators, and the Sobel edge orientation histogram are
    /// all produced by the same walk over the image; nothing is re-read.
    pub fn compute(image: &RgbImage, regions: &BTreeMap<String, PixelRect>) -> Self {
        let width = image.width();
        let height = image.height();
        let pixels = u64::from(width) * u64::from(height);
        let palette = palette_table();
        let luminance = linear_luminance_table();

        let mut luminance_sum = 0.0;
        let mut sentinel = 0u64;
        let mut palette_hits = 0u64;
        let mut nearest = [0u64; PaletteRole::ALL.len()];
        let mut near = [0u64; PaletteRole::ALL.len()];
        let mut edge_pixels = 0u64;
        let mut edge_mass = 0.0;
        let mut band_low = 0.0;
        let mut band_high = 0.0;

        let names = regions.keys().cloned().collect::<Vec<_>>();
        let rects = names.iter().map(|name| regions[name]).collect::<Vec<_>>();
        let mut region_luminance = vec![0.0f64; rects.len()];
        let mut region_sentinel = vec![0u64; rects.len()];
        let mut region_near = vec![[0u64; PaletteRole::ALL.len()]; rects.len()];
        let mut region_pixels = vec![0u64; rects.len()];

        let raw = image.as_raw();
        let stride = width as usize * 3;
        for y in 0..height {
            let row = y as usize * stride;
            for x in 0..width {
                let index = row + x as usize * 3;
                let red = raw[index];
                let green = raw[index + 1];
                let blue = raw[index + 2];

                let pixel_luminance = 0.2126 * luminance[red as usize]
                    + 0.7152 * luminance[green as usize]
                    + 0.0722 * luminance[blue as usize];
                luminance_sum += pixel_luminance;

                let is_sentinel = red >= 240 && green <= 24 && blue >= 240;
                if is_sentinel {
                    sentinel += 1;
                }

                // The palette is scanned once per pixel. The tolerance hits
                // are kept as a bit mask so every region this pixel falls in
                // reuses that one scan instead of repeating it.
                let mut best = f64::INFINITY;
                let mut best_role = 0usize;
                let mut near_mask = 0u32;
                for (role, colour) in palette.iter().enumerate() {
                    let distance = squared_distance(red, green, blue, *colour);
                    if distance < best {
                        best = distance;
                        best_role = role;
                    }
                    if distance <= PALETTE_TOLERANCE * PALETTE_TOLERANCE {
                        near[role] += 1;
                        near_mask |= 1 << role;
                    }
                }
                nearest[best_role] += 1;
                if best <= PALETTE_TOLERANCE * PALETTE_TOLERANCE {
                    palette_hits += 1;
                }

                for (slot, rect) in rects.iter().enumerate() {
                    if !rect.contains(x, y) {
                        continue;
                    }
                    region_pixels[slot] += 1;
                    region_luminance[slot] += pixel_luminance;
                    if is_sentinel {
                        region_sentinel[slot] += 1;
                    }
                    let mut remaining = near_mask;
                    while remaining != 0 {
                        let role = remaining.trailing_zeros() as usize;
                        remaining &= remaining - 1;
                        region_near[slot][role] += 1;
                    }
                }

                if x == 0 || y == 0 || x + 1 >= width || y + 1 >= height {
                    continue;
                }
                let grey = |dx: i32, dy: i32| -> f64 {
                    let sx = (x as i32 + dx) as usize;
                    let sy = (y as i32 + dy) as usize;
                    let at = sy * stride + sx * 3;
                    (0.299 * f64::from(raw[at])
                        + 0.587 * f64::from(raw[at + 1])
                        + 0.114 * f64::from(raw[at + 2]))
                        / 255.0
                };
                let gradient_x = (grey(1, -1) + 2.0 * grey(1, 0) + grey(1, 1))
                    - (grey(-1, -1) + 2.0 * grey(-1, 0) + grey(-1, 1));
                let gradient_y = (grey(-1, 1) + 2.0 * grey(0, 1) + grey(1, 1))
                    - (grey(-1, -1) + 2.0 * grey(0, -1) + grey(1, -1));
                let magnitude = (gradient_x * gradient_x + gradient_y * gradient_y).sqrt() / 4.0;
                if magnitude < STRONG_EDGE {
                    continue;
                }
                edge_pixels += 1;
                edge_mass += magnitude;
                // Screen space runs y downwards; the edge itself is the
                // gradient turned a quarter turn, and only its axis matters.
                let direction =
                    (gradient_x.atan2(-gradient_y).to_degrees() + 180.0).rem_euclid(180.0);
                if (30.0..50.0).contains(&direction) {
                    band_low += magnitude;
                } else if (130.0..150.0).contains(&direction) {
                    band_high += magnitude;
                }
            }
        }

        let total = pixels.max(1) as f64;
        let ratios = |counts: &[u64; PaletteRole::ALL.len()], divisor: f64| {
            PaletteRole::ALL
                .iter()
                .enumerate()
                .map(|(index, role)| (*role, counts[index] as f64 / divisor))
                .collect::<BTreeMap<_, _>>()
        };

        let mut measured = BTreeMap::new();
        for (slot, name) in names.into_iter().enumerate() {
            let count = region_pixels[slot].max(1) as f64;
            measured.insert(
                name,
                RegionMetrics {
                    pixels: region_pixels[slot],
                    near_role_ratio: ratios(&region_near[slot], count),
                    mean_linear_luminance: region_luminance[slot] / count,
                    sentinel_ratio: region_sentinel[slot] as f64 / count,
                },
            );
        }

        let mass = if edge_mass > 0.0 { edge_mass } else { 1.0 };
        Self {
            width,
            height,
            pixels,
            mean_linear_luminance: luminance_sum / total,
            sentinel_ratio: sentinel as f64 / total,
            palette_ratio: palette_hits as f64 / total,
            nearest_role_ratio: ratios(&nearest, total),
            near_role_ratio: ratios(&near, total),
            edge_density: edge_pixels as f64 / total,
            diagonal_band_low: band_low / mass,
            diagonal_band_high: band_high / mass,
            regions: measured,
        }
    }
}

fn squared_distance(red: u8, green: u8, blue: u8, colour: [f64; 3]) -> f64 {
    let dr = f64::from(red) - colour[0];
    let dg = f64::from(green) - colour[1];
    let db = f64::from(blue) - colour[2];
    dr * dr + dg * dg + db * db
}

fn palette_table() -> &'static [[f64; 3]; PaletteRole::ALL.len()] {
    static TABLE: OnceLock<[[f64; 3]; PaletteRole::ALL.len()]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [[0.0; 3]; PaletteRole::ALL.len()];
        for (slot, role) in PaletteRole::ALL.into_iter().enumerate() {
            let colour = role.color();
            table[slot] = [
                f64::from(colour.red) * 255.0,
                f64::from(colour.green) * 255.0,
                f64::from(colour.blue) * 255.0,
            ];
        }
        table
    })
}

fn linear_luminance_table() -> &'static [f64; 256] {
    static TABLE: OnceLock<[f64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0.0; 256];
        for (value, slot) in table.iter_mut().enumerate() {
            let channel = value as f64 / 255.0;
            *slot = if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            };
        }
        table
    })
}

/// Loads one PNG frame.
pub fn load_frame(path: &Path) -> Result<RgbImage, String> {
    let decoded = image::open(path)
        .map_err(|error| format!("{} could not be decoded: {error}", path.display()))?;
    Ok(decoded.to_rgb8())
}

/// The approved key art's metrics, measured once and cached for the process.
pub fn reference_metrics() -> &'static FrameMetrics {
    static REFERENCE: OnceLock<FrameMetrics> = OnceLock::new();
    REFERENCE.get_or_init(|| {
        let image = load_frame(Path::new(KEY_ART_REFERENCE_PATH))
            .expect("the approved key art is vendored in this repository");
        FrameMetrics::compute(&image, &BTreeMap::new())
    })
}

// ---------------------------------------------------------------------------
// Mandatory frame contracts
// ---------------------------------------------------------------------------

/// Largest share of magenta sentinel a frame may hold.
///
/// The sentinel is the clear colour, so any of it on screen means the camera's
/// ground quadrilateral left the 72 m rendered apron. This is the
/// rendered-coverage gate.
pub const SENTINEL_MAX: f64 = 0.001;

/// Absolute mean linear luminance window.
pub const LUMINANCE_RANGE: (f64, f64) = (0.48, 0.88);

/// How far a frame's mean linear luminance may sit from the approved key art.
pub const LUMINANCE_REFERENCE_TOLERANCE: f64 = 0.18;

/// Smallest share of pixels within [`PALETTE_TOLERANCE`] of the typed palette.
pub const PALETTE_MIN: f64 = 0.60;

/// Smallest share of floor tones.
pub const FLOOR_MIN: f64 = 0.20;

/// Smallest share of rack base and rack shadow tones.
pub const RACK_MIN: f64 = 0.06;

/// Smallest share of signature yellow.
pub const YELLOW_MIN: f64 = 0.005;

/// Allowed share of ink and hose charcoal.
pub const INK_RANGE: (f64, f64) = (0.03, 0.35);

/// Smallest share of strong-edge mass each diagonal band must hold at a
/// settled heading.
pub const DIAGONAL_BAND_MIN: f64 = 0.08;

/// Largest nearest-palette histogram L1 distance from the key art.
pub const HISTOGRAM_MAX: f64 = 0.90;

/// Allowed edge density, as a multiple of the key art's edge density.
pub const EDGE_DENSITY_RANGE: (f64, f64) = (0.35, 2.5);

/// Smallest share of the projected worker crop each worker identity colour
/// must cover.
pub const WORKER_ROLE_MIN: f64 = 0.002;

/// Smallest share of a drawn badge rectangle its own colour must cover.
pub const BADGE_ROLE_MIN: f64 = 0.10;

/// Smallest share of the queue panel each live state colour must cover.
pub const HUD_STATE_MIN: f64 = 0.002;

/// Allowed difference between two worker crops playing different clips.
pub const CLIP_DIFFERENCE_RANGE: (f64, f64) = (0.02, 0.60);

/// Largest share of a frame outside the worker crop that may change between
/// two captures taken from the same position.
pub const OUTSIDE_CROP_MAX: f64 = 0.01;

/// The stable region name of the projected worker crop.
pub const WORKER_REGION: &str = "worker";

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

    if metrics.sentinel_ratio > SENTINEL_MAX {
        failures.push(failure(
            name,
            "sentinel-ratio",
            metrics.sentinel_ratio,
            format!("at most {SENTINEL_MAX}; the ground quadrilateral left the rendered apron"),
        ));
    }

    let luminance = metrics.mean_linear_luminance;
    if luminance < LUMINANCE_RANGE.0 || luminance > LUMINANCE_RANGE.1 {
        failures.push(failure(
            name,
            "mean-linear-luminance",
            luminance,
            format!("between {} and {}", LUMINANCE_RANGE.0, LUMINANCE_RANGE.1),
        ));
    }
    let drift = (luminance - reference.mean_linear_luminance).abs();
    if drift > LUMINANCE_REFERENCE_TOLERANCE {
        failures.push(failure(
            name,
            "luminance-drift-from-key-art",
            drift,
            format!("at most {LUMINANCE_REFERENCE_TOLERANCE}"),
        ));
    }

    if metrics.palette_ratio < PALETTE_MIN {
        failures.push(failure(
            name,
            "palette-ratio",
            metrics.palette_ratio,
            format!("at least {PALETTE_MIN}"),
        ));
    }

    let floor = metrics.nearest_of(&[PaletteRole::FloorLight, PaletteRole::FloorShadow]);
    if floor < FLOOR_MIN {
        failures.push(failure(
            name,
            "floor-ratio",
            floor,
            format!("at least {FLOOR_MIN}"),
        ));
    }
    let rack = metrics.nearest_of(&[PaletteRole::RackWhite, PaletteRole::RackShadow]);
    if rack < RACK_MIN {
        failures.push(failure(
            name,
            "rack-ratio",
            rack,
            format!("at least {RACK_MIN}"),
        ));
    }
    let yellow = metrics.nearest(PaletteRole::SignatureYellow);
    if yellow < YELLOW_MIN {
        failures.push(failure(
            name,
            "signature-yellow-ratio",
            yellow,
            format!("at least {YELLOW_MIN}"),
        ));
    }
    let ink = metrics.nearest_of(&[PaletteRole::Ink, PaletteRole::HoseCharcoal]);
    if ink < INK_RANGE.0 || ink > INK_RANGE.1 {
        failures.push(failure(
            name,
            "ink-and-hose-ratio",
            ink,
            format!("between {} and {}", INK_RANGE.0, INK_RANGE.1),
        ));
    }

    if frame.is_settled() {
        if metrics.diagonal_band_low < DIAGONAL_BAND_MIN {
            failures.push(failure(
                name,
                "diagonal-edge-band-30-50",
                metrics.diagonal_band_low,
                format!("at least {DIAGONAL_BAND_MIN}"),
            ));
        }
        if metrics.diagonal_band_high < DIAGONAL_BAND_MIN {
            failures.push(failure(
                name,
                "diagonal-edge-band-130-150",
                metrics.diagonal_band_high,
                format!("at least {DIAGONAL_BAND_MIN}"),
            ));
        }
    }

    let distance = metrics.histogram_distance(reference);
    if distance > HISTOGRAM_MAX {
        failures.push(failure(
            name,
            "key-art-histogram-distance",
            distance,
            format!("at most {HISTOGRAM_MAX}"),
        ));
    }
    let density = metrics.edge_density / reference.edge_density;
    if density < EDGE_DENSITY_RANGE.0 || density > EDGE_DENSITY_RANGE.1 {
        failures.push(failure(
            name,
            "edge-density-vs-key-art",
            density,
            format!(
                "between {} and {} times the key art",
                EDGE_DENSITY_RANGE.0, EDGE_DENSITY_RANGE.1
            ),
        ));
    }

    match metrics.region(WORKER_REGION) {
        None => failures.push(failure(name, "worker-crop", 0.0, "a measured crop")),
        Some(worker) => {
            for role in [PaletteRole::WorkerHardHat, PaletteRole::WorkerHiVis] {
                let share = worker.near(role);
                if share < WORKER_ROLE_MIN {
                    failures.push(failure(
                        name,
                        &format!("worker-crop-{role:?}"),
                        share,
                        format!("at least {WORKER_ROLE_MIN} of the projected worker crop"),
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
                if share < BADGE_ROLE_MIN {
                    failures.push(failure(
                        name,
                        &format!("{region}-{role:?}"),
                        share,
                        format!("at least {BADGE_ROLE_MIN} of the badge rectangle"),
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
            if share < HUD_STATE_MIN {
                failures.push(failure(
                    name,
                    &format!("hud-queue-{role:?}"),
                    share,
                    format!("at least {HUD_STATE_MIN} of the queue panel"),
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
