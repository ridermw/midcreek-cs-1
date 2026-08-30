use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use image::ImageReader;
use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};
use scraper::{Html, Node, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    design::{
        CHARACTER_SHEET_REFERENCE_PATH, CHARACTER_SHEET_SHA256, KEY_ART_REFERENCE_PATH,
        KEY_ART_SHA256,
    },
    verification::{
        FrameFacts, FrameName, PixelRect, VerificationReport, canonical_json, semantic_hash,
    },
};

/// The published directory holding every screenshot the hub serves.
pub const SCREENSHOTS_ROOT: &str = "screenshots";

/// The published directory holding the frames of the current verified run.
pub const CURRENT_SCREENSHOTS: &str = "screenshots/current";

/// The published directory holding one folder per accepted history entry.
pub const HISTORY_SCREENSHOTS: &str = "screenshots/history";

/// The published screenshot history manifest.
pub const GALLERY_FILE: &str = "gallery.json";

/// The published record of the last green publication.
pub const LAST_GREEN_FILE: &str = "last-green.json";

/// The published sanitized projection of the verification reports.
pub const VERIFICATION_FILE: &str = "verification.json";

/// The published crop of the technician, taken from the centre frame.
pub const WORKER_CROP_FILE: &str = "worker-crop.png";

/// The published browser-gate canvas screenshot.
pub const BROWSER_FRAME_FILE: &str = "browser-canvas.png";

/// The browser gate's own canvas screenshot, inside its diagnostics directory.
pub const BROWSER_CANVAS_ARTIFACT: &str = "canvas.png";

/// The four frames the public comparison and every history entry carry.
pub const GALLERY_FRAMES: [(&str, &str); 4] = [
    ("center", "01-healthy-center-ne.png"),
    ("fault", "02-fault-queue-ne.png"),
    ("repair", "04-repairing-ne.png"),
    ("resolved", "05-resolved-ne.png"),
];

/// The stable label of the crop published beside the character sheet.
pub const WORKER_FRAME_LABEL: &str = "worker";

/// The one repository every published commit link points into.
///
/// Task cards and challenge cards both link commits, from different renderers.
/// A second copy of this URL is a second thing to rename, so there is exactly
/// one and both read it.
pub const REPOSITORY_URL: &str = "https://github.com/ridermw/midcreek-cs-1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressDocument {
    pub schema_version: u32,
    pub project: String,
    pub tasks: Vec<ProgressTask>,
    pub challenges: Vec<Challenge>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStatus {
    Future,
    InProgress,
    Done,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressTask {
    pub id: String,
    pub title: String,
    pub status: ProgressStatus,
    pub depends_on: Vec<String>,
    pub summary: String,
    pub completed_commit: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeStatus {
    Open,
    Resolved,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub id: String,
    pub title: String,
    pub status: ChallengeStatus,
    pub impact: String,
    pub approach: String,
    pub resolution: Option<String>,
    pub resolved_commit: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepoFacts {
    pub head_sha: String,
    pub known_commits: BTreeSet<String>,
    pub commits: Vec<CommitSummary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitSummary {
    pub sha: String,
    pub subject: String,
    pub committed_at: String,
    pub task_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceAsset {
    pub name: String,
    pub source_path: String,
    pub public_path: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceManifest {
    pub assets: Vec<ReferenceAsset>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    Passed,
    Failed,
    SkippedDependency,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GateSummary {
    pub name: String,
    pub status: GateStatus,
    pub passed: u32,
    pub failed: u32,
    pub duration_ms: u64,
    pub artifact_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSummary {
    pub source_commit: String,
    pub run_url: String,
    pub native: GateStatus,
    pub web: GateStatus,
    pub gates: Vec<GateSummary>,
}

// ---------------------------------------------------------------------------
// Workflow job results
// ---------------------------------------------------------------------------

/// The file name each job's result artifact carries.
pub const RESULT_FILE: &str = "result.json";

/// The published name of the job that runs the native gates.
pub const NATIVE_JOB: &str = "verify";

/// The published name of the job that packages and proves the browser game.
pub const WEB_JOB: &str = "build-web";

/// The gate a missing native job is published as.
pub const NATIVE_JOB_GATE: &str = "Native verification";

/// The gate a missing web job is published as.
pub const WEB_JOB_GATE: &str = "Web package and browser gate";

/// The file `scripts/run-gate.sh` appends one measured gate to.
pub const GATE_RESULTS_FILE: &str = "gates.jsonl";

/// One named gate, exactly as the runner measured it.
///
/// The runner records the gate's name, whether the command succeeded, and how
/// long it really took. It never records the command, its output, or anything
/// about the machine it ran on.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GateRecord {
    /// The published gate name.
    pub name: String,
    /// Whether the gate's command succeeded.
    pub status: GateStatus,
    /// The measured wall-clock duration.
    pub duration_ms: u64,
}

/// Reads every gate one job measured, in the order it ran them.
///
/// A record the runner could not have written is a corrupted or forged result,
/// so the whole file is refused rather than partially published.
pub fn read_gate_records(lines: &str) -> Result<Vec<GateSummary>, SitegenError> {
    let mut gates = Vec::new();
    for line in lines.lines().filter(|line| !line.trim().is_empty()) {
        let record =
            serde_json::from_str::<GateRecord>(line).map_err(|error| SitegenError::Json {
                path: PathBuf::from(GATE_RESULTS_FILE),
                message: error.to_string(),
            })?;
        let gate = GateSummary {
            name: record.name,
            status: record.status,
            passed: u32::from(record.status == GateStatus::Passed),
            failed: u32::from(record.status == GateStatus::Failed),
            duration_ms: record.duration_ms,
            artifact_url: None,
        };
        validate_gate(&gate)?;
        gates.push(gate);
    }
    Ok(gates)
}

/// The verdict a job reaches over every gate it measured.
///
/// A job that measured nothing at all failed before its first gate, so it
/// never reports success by omission.
pub fn gate_verdict(gates: &[GateSummary]) -> GateStatus {
    if gates.is_empty() || gates.iter().any(|gate| gate.status != GateStatus::Passed) {
        return GateStatus::Failed;
    }
    GateStatus::Passed
}

/// The longest gate name the site will publish.
const MAX_GATE_NAME: usize = 96;

/// The only URL prefix a published link may carry.
const TRUSTED_URL_PREFIX: &str = "https://github.com/";

/// The strict result manifest one workflow job uploads.
///
/// Each job measures its own named gates and declares whether it produced a
/// complete, publishable evidence directory. Nothing else crosses the boundary
/// between a runner and the public site: the shape denies unknown fields, so a
/// manifest that grew a command line, a log excerpt, or an environment map is
/// refused instead of being merged unread.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobResult {
    /// The job that wrote it.
    pub job: String,
    /// The job's own verdict over every gate it ran.
    pub status: GateStatus,
    /// Every named gate the job ran, in the order it ran them.
    pub gates: Vec<GateSummary>,
    /// The artifact-relative directory holding publishable evidence, when the
    /// job produced a complete set.
    pub evidence: Option<String>,
}

impl JobResult {
    /// Whether this manifest measured a single named gate.
    ///
    /// A job that fell over before its first gate still uploads a manifest,
    /// and that manifest is empty: it records that the job existed and nothing
    /// about what it proved. Publishing from it would put a run on the site
    /// with no row explaining why it failed, so an empty manifest is treated
    /// as the incomplete result it is.
    pub fn measured_a_gate(&self) -> bool {
        !self.gates.is_empty()
    }
}

/// The outcome GitHub Actions reports for one job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobOutcome {
    Success,
    Failure,
    Cancelled,
    Skipped,
}

impl JobOutcome {
    /// Parses one `needs.<job>.result` value.
    ///
    /// An empty value is what a job that never reported leaves behind, and an
    /// unknown value is a GitHub outcome this repository has not reviewed.
    /// Both are read as a failure, because publishing an unreviewed outcome as
    /// a success is the one mistake this workflow may not make.
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "success" => Self::Success,
            "skipped" => Self::Skipped,
            "cancelled" => Self::Cancelled,
            _ => Self::Failure,
        }
    }

    /// The published status this outcome alone justifies.
    pub fn status(self) -> GateStatus {
        match self {
            Self::Success => GateStatus::Passed,
            Self::Skipped => GateStatus::SkippedDependency,
            Self::Failure | Self::Cancelled => GateStatus::Failed,
        }
    }

    /// The name this outcome is published under.
    pub fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }
}

/// One job's result artifact, as Publish found it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobReport {
    /// What GitHub reported about the job itself.
    pub outcome: JobOutcome,
    /// The manifest the job uploaded, when one arrived and parsed.
    pub result: Option<JobResult>,
}

impl JobReport {
    /// A job whose result artifact never arrived.
    pub fn absent(outcome: JobOutcome) -> Self {
        Self {
            outcome,
            result: None,
        }
    }

    /// The manifest the site may publish this job from.
    ///
    /// A manifest that measured no gates is incomplete, whatever it declares
    /// about itself, so it is read exactly like a manifest that never
    /// arrived. Everything the site says about the job — its status, its
    /// evidence, and its rows in the published matrix — then comes from the
    /// one path that already knows how to explain a job that reported
    /// nothing.
    pub fn publishable_result(&self) -> Option<&JobResult> {
        self.result
            .as_ref()
            .filter(|result| result.measured_a_gate())
    }

    /// The status the site publishes for this job.
    ///
    /// The job outcome and the manifest both have to agree on success. A job
    /// that passed every gate and then failed to upload its artifact, and a
    /// job whose manifest declares a failure GitHub did not see, are both
    /// published as failures.
    pub fn status(&self) -> GateStatus {
        let outcome = self.outcome.status();
        match self.publishable_result() {
            Some(result) if result.status != GateStatus::Passed => GateStatus::Failed,
            Some(_) => outcome,
            None if outcome == GateStatus::SkippedDependency => GateStatus::SkippedDependency,
            None => GateStatus::Failed,
        }
    }

    /// The evidence directory this job declared, when the job really passed.
    ///
    /// A failed or skipped job publishes no evidence at all, so a partial
    /// directory a failing run happened to leave behind can never be projected
    /// onto the public site.
    pub fn evidence(&self) -> Option<&str> {
        if self.status() != GateStatus::Passed {
            return None;
        }
        self.publishable_result()
            .and_then(|result| result.evidence.as_deref())
    }
}

/// Refuses a result manifest that carries a value the public site may not
/// publish.
pub fn validate_job_result(result: &JobResult) -> Result<(), SitegenError> {
    if result.job != NATIVE_JOB && result.job != WEB_JOB {
        return Err(SitegenError::UnsafeResultValue {
            field: "job".to_owned(),
            message: format!("{:?} is not a declared workflow job", result.job),
        });
    }
    for gate in &result.gates {
        validate_gate(gate)?;
    }
    if let Some(evidence) = &result.evidence {
        validate_relative_directory("evidence", evidence)?;
    }
    Ok(())
}

/// Refuses a merged workflow summary that carries a value the public site may
/// not publish.
pub fn validate_workflow_summary(summary: &WorkflowSummary) -> Result<(), SitegenError> {
    validate_commit("source_commit", &summary.source_commit)?;
    validate_url("run_url", &summary.run_url)?;
    for gate in &summary.gates {
        validate_gate(gate)?;
    }
    Ok(())
}

fn validate_gate(gate: &GateSummary) -> Result<(), SitegenError> {
    validate_published_text("gates[].name", &gate.name, MAX_GATE_NAME)?;
    if let Some(url) = &gate.artifact_url {
        validate_url("gates[].artifact_url", url)?;
    }
    Ok(())
}

fn validate_commit(field: &str, value: &str) -> Result<(), SitegenError> {
    let valid = value.len() == 40 && value.chars().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        return Ok(());
    }
    Err(SitegenError::UnsafeResultValue {
        field: field.to_owned(),
        message: format!("{value:?} is not a full commit SHA"),
    })
}

/// Accepts only an absolute URL into this repository's own GitHub host.
///
/// The site links workflow runs and artifacts, and both only ever live under
/// `github.com`. Anything else is an untrusted destination a runner should
/// never be able to publish onto a page other people open.
fn validate_url(field: &str, value: &str) -> Result<(), SitegenError> {
    let trusted = value.starts_with(TRUSTED_URL_PREFIX)
        && value.len() > TRUSTED_URL_PREFIX.len()
        && !value.contains("..")
        && value
            .chars()
            .all(|byte| byte.is_ascii_graphic() && byte != '"' && byte != '\'' && byte != '\\');
    if trusted {
        return Ok(());
    }
    Err(SitegenError::UnsafeResultValue {
        field: field.to_owned(),
        message: format!("{value:?} is not a {TRUSTED_URL_PREFIX} URL"),
    })
}

/// Accepts only a short, printable, single-line label with no path in it.
fn validate_published_text(field: &str, value: &str, limit: usize) -> Result<(), SitegenError> {
    let trimmed = value.trim();
    let printable = !trimmed.is_empty()
        && trimmed.len() <= limit
        && trimmed
            .chars()
            .all(|byte| byte.is_ascii_graphic() || byte == ' ');
    let pathless = !trimmed.contains('/') && !trimmed.contains('\\') && !trimmed.contains("..");
    if printable && pathless {
        return Ok(());
    }
    Err(SitegenError::UnsafeResultValue {
        field: field.to_owned(),
        message: format!("{value:?} is not a publishable label"),
    })
}

/// Accepts only a relative directory strictly inside the artifact that names
/// it.
fn validate_relative_directory(field: &str, value: &str) -> Result<(), SitegenError> {
    let path = Path::new(value);
    let contained = !value.is_empty()
        && !value.contains('\\')
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if contained {
        return Ok(());
    }
    Err(SitegenError::UnsafeResultValue {
        field: field.to_owned(),
        message: format!("{value:?} is not a contained relative directory"),
    })
}

/// Combines what the two jobs reported into the one summary the site
/// publishes.
///
/// Publish runs whatever the upstream jobs did, so every gap has to become a
/// published fact rather than a missing row: a skipped job publishes a
/// `skipped_dependency` gate, and a job that ran without leaving a usable
/// result manifest publishes both a failed job gate and the missing manifest
/// itself. A manifest that measured no gates at all — what a job that fell
/// over before its first gate uploads — is a gap of exactly that kind, and is
/// published the same way rather than as silence.
pub fn merge_job_results(
    source_commit: &str,
    run_url: &str,
    native: &JobReport,
    web: &JobReport,
) -> Result<WorkflowSummary, SitegenError> {
    let mut gates = Vec::new();
    for (report, label) in [(native, NATIVE_JOB_GATE), (web, WEB_JOB_GATE)] {
        match report.publishable_result() {
            Some(result) => {
                validate_job_result(result)?;
                gates.extend(result.gates.iter().cloned());
                // A job may pass every gate it measured and still fail after
                // the last one. The published matrix has to say so.
                if report.status() == GateStatus::Failed && result.status == GateStatus::Passed {
                    gates.push(job_gate(label, GateStatus::Failed));
                }
            }
            None => {
                gates.push(job_gate(label, report.status()));
                if report.outcome != JobOutcome::Skipped {
                    gates.push(job_gate(
                        &format!("{label} result manifest"),
                        GateStatus::Failed,
                    ));
                }
            }
        }
    }

    let summary = WorkflowSummary {
        source_commit: source_commit.to_owned(),
        run_url: run_url.to_owned(),
        native: native.status(),
        web: web.status(),
        gates,
    };
    validate_workflow_summary(&summary)?;
    Ok(summary)
}

fn job_gate(name: &str, status: GateStatus) -> GateSummary {
    GateSummary {
        name: name.to_owned(),
        status,
        passed: u32::from(status == GateStatus::Passed),
        failed: u32::from(status == GateStatus::Failed),
        duration_ms: 0,
        artifact_url: None,
    }
}

// ---------------------------------------------------------------------------
// Raw publication inputs
// ---------------------------------------------------------------------------

/// The document `scripts/browser_gate.py` writes, exactly as it writes it.
///
/// The shape is strict so a gate summary that grew a field, or that was edited
/// by hand, is refused instead of being published unread.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserGateReport {
    /// The measured canvas.
    pub canvas: BrowserCanvasFacts,
    /// The analysed canvas region.
    pub pixels: BrowserPixelFacts,
    /// How long the game took to report `data-game-state="ready"`.
    pub ready_seconds: f64,
    /// The no-scroll assertion and its positive control.
    pub scroll: BrowserScrollFacts,
}

/// The canvas geometry the browser gate measured.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserCanvasFacts {
    /// The drawing buffer size, `[width, height]`.
    pub buffer: [u32; 2],
    /// The laid-out CSS height.
    pub height: u32,
    /// The laid-out CSS width.
    pub width: u32,
}

/// The canvas-region analysis the browser gate performed.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserPixelFacts {
    /// Approved palette classes that met the minimum share.
    pub palette_classes: Vec<String>,
    /// The sampled region, `[left, top, right, bottom]`.
    pub region: [u32; 4],
    /// How many pixels were sampled.
    pub sampled_pixels: u64,
    /// Share of sampled pixels matching no approved role.
    pub unmatched_share: f64,
    /// Per-channel variance across the sampled region.
    pub variance: [f64; 3],
}

/// The no-scroll assertion and the positive control that proves it can fail.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserScrollFacts {
    /// Scroll deltas recorded while the canvas held focus.
    pub focused_deltas: BTreeMap<String, f64>,
    /// The neutral element the positive control focused.
    pub probe: String,
    /// The scroll reserve the page kept below the fold.
    pub reserve_pixels: f64,
    /// Scroll deltas recorded while the neutral probe held focus.
    pub unfocused_deltas: BTreeMap<String, f64>,
}

// ---------------------------------------------------------------------------
// Sanitized public projection
// ---------------------------------------------------------------------------

/// One pixel rectangle, snapped onto the grid of the frame it belongs to.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicRect {
    /// Left edge, in pixels.
    pub x: u32,
    /// Top edge, in pixels.
    pub y: u32,
    /// Width, in pixels.
    pub width: u32,
    /// Height, in pixels.
    pub height: u32,
}

/// The render settings the one game camera carried.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicCamera {
    /// The display transform.
    pub tonemapping: String,
    /// The deband dither.
    pub deband_dither: String,
    /// Multisample count.
    pub msaa_samples: u32,
    /// The clear colour, as `#RRGGBB`.
    pub clear_color: String,
}

/// Every SHA-256 the run recorded, by repository-relative path.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicHashes {
    /// Generated assets.
    pub assets: BTreeMap<String, String>,
    /// Declarative asset sources.
    pub asset_sources: BTreeMap<String, String>,
    /// Approved references.
    pub references: BTreeMap<String, String>,
    /// Verification source files.
    pub sources: BTreeMap<String, String>,
}

/// The selected camera, ticket, and UI facts one captured frame publishes.
///
/// Everything a frame recorded about world positions, ground projections, key
/// messages, and interaction outcomes stays in the private report.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicFrame {
    /// The canonical frame file name.
    pub name: String,
    /// The artifact path the report declared, relative to its own directory.
    pub artifact: String,
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
    /// The status line the HUD showed.
    pub hud_status: String,
    /// How many tickets were open.
    pub open_tickets: usize,
    /// Every rack's state, in stable rack order.
    pub rack_states: Vec<String>,
    /// The projected technician crop, snapped onto the frame's pixel grid.
    pub worker_crop: PublicRect,
    /// How many authored equipment props projected into the viewport.
    pub equipment_on_screen: usize,
}

/// One published metric that missed its published bound.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicMetricFailure {
    /// The metric name, in the published vocabulary.
    pub metric: String,
    /// The measured value.
    pub value: f64,
    /// The bound it had to satisfy.
    pub expected: String,
}

/// What the headless browser proved about the packaged game.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicBrowser {
    /// How long the game took to report readiness.
    pub ready_seconds: f64,
    /// The measured canvas width.
    pub canvas_width: u32,
    /// The measured canvas height.
    pub canvas_height: u32,
    /// Approved palette classes present in the canvas region.
    pub palette_classes: Vec<String>,
    /// Share of sampled pixels matching no approved role.
    pub unmatched_share: f64,
    /// How many pixels were sampled.
    pub sampled_pixels: u64,
    /// The canvas screenshot, relative to the browser artifact directory.
    pub screenshot: Option<String>,
}

/// The strict public projection of one native report and one browser gate.
///
/// Nothing here carries a command line, a stream capture, a host or
/// environment value, an absolute path, or an undeclared field: the site is
/// generated from this projection alone, never from the raw documents.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationSummary {
    /// The report schema this projection was taken from.
    pub schema_version: u32,
    /// Whether the run succeeded and every published metric met its bound.
    pub succeeded: bool,
    /// The named stage a failed run failed in.
    pub failed_stage: Option<String>,
    /// Every stage the run entered, in order.
    pub stages: Vec<String>,
    /// SHA-256 of the game's own canonical semantic report.
    pub semantic_visual_hash: String,
    /// The render settings the one game camera carried.
    pub camera: PublicCamera,
    /// Every SHA-256 the run recorded.
    pub hashes: PublicHashes,
    /// Every captured frame, in capture order.
    pub frames: Vec<PublicFrame>,
    /// What the headless browser proved, when it ran.
    pub browser: Option<PublicBrowser>,
    /// Every published metric value, by name.
    pub metrics: BTreeMap<String, f64>,
    /// Every published metric that missed its bound.
    pub metric_failures: Vec<PublicMetricFailure>,
    /// The named gates this evidence proves, with counts and durations.
    pub gates: Vec<GateSummary>,
}

/// A sanitized projection together with the directories its artifacts live in.
#[derive(Clone, Debug, PartialEq)]
pub struct VerificationEvidence {
    /// The public projection.
    pub summary: VerificationSummary,
    /// The verified `--verify-output` directory the frames came from.
    pub artifacts: PathBuf,
    /// The browser gate diagnostics directory, when the gate ran.
    pub browser_artifacts: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SiteInputs {
    pub progress: ProgressDocument,
    pub plan_markdown: String,
    pub reference_manifest: ReferenceManifest,
    pub verification: Option<VerificationEvidence>,
    /// The screenshot history the previous `pages-live` publication left.
    pub gallery: Option<GalleryManifest>,
    pub workflow: WorkflowSummary,
    pub repo: RepoFacts,
    pub playable: Option<PlayableBuild>,
}

/// A packaged, browser-verified WASM build waiting to be published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayableBuild {
    /// Directory holding the packaged browser game.
    pub directory: PathBuf,
    /// The source commit the package was built from.
    pub source_commit: String,
    /// The workflow run that proved it in a browser.
    pub run_url: String,
}

/// Every file the packaged browser game must contain before it is published.
pub const REQUIRED_PLAYABLE_FILES: [&str; 5] = [
    "index.html",
    "play.js",
    "play.css",
    "game.js",
    "game_bg.wasm",
];

/// The directory holding the generated models the packaged game loads.
pub const REQUIRED_PLAYABLE_ASSETS: &str = "assets";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildDisposition {
    GreenReplacement,
    RetainLastGreen,
    FailedRetainLastGreen,
    FirstRunStatusOnly,
}

impl fmt::Display for BuildDisposition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GreenReplacement => formatter.write_str("GreenReplacement"),
            Self::RetainLastGreen => formatter.write_str("RetainLastGreen"),
            Self::FailedRetainLastGreen => formatter.write_str("FailedRetainLastGreen"),
            Self::FirstRunStatusOnly => formatter.write_str("FirstRunStatusOnly"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SiteManifest {
    pub source_commit: String,
    pub playable_commit: Option<String>,
    pub current_task: Option<String>,
    pub generated_files: Vec<PathBuf>,
    pub semantic_visual_hash: Option<String>,
}

/// One accepted point in the published visual history.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GalleryEntry {
    /// The semantic hash of the canonical report this entry was accepted for.
    pub semantic_visual_hash: String,
    /// The source commit that produced it.
    pub source_commit: String,
    /// When that commit was committed.
    pub committed_at: String,
    /// The task that was current when it was published.
    pub current_task: String,
    /// The published frames, by stable label.
    pub frames: BTreeMap<String, String>,
    /// Every published metric value at this point.
    pub metrics: BTreeMap<String, f64>,
    /// The change in each metric from the previous entry.
    pub metric_deltas: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GalleryManifest {
    pub entries: Vec<GalleryEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LastGreenManifest {
    pub source_commit: String,
    pub semantic_visual_hash: Option<String>,
    pub game_files: Vec<PathBuf>,
    pub screenshot_files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgressError {
    UnsupportedSchemaVersion {
        actual: u32,
    },
    DuplicateTaskId {
        task_id: String,
    },
    UnknownPlanTask {
        task_id: String,
    },
    MultipleCurrentTasks,
    MissingCurrentTask,
    DependencyNotDone {
        task_id: String,
        dependency_id: String,
    },
    MissingCompletionCommit {
        task_id: String,
    },
    UnknownCompletionCommit {
        task_id: String,
        commit: String,
    },
    UnexpectedCompletionCommit {
        task_id: String,
    },
    MissingChallengeContext {
        challenge_id: String,
        field: String,
    },
    /// A resolved challenge names a commit this repository does not have.
    UnknownChallengeCommit {
        challenge_id: String,
        commit: String,
    },
    MissingChallengeResolution {
        challenge_id: String,
    },
}

impl fmt::Display for ProgressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { actual } => {
                write!(formatter, "unsupported schema version {actual}; expected 1")
            }
            Self::DuplicateTaskId { task_id } => write!(formatter, "duplicate task id: {task_id}"),
            Self::UnknownPlanTask { task_id } => {
                write!(formatter, "task id is not in the reviewed plan: {task_id}")
            }
            Self::MultipleCurrentTasks => write!(formatter, "multiple tasks are in progress"),
            Self::MissingCurrentTask => write!(formatter, "no task is in progress"),
            Self::DependencyNotDone {
                task_id,
                dependency_id,
            } => write!(
                formatter,
                "task {task_id} started before dependency {dependency_id} was done"
            ),
            Self::MissingCompletionCommit { task_id } => {
                write!(formatter, "done task {task_id} has no completion commit")
            }
            Self::UnknownCompletionCommit { task_id, commit } => {
                write!(
                    formatter,
                    "task {task_id} references unknown commit {commit}"
                )
            }
            Self::UnexpectedCompletionCommit { task_id } => {
                write!(
                    formatter,
                    "unfinished task {task_id} has a completion commit"
                )
            }
            Self::MissingChallengeContext {
                challenge_id,
                field,
            } => write!(formatter, "challenge {challenge_id} is missing {field}"),
            Self::UnknownChallengeCommit {
                challenge_id,
                commit,
            } => write!(
                formatter,
                "challenge {challenge_id} references unknown commit {commit}"
            ),
            Self::MissingChallengeResolution { challenge_id } => {
                write!(
                    formatter,
                    "resolved challenge {challenge_id} has no resolution"
                )
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceError {
    AssetCount {
        expected: usize,
        actual: usize,
    },
    ManifestFieldMismatch {
        asset: String,
        field: String,
        expected: String,
        actual: String,
    },
    AssetRead {
        path: PathBuf,
        message: String,
    },
    AssetHashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    AssetDimensionsMismatch {
        path: PathBuf,
        expected: (u32, u32),
        actual: (u32, u32),
    },
}

impl fmt::Display for ReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AssetCount { expected, actual } => {
                write!(
                    formatter,
                    "reference manifest has {actual} assets; expected {expected}"
                )
            }
            Self::ManifestFieldMismatch {
                asset,
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "reference {asset} has invalid {field}: expected {expected}, got {actual}"
            ),
            Self::AssetRead { path, message } => {
                write!(
                    formatter,
                    "could not read reference {}: {message}",
                    path.display()
                )
            }
            Self::AssetHashMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "reference {} has SHA-256 {actual}; expected {expected}",
                path.display()
            ),
            Self::AssetDimensionsMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "reference {} is {}x{}; expected {}x{}",
                path.display(),
                actual.0,
                actual.1,
                expected.0,
                expected.1
            ),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum SitegenError {
    Io {
        path: PathBuf,
        message: String,
    },
    Json {
        path: PathBuf,
        message: String,
    },
    Progress(Vec<ProgressError>),
    Reference(Vec<ReferenceError>),
    Markdown {
        path: PathBuf,
        message: String,
    },
    MissingInput {
        path: PathBuf,
    },
    UnsafeOutputPath {
        path: PathBuf,
    },
    BrokenLocalLink {
        source: PathBuf,
        target: PathBuf,
    },
    MissingAltText {
        source: PathBuf,
    },
    InvalidHtml {
        path: PathBuf,
        message: String,
    },
    MissingPreviousArtifact {
        path: PathBuf,
    },
    OutputNotEmpty {
        path: PathBuf,
    },
    UntrustedPlayablePackage {
        path: PathBuf,
    },
    IncompletePlayablePackage {
        path: PathBuf,
        missing: Vec<String>,
    },
    /// A verification artifact left the directory that declared it.
    UntrustedArtifact {
        path: PathBuf,
    },
    /// A verification artifact is not the image its report says it is.
    CorruptArtifact {
        path: PathBuf,
        message: String,
    },
    /// A report's reference hash disagrees with the approved manifest.
    ReferenceProvenance {
        path: String,
        expected: String,
        actual: String,
    },
    /// A workflow result manifest carries a value the site may not publish.
    UnsafeResultValue {
        field: String,
        message: String,
    },
    /// The assembled tree publishes a history manifest naming images no
    /// previous publication supplied.
    MissingRetainedHistory {
        targets: Vec<String>,
    },
    /// The published history names an image outside the directory of the entry
    /// that declared it.
    HistoryFrameOutsideEntry {
        frames: Vec<String>,
    },
    /// The current tree carries promoted frames without the gallery manifest
    /// that always accompanies a real promotion.
    PartialEvidencePublication {
        path: PathBuf,
    },
}

impl fmt::Display for SitegenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => write!(formatter, "{}: {message}", path.display()),
            Self::Json { path, message } => write!(formatter, "{}: {message}", path.display()),
            Self::Progress(errors) => {
                write!(formatter, "progress validation failed")?;
                for error in errors {
                    write!(formatter, ": {error}")?;
                }
                Ok(())
            }
            Self::Reference(errors) => {
                write!(formatter, "reference validation failed")?;
                for error in errors {
                    write!(formatter, ": {error}")?;
                }
                Ok(())
            }
            Self::Markdown { path, message } => {
                write!(
                    formatter,
                    "could not render Markdown {}: {message}",
                    path.display()
                )
            }
            Self::MissingInput { path } => {
                write!(formatter, "required input is missing: {}", path.display())
            }
            Self::UnsafeOutputPath { path } => {
                write!(formatter, "refusing unsafe output path: {}", path.display())
            }
            Self::BrokenLocalLink { source, target } => write!(
                formatter,
                "{} links to missing local target {}",
                source.display(),
                target.display()
            ),
            Self::MissingAltText { source } => {
                write!(
                    formatter,
                    "{} contains an image without alt text",
                    source.display()
                )
            }
            Self::InvalidHtml { path, message } => {
                write!(
                    formatter,
                    "{} contains invalid HTML: {message}",
                    path.display()
                )
            }
            Self::MissingPreviousArtifact { path } => {
                write!(
                    formatter,
                    "previous artifact is missing: {}",
                    path.display()
                )
            }
            Self::OutputNotEmpty { path } => {
                write!(
                    formatter,
                    "assembly output directory is not empty: {}",
                    path.display()
                )
            }
            Self::UntrustedPlayablePackage { path } => {
                write!(
                    formatter,
                    "playable package is not inside a trusted build root: {}",
                    path.display()
                )
            }
            Self::IncompletePlayablePackage { path, missing } => {
                write!(
                    formatter,
                    "playable package {} is incomplete: missing {}",
                    path.display(),
                    missing.join(", ")
                )
            }
            Self::UntrustedArtifact { path } => write!(
                formatter,
                "verification artifact is not inside its own report directory: {}",
                path.display()
            ),
            Self::CorruptArtifact { path, message } => write!(
                formatter,
                "verification artifact {} is unusable: {message}",
                path.display()
            ),
            Self::ReferenceProvenance {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "report records {path} as {actual}; the approved reference is {expected}"
            ),
            Self::UnsafeResultValue { field, message } => {
                write!(
                    formatter,
                    "workflow result field {field} is unsafe: {message}"
                )
            }
            Self::MissingRetainedHistory { targets } => write!(
                formatter,
                "the published history names {} image(s) no previous publication supplied: {}",
                targets.len(),
                targets.join(", ")
            ),
            Self::HistoryFrameOutsideEntry { frames } => write!(
                formatter,
                "the published history names {} image(s) outside the entry that declared them: {}",
                frames.len(),
                frames.join(", ")
            ),
            Self::PartialEvidencePublication { path } => write!(
                formatter,
                "{} is published without the gallery manifest that must accompany a promotion",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SitegenError {}

pub fn validate_progress(
    document: &ProgressDocument,
    plan_task_ids: &BTreeSet<String>,
    repo: &RepoFacts,
) -> Result<(), Vec<ProgressError>> {
    let mut errors = Vec::new();

    if document.schema_version != 1 {
        errors.push(ProgressError::UnsupportedSchemaVersion {
            actual: document.schema_version,
        });
    }

    let statuses = document
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task.status))
        .collect::<BTreeMap<_, _>>();
    let mut seen_ids = BTreeSet::new();

    for task in &document.tasks {
        if !seen_ids.insert(task.id.as_str()) {
            errors.push(ProgressError::DuplicateTaskId {
                task_id: task.id.clone(),
            });
        }

        if !plan_task_ids.contains(&task.id) {
            errors.push(ProgressError::UnknownPlanTask {
                task_id: task.id.clone(),
            });
        }

        if task.status != ProgressStatus::Future {
            for dependency_id in &task.depends_on {
                if statuses.get(dependency_id.as_str()) != Some(&ProgressStatus::Done) {
                    errors.push(ProgressError::DependencyNotDone {
                        task_id: task.id.clone(),
                        dependency_id: dependency_id.clone(),
                    });
                }
            }
        }

        match (&task.status, &task.completed_commit) {
            (ProgressStatus::Done, None) => {
                errors.push(ProgressError::MissingCompletionCommit {
                    task_id: task.id.clone(),
                });
            }
            (ProgressStatus::Done, Some(commit)) if resolve_commit_ref(commit, repo).is_none() => {
                errors.push(ProgressError::UnknownCompletionCommit {
                    task_id: task.id.clone(),
                    commit: commit.clone(),
                });
            }
            (ProgressStatus::Future | ProgressStatus::InProgress, Some(_)) => {
                errors.push(ProgressError::UnexpectedCompletionCommit {
                    task_id: task.id.clone(),
                });
            }
            _ => {}
        }
    }

    let current_count = document
        .tasks
        .iter()
        .filter(|task| task.status == ProgressStatus::InProgress)
        .count();
    let all_done = document
        .tasks
        .iter()
        .all(|task| task.status == ProgressStatus::Done);
    if current_count > 1 {
        errors.push(ProgressError::MultipleCurrentTasks);
    } else if current_count == 0 && !all_done {
        errors.push(ProgressError::MissingCurrentTask);
    }

    for challenge in &document.challenges {
        for (field, value) in [
            ("impact", challenge.impact.as_str()),
            ("approach", challenge.approach.as_str()),
        ] {
            if value.trim().is_empty() {
                errors.push(ProgressError::MissingChallengeContext {
                    challenge_id: challenge.id.clone(),
                    field: field.to_owned(),
                });
            }
        }

        if challenge.status == ChallengeStatus::Resolved {
            if challenge
                .resolution
                .as_deref()
                .is_none_or(|resolution| resolution.trim().is_empty())
            {
                errors.push(ProgressError::MissingChallengeResolution {
                    challenge_id: challenge.id.clone(),
                });
            }
            if challenge
                .resolved_commit
                .as_deref()
                .is_none_or(|commit| commit.trim().is_empty())
            {
                errors.push(ProgressError::MissingChallengeContext {
                    challenge_id: challenge.id.clone(),
                    field: "resolved_commit".to_owned(),
                });
            } else if let Some(commit) = challenge.resolved_commit.as_deref()
                && resolve_commit_ref(commit, repo).is_none()
            {
                // A commit nobody can find is a wrong reference to chase, not
                // an empty field to fill in, and saying "missing" sends the
                // reader looking for a value that is right there.
                errors.push(ProgressError::UnknownChallengeCommit {
                    challenge_id: challenge.id.clone(),
                    commit: commit.to_owned(),
                });
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub fn resolve_commit_ref(commit: &str, repo: &RepoFacts) -> Option<String> {
    if commit == "HEAD" {
        return Some(repo.head_sha.clone());
    }

    (commit.len() == 40
        && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        && repo.known_commits.contains(commit))
    .then(|| commit.to_owned())
}

pub fn plan_task_ids_from_markdown(markdown: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let mut heading = None;

    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Heading { .. }) => heading = Some(String::new()),
            Event::Text(text) | Event::Code(text) => {
                if let Some(value) = &mut heading {
                    value.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(value) = heading.take() {
                    add_heading_task_ids(value.trim(), &mut ids);
                }
            }
            _ => {}
        }
    }

    ids
}

fn add_heading_task_ids(heading: &str, ids: &mut BTreeSet<String>) {
    ids.extend(
        task_ids_for_heading(heading)
            .iter()
            .map(|id| (*id).to_owned()),
    );
}

fn task_ids_for_heading(heading: &str) -> &'static [&'static str] {
    match heading {
        "Task 1: Establish the project and reviewed contracts" => &["foundation-contracts"][..],
        "Pages Milestone A: Publish the status-only progress hub" => &["pages-foundation"][..],
        "Task 2: Build the autonomous no-Blender asset pipeline" => &["autonomous-assets"][..],
        "Task 3: Load assets and build the data hall" => &["data-hall"][..],
        "Task 4: Add the rigged technician and real keyboard movement" => {
            &["technician-movement"][..]
        }
        "Task 5: Add clamped four-way camera orbit" => &["camera-orbit"][..],
        "Task 6: Add recurring faults, prioritized tickets, and repair" => &["operations-loop"][..],
        "Task 7: Add operations HUD and diegetic badges" => &["operations-hud"][..],
        "Pages Milestone B: Publish the playable WASM game" => &["pages-playable"][..],
        "Task 8: Build the autonomous verification and visual hill-climb gate" => {
            &["autonomous-verification"][..]
        }
        "Pages Milestone C: Publish comparisons, evidence, and last-green retention" => {
            &["pages-verification", "pages-status-always"][..]
        }
        "Task 9: Add CI and publish the reproducible POC baseline" => &["ci-baseline"][..],
        _ => &[],
    }
}

pub fn validate_reference_manifest(
    manifest: &ReferenceManifest,
    repository: &Path,
) -> Result<(), Vec<ReferenceError>> {
    const EXPECTED_DIMENSIONS: (u32, u32) = (1536, 1024);
    const EXPECTED: [(&str, &str, &str, &str); 2] = [
        (
            "Cel Shift key art",
            "../midcreek-concept/themes/cel-shift/masters/key-art/04-diamond-bright.png",
            KEY_ART_REFERENCE_PATH,
            KEY_ART_SHA256,
        ),
        (
            "Cel Shift character sheet",
            "../midcreek-concept/themes/cel-shift/masters/animation/01-model-sheet.png",
            CHARACTER_SHEET_REFERENCE_PATH,
            CHARACTER_SHEET_SHA256,
        ),
    ];

    let mut errors = Vec::new();
    if manifest.assets.len() != EXPECTED.len() {
        errors.push(ReferenceError::AssetCount {
            expected: EXPECTED.len(),
            actual: manifest.assets.len(),
        });
    }

    for (asset, (name, source_path, public_path, sha256)) in manifest.assets.iter().zip(EXPECTED) {
        for (field, expected, actual) in [
            ("name", name, asset.name.as_str()),
            ("source_path", source_path, asset.source_path.as_str()),
            ("public_path", public_path, asset.public_path.as_str()),
            ("sha256", sha256, asset.sha256.as_str()),
        ] {
            if actual != expected {
                errors.push(ReferenceError::ManifestFieldMismatch {
                    asset: name.to_owned(),
                    field: field.to_owned(),
                    expected: expected.to_owned(),
                    actual: actual.to_owned(),
                });
            }
        }

        if (asset.width, asset.height) != EXPECTED_DIMENSIONS {
            errors.push(ReferenceError::ManifestFieldMismatch {
                asset: name.to_owned(),
                field: "dimensions".to_owned(),
                expected: format!("{}x{}", EXPECTED_DIMENSIONS.0, EXPECTED_DIMENSIONS.1),
                actual: format!("{}x{}", asset.width, asset.height),
            });
        }

        if asset.public_path != public_path {
            continue;
        }

        let relative_path = Path::new(public_path);
        let full_path = repository.join(relative_path);
        let bytes = match fs::read(&full_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                errors.push(ReferenceError::AssetRead {
                    path: relative_path.to_path_buf(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        let actual_hash = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual_hash != sha256 {
            errors.push(ReferenceError::AssetHashMismatch {
                path: relative_path.to_path_buf(),
                expected: sha256.to_owned(),
                actual: actual_hash,
            });
        }

        let dimensions = ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|error| error.to_string())
            .and_then(|reader| reader.into_dimensions().map_err(|error| error.to_string()));
        match dimensions {
            Ok(actual) if actual != EXPECTED_DIMENSIONS => {
                errors.push(ReferenceError::AssetDimensionsMismatch {
                    path: relative_path.to_path_buf(),
                    expected: EXPECTED_DIMENSIONS,
                    actual,
                });
            }
            Ok(_) => {}
            Err(error) => errors.push(ReferenceError::AssetRead {
                path: relative_path.to_path_buf(),
                message: error.to_string(),
            }),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------------
// Sanitized projection of the verification evidence
// ---------------------------------------------------------------------------

impl VerificationEvidence {
    /// Projects one raw native report, and one optional browser gate summary,
    /// onto the strict public shape the site is generated from.
    ///
    /// Every declared artifact is resolved inside the directory that declared
    /// it, refused if it is absolute, escapes with `..`, is a symbolic link,
    /// is missing, or is not the image its own report says it is. The approved
    /// reference hashes are checked against the repository's own constants, so
    /// a report that measured a different key art can never be published
    /// beside it.
    pub fn project(
        report: &VerificationReport,
        artifacts: &Path,
        browser: Option<(&BrowserGateReport, &Path)>,
    ) -> Result<Self, SitegenError> {
        check_reference_provenance(report)?;

        let mut frames = Vec::new();
        for name in FrameName::ALL {
            let Some(facts) = report.frames.get(name.file_name()) else {
                continue;
            };
            let path = resolve_artifact(artifacts, &facts.path)?;
            let dimensions = image_dimensions(&path)?;
            if dimensions != (facts.width, facts.height) {
                return Err(SitegenError::CorruptArtifact {
                    path,
                    message: format!(
                        "the report declares {}x{}, the image is {}x{}",
                        facts.width, facts.height, dimensions.0, dimensions.1
                    ),
                });
            }
            frames.push(public_frame(name, facts));
        }

        let browser_artifacts = browser.map(|(_, root)| root.to_path_buf());
        let browser = match browser {
            Some((gate, root)) => Some(project_browser(gate, root)?),
            None => None,
        };
        let metrics = collect_metrics(report, &frames, browser.as_ref());
        let metric_failures = collect_metric_failures(&metrics);
        let succeeded = report.result == "success" && metric_failures.is_empty();
        let gates = derive_gates(
            succeeded,
            &frames,
            &metric_failures,
            report,
            browser.as_ref(),
        );

        Ok(Self {
            summary: VerificationSummary {
                schema_version: report.schema_version,
                succeeded,
                failed_stage: report.failed_stage.clone(),
                stages: report.stages.clone(),
                semantic_visual_hash: semantic_hash(&canonical_json(report)),
                camera: PublicCamera {
                    tonemapping: report.camera.tonemapping.clone(),
                    deband_dither: report.camera.deband_dither.clone(),
                    msaa_samples: report.camera.msaa_samples,
                    clear_color: report.camera.clear_color.clone(),
                },
                hashes: PublicHashes {
                    assets: report.assets.clone(),
                    asset_sources: report.asset_sources.clone(),
                    references: report.references.clone(),
                    sources: report.sources.clone(),
                },
                frames,
                browser,
                metrics,
                metric_failures,
                gates,
            },
            artifacts: artifacts.to_path_buf(),
            browser_artifacts,
        })
    }
}

fn public_frame(name: FrameName, facts: &FrameFacts) -> PublicFrame {
    let crop = PixelRect::snap(facts.worker_crop, facts.width, facts.height);
    PublicFrame {
        name: name.file_name().to_owned(),
        artifact: facts.path.clone(),
        width: facts.width,
        height: facts.height,
        stage: facts.stage.clone(),
        heading: facts.heading.clone(),
        camera_yaw_degrees: facts.camera_yaw_degrees,
        camera_settled: facts.camera_settled,
        hud_status: facts.hud_status.clone(),
        open_tickets: facts.tickets.len(),
        rack_states: facts.rack_states.clone(),
        worker_crop: PublicRect {
            x: crop.x,
            y: crop.y,
            width: crop.width,
            height: crop.height,
        },
        equipment_on_screen: facts.equipment.iter().filter(|prop| prop.on_screen).count(),
    }
}

fn project_browser(gate: &BrowserGateReport, root: &Path) -> Result<PublicBrowser, SitegenError> {
    let screenshot = match resolve_artifact(root, BROWSER_CANVAS_ARTIFACT) {
        Ok(path) => {
            image_dimensions(&path)?;
            Some(BROWSER_CANVAS_ARTIFACT.to_owned())
        }
        // A gate that reported without keeping its canvas screenshot is still
        // publishable evidence; a canvas that is there but unreadable is not.
        Err(SitegenError::MissingInput { .. }) => None,
        Err(error) => return Err(error),
    };
    Ok(PublicBrowser {
        ready_seconds: gate.ready_seconds,
        canvas_width: gate.canvas.width,
        canvas_height: gate.canvas.height,
        palette_classes: gate.pixels.palette_classes.clone(),
        unmatched_share: gate.pixels.unmatched_share,
        sampled_pixels: gate.pixels.sampled_pixels,
        screenshot,
    })
}

/// Refuses a report whose approved-reference hashes are not the ones this
/// repository vendors and publishes.
fn check_reference_provenance(report: &VerificationReport) -> Result<(), SitegenError> {
    for (path, expected) in [
        (KEY_ART_REFERENCE_PATH, KEY_ART_SHA256),
        (CHARACTER_SHEET_REFERENCE_PATH, CHARACTER_SHEET_SHA256),
    ] {
        let actual = report.references.get(path).map(String::as_str);
        if actual != Some(expected) {
            return Err(SitegenError::ReferenceProvenance {
                path: path.to_owned(),
                expected: expected.to_owned(),
                actual: actual.unwrap_or("nothing").to_owned(),
            });
        }
    }
    Ok(())
}

/// Resolves one declared artifact strictly inside the directory that declared
/// it.
fn resolve_artifact(root: &Path, relative: &str) -> Result<PathBuf, SitegenError> {
    let declared = Path::new(relative);
    let untrusted = || SitegenError::UntrustedArtifact {
        path: PathBuf::from(relative),
    };
    if relative.is_empty()
        || declared.is_absolute()
        || relative.contains('\\')
        || declared
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(untrusted());
    }

    let candidate = root.join(declared);
    match fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => return Err(untrusted()),
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) | Err(_) if !candidate.is_file() => {
            return Err(SitegenError::MissingInput { path: candidate });
        }
        _ => {}
    }

    // A symbolic link anywhere above the artifact would let a contained
    // relative path still resolve outside the directory that declared it.
    let canonical_root = fs::canonicalize(root).map_err(|error| SitegenError::Io {
        path: root.to_path_buf(),
        message: error.to_string(),
    })?;
    let canonical = fs::canonicalize(&candidate).map_err(|error| SitegenError::Io {
        path: candidate.clone(),
        message: error.to_string(),
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(untrusted());
    }
    Ok(candidate)
}

fn image_dimensions(path: &Path) -> Result<(u32, u32), SitegenError> {
    ImageReader::open(path)
        .map_err(|error| error.to_string())
        .and_then(|reader| {
            reader
                .with_guessed_format()
                .map_err(|error| error.to_string())
        })
        .and_then(|reader| reader.into_dimensions().map_err(|error| error.to_string()))
        .map_err(|message| SitegenError::CorruptArtifact {
            path: path.to_path_buf(),
            message,
        })
}

/// Every published metric value, by name.
///
/// The vocabulary is fixed: nothing is copied out of the raw documents by key,
/// so a field that appears in a future report cannot appear on the site until
/// it is named here.
fn collect_metrics(
    report: &VerificationReport,
    frames: &[PublicFrame],
    browser: Option<&PublicBrowser>,
) -> BTreeMap<String, f64> {
    let mut metrics = BTreeMap::new();
    let mut record = |name: &str, value: f64| {
        metrics.insert(name.to_owned(), value);
    };

    record("render.frames-captured", frames.len() as f64);
    record("render.stages", report.stages.len() as f64);
    record(
        "render.blueprint-validation-errors",
        report.blueprint.validation_errors.len() as f64,
    );
    record("hall.rack-rows", report.blueprint.rack_rows as f64);
    record("hall.aisles", report.blueprint.aisles as f64);
    record("hall.visuals", report.blueprint.visuals as f64);
    record("hall.colliders", report.blueprint.colliders as f64);
    record("camera.msaa-samples", f64::from(report.camera.msaa_samples));
    record(
        "gameplay.tickets-emitted",
        report.gameplay.tickets_emitted as f64,
    );
    record(
        "gameplay.capacity-pauses",
        report.gameplay.capacity_pauses as f64,
    );
    record(
        "gameplay.duplicate-pauses",
        report.gameplay.duplicate_pauses as f64,
    );
    record("gameplay.busy-pauses", report.gameplay.busy_pauses as f64);
    record(
        "tickets.peak-open",
        frames
            .iter()
            .map(|frame| frame.open_tickets)
            .max()
            .unwrap_or_default() as f64,
    );

    if let Some(centre) = frames.first() {
        record("camera.yaw-degrees", centre.camera_yaw_degrees);
        record(
            "worker.crop-pixels",
            f64::from(centre.worker_crop.width) * f64::from(centre.worker_crop.height),
        );
        record("worker.crop-width", f64::from(centre.worker_crop.width));
        record("worker.crop-height", f64::from(centre.worker_crop.height));
        record("equipment.on-screen", centre.equipment_on_screen as f64);
    }

    if let Some(browser) = browser {
        record("browser.ready-seconds", browser.ready_seconds);
        record("browser.canvas-width", f64::from(browser.canvas_width));
        record("browser.canvas-height", f64::from(browser.canvas_height));
        record(
            "browser.palette-classes",
            browser.palette_classes.len() as f64,
        );
        record("browser.unmatched-share", browser.unmatched_share);
    }
    metrics
}

/// The published bound each published metric has to meet.
const METRIC_BOUNDS: [(&str, MetricBound, &str); 8] = [
    (
        "render.frames-captured",
        MetricBound::Exactly(14.0),
        "all fourteen reviewed captures",
    ),
    (
        "render.blueprint-validation-errors",
        MetricBound::Exactly(0.0),
        "no blueprint validation errors",
    ),
    (
        "camera.msaa-samples",
        MetricBound::Exactly(1.0),
        "multisampling off",
    ),
    (
        "worker.crop-pixels",
        MetricBound::AtLeast(1.0),
        "a projected technician crop with area",
    ),
    (
        "browser.palette-classes",
        MetricBound::AtLeast(MINIMUM_PALETTE_CLASSES as f64),
        "at least three approved palette classes in the canvas",
    ),
    (
        "browser.ready-seconds",
        MetricBound::AtMost(BROWSER_READY_LIMIT_SECONDS),
        "readiness inside the browser gate's own timeout",
    ),
    (
        "browser.canvas-width",
        MetricBound::AtLeast(1.0),
        "a canvas with width",
    ),
    (
        "browser.canvas-height",
        MetricBound::AtLeast(1.0),
        "a canvas with height",
    ),
];

/// How many approved palette classes a published canvas has to show.
const MINIMUM_PALETTE_CLASSES: usize = 3;

/// The longest a published browser gate may have waited for readiness.
///
/// This is the browser gate's own `READY_TIMEOUT_SECONDS`. A report that
/// records a longer wait than the gate would ever have allowed did not come
/// from a passing gate, whatever else it says about itself.
const BROWSER_READY_LIMIT_SECONDS: f64 = 30.0;

#[derive(Clone, Copy)]
enum MetricBound {
    Exactly(f64),
    AtLeast(f64),
    AtMost(f64),
}

impl MetricBound {
    fn holds(self, value: f64) -> bool {
        match self {
            Self::Exactly(expected) => (value - expected).abs() < f64::EPSILON,
            Self::AtLeast(minimum) => value >= minimum,
            Self::AtMost(maximum) => value <= maximum,
        }
    }
}

fn collect_metric_failures(metrics: &BTreeMap<String, f64>) -> Vec<PublicMetricFailure> {
    METRIC_BOUNDS
        .into_iter()
        .filter_map(|(metric, bound, expected)| {
            let value = *metrics.get(metric)?;
            (!bound.holds(value)).then(|| PublicMetricFailure {
                metric: metric.to_owned(),
                value,
                expected: expected.to_owned(),
            })
        })
        .collect()
}

fn derive_gates(
    succeeded: bool,
    frames: &[PublicFrame],
    failures: &[PublicMetricFailure],
    report: &VerificationReport,
    browser: Option<&PublicBrowser>,
) -> Vec<GateSummary> {
    let status = |passed: bool| {
        if passed {
            GateStatus::Passed
        } else {
            GateStatus::Failed
        }
    };
    let mut gates = vec![
        GateSummary {
            // The run's own report vouches for the frames it captured. It does
            // not carry the reference image analyzers' verdict, so this gate
            // never claims one.
            name: "Verified frame captures".to_owned(),
            status: status(succeeded),
            passed: frames.len() as u32,
            failed: failures.len() as u32,
            duration_ms: 0,
            artifact_url: None,
        },
        GateSummary {
            name: "Verification stages".to_owned(),
            status: status(report.failed_stage.is_none()),
            passed: report.stages.len() as u32,
            failed: u32::from(report.failed_stage.is_some()),
            duration_ms: 0,
            artifact_url: None,
        },
    ];
    if let Some(browser) = browser {
        // Readiness is a measurement, not the existence of a report: a gate
        // that waited longer than its own timeout allows never proved the game
        // was ready, so the row says so instead of turning green because a
        // document arrived.
        let ready = MetricBound::AtMost(BROWSER_READY_LIMIT_SECONDS).holds(browser.ready_seconds);
        gates.push(GateSummary {
            name: "Browser readiness".to_owned(),
            status: status(ready),
            passed: u32::from(ready),
            failed: u32::from(!ready),
            duration_ms: (browser.ready_seconds * 1_000.0).round().max(0.0) as u64,
            artifact_url: None,
        });
        // A failed palette row that reports zero failures says a red gate
        // found nothing wrong. The one thing it found wrong is the canvas.
        let palette = browser.palette_classes.len() >= MINIMUM_PALETTE_CLASSES;
        gates.push(GateSummary {
            name: "Browser canvas palette".to_owned(),
            status: status(palette),
            passed: browser.palette_classes.len() as u32,
            failed: u32::from(!palette),
            duration_ms: 0,
            artifact_url: None,
        });
    }
    gates
}

// ---------------------------------------------------------------------------
// Screenshot history
// ---------------------------------------------------------------------------

/// Decides whether one verified run earns a new point in the visual history.
///
/// A run only ever appends: a failed run, a run whose semantic hash matches the
/// latest entry, and a rerun of a commit that is already recorded all return
/// the previous history untouched, so a documentation-only push cannot
/// duplicate a screenshot and a failure cannot erase one.
pub fn update_gallery(
    previous: &GalleryManifest,
    summary: &VerificationSummary,
    commit: &CommitSummary,
) -> GalleryManifest {
    let latest = previous.entries.last();
    let unchanged =
        latest.is_some_and(|entry| entry.semantic_visual_hash == summary.semantic_visual_hash);
    let recorded = previous
        .entries
        .iter()
        .any(|entry| entry.source_commit == commit.sha);
    if !summary.succeeded || unchanged || recorded {
        return previous.clone();
    }

    let short = short_sha(&commit.sha);
    let mut frames = GALLERY_FRAMES
        .into_iter()
        .map(|(label, file)| {
            (
                label.to_owned(),
                format!("{HISTORY_SCREENSHOTS}/{short}/{file}"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    frames.insert(
        WORKER_FRAME_LABEL.to_owned(),
        format!("{HISTORY_SCREENSHOTS}/{short}/{WORKER_CROP_FILE}"),
    );

    let metric_deltas = latest.map_or_else(BTreeMap::new, |entry| {
        summary
            .metrics
            .iter()
            .filter_map(|(name, value)| {
                entry
                    .metrics
                    .get(name)
                    .map(|before| (name.clone(), value - before))
            })
            .collect()
    });

    let mut entries = previous.entries.clone();
    entries.push(GalleryEntry {
        semantic_visual_hash: summary.semantic_visual_hash.clone(),
        source_commit: commit.sha.clone(),
        committed_at: commit.committed_at.clone(),
        current_task: commit
            .task_id
            .clone()
            .unwrap_or_else(|| "unassigned".to_owned()),
        frames,
        metrics: summary.metrics.clone(),
        metric_deltas,
    });
    GalleryManifest { entries }
}

pub fn build_site(inputs: &SiteInputs, output: &Path) -> Result<SiteManifest, SitegenError> {
    build_site_in(&default_repository(), inputs, output)
}

/// The repository this generator publishes from when a caller declares none.
///
/// A binary still sitting in the checkout it was built from finds that
/// checkout here. A relocated one does not, which is exactly why every
/// repository-relative decision is taken from the root the caller declares and
/// this is only the fallback.
pub fn default_repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Generates the site from one declared repository.
///
/// The approved references, the trusted build roots, and the source tree the
/// output may not be written into are all read from `repository`, so a
/// relocated `sitegen` publishes from the checkout it was handed rather than
/// from the path it happened to be compiled in.
pub fn build_site_in(
    repository: &Path,
    inputs: &SiteInputs,
    output: &Path,
) -> Result<SiteManifest, SitegenError> {
    validate_progress(
        &inputs.progress,
        &plan_task_ids_from_markdown(&inputs.plan_markdown),
        &inputs.repo,
    )
    .map_err(SitegenError::Progress)?;
    validate_reference_manifest(&inputs.reference_manifest, repository)
        .map_err(SitegenError::Reference)?;
    prepare_output(repository, output)?;

    let plan_html = render_plan_html(&inputs.plan_markdown);
    let current_task = inputs
        .progress
        .tasks
        .iter()
        .find(|task| task.status == ProgressStatus::InProgress);
    let source_commit = resolve_commit_ref(&inputs.workflow.source_commit, &inputs.repo)
        .unwrap_or_else(|| inputs.workflow.source_commit.clone());
    let updated_at = inputs
        .repo
        .commits
        .iter()
        .find(|commit| commit.sha == source_commit)
        .or_else(|| inputs.repo.commits.first())
        .map_or("Unknown", |commit| commit.committed_at.as_str());
    let reference_paths = copy_reference_assets(repository, &inputs.reference_manifest, output)?;
    let playable_files = copy_playable_build(repository, inputs.playable.as_ref(), output)?;
    let evidence = publish_verification(inputs, &source_commit, updated_at, current_task, output)?;
    // The manifest is written last, because only a completed promotion knows
    // which pixels this build actually published.
    write_last_green(inputs.playable.as_ref(), evidence.as_ref(), output)?;

    let replacements = [
        ("{{PROJECT}}", escape_html(&inputs.progress.project)),
        (
            "{{STATUS}}",
            render_status(inputs, current_task, &source_commit, updated_at),
        ),
        (
            "{{PLAY}}",
            mark_reconcilable("play", &render_play(inputs.playable.as_ref())),
        ),
        (
            "{{MODE}}",
            mark_reconcilable(
                "mode",
                &render_mode(inputs.playable.is_some(), evidence.is_some()),
            ),
        ),
        (
            "{{COMPARISON}}",
            render_comparison(
                &inputs.reference_manifest,
                &reference_paths,
                evidence.as_ref(),
            ),
        ),
        (
            "{{PROGRESS}}",
            render_progress(&inputs.progress, &inputs.repo),
        ),
        (
            "{{SCREENSHOTS}}",
            render_screenshots(inputs, evidence.as_ref()),
        ),
        ("{{PLAN}}", plan_html),
        (
            "{{CHALLENGES}}",
            render_challenges(&inputs.progress, &inputs.repo),
        ),
        ("{{TESTS}}", render_tests(inputs)),
        ("{{COMMITS}}", render_commits(&inputs.repo)),
    ];
    let html = render_template(include_str!("../site/templates/index.html"), &replacements);

    write_file(&output.join("index.html"), html.as_bytes())?;
    write_file(
        &output.join("site.css"),
        include_bytes!("../site/static/site.css"),
    )?;
    write_file(
        &output.join("site.js"),
        include_bytes!("../site/static/site.js"),
    )?;
    validate_site_output_in(repository, output, &inputs.progress)?;

    let mut generated_files = vec![
        PathBuf::from("index.html"),
        PathBuf::from("site.css"),
        PathBuf::from("site.js"),
    ];
    generated_files.extend(reference_paths.values().cloned());
    generated_files.extend(playable_files.iter().cloned());
    if inputs.playable.is_some() {
        generated_files.push(PathBuf::from("last-green.json"));
    }
    if let Some(evidence) = &evidence {
        generated_files.extend(evidence.files.iter().cloned());
    } else if inputs.verification.is_some() {
        generated_files.push(PathBuf::from(VERIFICATION_FILE));
    }
    generated_files.sort();

    Ok(SiteManifest {
        source_commit,
        playable_commit: inputs
            .playable
            .as_ref()
            .map(|playable| playable.source_commit.clone()),
        current_task: current_task.map(|task| task.id.clone()),
        generated_files,
        semantic_visual_hash: inputs
            .verification
            .as_ref()
            .map(|evidence| evidence.summary.semantic_visual_hash.clone()),
    })
}

/// What one build actually published from the verified evidence.
struct PublishedEvidence {
    /// Every published file, relative to the site output.
    files: Vec<PathBuf>,
    /// The semantic hash of the frames this build promoted.
    semantic_visual_hash: String,
    /// The history after this publication.
    gallery: GalleryManifest,
    /// The current frames, by stable label.
    current: BTreeMap<String, String>,
    /// The browser canvas proof, when the gate kept one.
    browser: Option<String>,
    /// Whether this build opened a new point in the history.
    appended: bool,
}

/// Promotes the current verified frames and updates the visual history.
///
/// Only a run that succeeded and met every published bound publishes pixels.
/// A failed run writes the sanitized projection alone, so `screenshots/` and
/// `gallery.json` stay absent from the generated tree and assembly retains
/// whatever the last green publication left there.
fn publish_verification(
    inputs: &SiteInputs,
    source_commit: &str,
    committed_at: &str,
    current_task: Option<&ProgressTask>,
    output: &Path,
) -> Result<Option<PublishedEvidence>, SitegenError> {
    let Some(evidence) = inputs.verification.as_ref() else {
        return Ok(None);
    };
    let summary = &evidence.summary;
    let json = serde_json::to_string_pretty(summary).map_err(|error| SitegenError::Json {
        path: PathBuf::from(VERIFICATION_FILE),
        message: error.to_string(),
    })?;
    write_file(&output.join(VERIFICATION_FILE), json.as_bytes())?;
    if !summary.succeeded {
        return Ok(None);
    }

    let mut files = vec![PathBuf::from(VERIFICATION_FILE)];
    let mut current = BTreeMap::new();
    for frame in &summary.frames {
        let source = resolve_artifact(&evidence.artifacts, &frame.artifact)?;
        let relative = Path::new(CURRENT_SCREENSHOTS).join(&frame.name);
        copy_artifact(&source, &output.join(&relative))?;
        files.push(relative);
    }
    for (label, file) in GALLERY_FRAMES {
        current.insert(label.to_owned(), format!("{CURRENT_SCREENSHOTS}/{file}"));
    }

    let centre = summary
        .frames
        .first()
        .ok_or_else(|| SitegenError::MissingInput {
            path: PathBuf::from(CURRENT_SCREENSHOTS),
        })?;
    let crop_relative = Path::new(CURRENT_SCREENSHOTS).join(WORKER_CROP_FILE);
    write_worker_crop(
        &output.join(CURRENT_SCREENSHOTS).join(&centre.name),
        centre.worker_crop,
        &output.join(&crop_relative),
    )?;
    files.push(crop_relative);
    current.insert(
        WORKER_FRAME_LABEL.to_owned(),
        format!("{CURRENT_SCREENSHOTS}/{WORKER_CROP_FILE}"),
    );

    let browser = match (&evidence.browser_artifacts, summary.browser.as_ref()) {
        (Some(root), Some(browser)) => match &browser.screenshot {
            Some(name) => {
                let source = resolve_artifact(root, name)?;
                let relative = Path::new(CURRENT_SCREENSHOTS).join(BROWSER_FRAME_FILE);
                copy_artifact(&source, &output.join(&relative))?;
                files.push(relative);
                Some(format!("{CURRENT_SCREENSHOTS}/{BROWSER_FRAME_FILE}"))
            }
            None => None,
        },
        _ => None,
    };

    let commit = CommitSummary {
        sha: source_commit.to_owned(),
        subject: String::new(),
        committed_at: committed_at.to_owned(),
        task_id: current_task.map(|task| task.id.clone()),
    };
    let previous = inputs.gallery.clone().unwrap_or_default();
    let gallery = update_gallery(&previous, summary, &commit);
    let appended = gallery.entries.len() > previous.entries.len();
    if appended {
        let entry = gallery
            .entries
            .last()
            .expect("an appended history always has a latest entry");
        for (label, published) in &entry.frames {
            let source = current
                .get(label)
                .ok_or_else(|| SitegenError::MissingInput {
                    path: PathBuf::from(published),
                })?;
            let relative = PathBuf::from(published);
            copy_artifact(&output.join(source), &output.join(&relative))?;
            files.push(relative);
        }
    }

    let manifest = serde_json::to_string_pretty(&gallery).map_err(|error| SitegenError::Json {
        path: PathBuf::from(GALLERY_FILE),
        message: error.to_string(),
    })?;
    write_file(&output.join(GALLERY_FILE), manifest.as_bytes())?;
    files.push(PathBuf::from(GALLERY_FILE));

    Ok(Some(PublishedEvidence {
        files,
        semantic_visual_hash: summary.semantic_visual_hash.clone(),
        gallery,
        current,
        browser,
        appended,
    }))
}

/// Writes the reported technician rectangle of one published frame.
fn write_worker_crop(
    frame: &Path,
    crop: PublicRect,
    destination: &Path,
) -> Result<(), SitegenError> {
    let image = ImageReader::open(frame)
        .map_err(|error| error.to_string())
        .and_then(|reader| {
            reader
                .with_guessed_format()
                .map_err(|error| error.to_string())
        })
        .and_then(|reader| reader.decode().map_err(|error| error.to_string()))
        .map_err(|message| SitegenError::CorruptArtifact {
            path: frame.to_path_buf(),
            message,
        })?
        .to_rgb8();
    if crop.width == 0
        || crop.height == 0
        || crop.x + crop.width > image.width()
        || crop.y + crop.height > image.height()
    {
        return Err(SitegenError::CorruptArtifact {
            path: frame.to_path_buf(),
            message: format!(
                "the reported worker crop {}x{} at {},{} is not inside the frame",
                crop.width, crop.height, crop.x, crop.y
            ),
        });
    }
    let cropped =
        image::imageops::crop_imm(&image, crop.x, crop.y, crop.width, crop.height).to_image();
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| SitegenError::Io {
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;
    }
    cropped.save(destination).map_err(|error| SitegenError::Io {
        path: destination.to_path_buf(),
        message: error.to_string(),
    })
}

pub fn assemble_site(
    previous: Option<&Path>,
    current: &Path,
    workflow: &WorkflowSummary,
    output: &Path,
) -> Result<BuildDisposition, SitegenError> {
    require_directory(current)?;

    let disposition = match (workflow.native, workflow.web, previous) {
        (GateStatus::Passed, GateStatus::Passed, _) => BuildDisposition::GreenReplacement,
        (GateStatus::Failed, _, Some(_)) | (_, GateStatus::Failed, Some(_)) => {
            BuildDisposition::FailedRetainLastGreen
        }
        (GateStatus::Failed, _, None) | (_, GateStatus::Failed, None) => {
            BuildDisposition::FirstRunStatusOnly
        }
        (_, _, Some(_)) => BuildDisposition::RetainLastGreen,
        (_, _, None) => BuildDisposition::FirstRunStatusOnly,
    };

    // A green replacement publishes the current game alone, so the previous
    // one is only ever discarded when a complete replacement really exists.
    // Everything below runs before the output is touched.
    if disposition == BuildDisposition::GreenReplacement {
        require_complete_replacement(current, previous)?;
    }

    let retains_game = matches!(
        disposition,
        BuildDisposition::RetainLastGreen | BuildDisposition::FailedRetainLastGreen
    );
    let evidence = evidence_publication(current)?;

    prepare_assembly_output(output)?;

    // The two domains are retained independently. A build may promote verified
    // evidence without producing a game, and a new game never invalidates the
    // last verified pixels. The previous publication is laid down first so
    // that whatever this build really published overlays it.
    if let Some(previous) = previous {
        require_directory(previous)?;
        let mut retained = Vec::new();
        if retains_game {
            retained.extend(PLAYABLE_ARTIFACTS);
        }
        retained.extend(match evidence {
            // Promoted pixels replace the current frames and the manifest, but
            // the visual history is cumulative and always carries forward.
            EvidencePublication::Promoted => &[HISTORY_SCREENSHOTS][..],
            // A projection alone publishes this run's status over the last
            // green pixels and the manifest that names them.
            EvidencePublication::ProjectionOnly => &EVIDENCE_PIXELS[..],
            // Nothing verified at all: the whole last green evidence set
            // stays, so no image is left without the manifest that names it.
            EvidencePublication::Absent => &EVIDENCE_ARTIFACTS[..],
        });
        copy_previous_artifacts(previous, output, &retained)?;
    }

    // A build never publishes a game it did not earn. A first run has no
    // previous game to retain either, so any playable-looking artifact an
    // inconsistent `current` tree happens to carry is refused here rather
    // than trusted: the public `assemble_site` API is called directly by
    // more than the generator's own pipeline, and `FirstRunStatusOnly` is a
    // guarantee that no game is published, not merely a default.
    let protected: &[&str] = if retains_game || disposition == BuildDisposition::FirstRunStatusOnly
    {
        &PLAYABLE_ARTIFACTS
    } else {
        &[]
    };
    copy_site_tree(current, current, output, protected)?;

    // Whether the retained manifest is trustworthy has to be judged from
    // exactly what the previous publication declared, before
    // `reconcile_last_green` resyncs its file lists with reality below: that
    // resync silently repairs a manifest whose declared files disagree with
    // its package, which would otherwise erase the very inconsistency this
    // is meant to catch.
    let retained_playable_trusted = retained_playable_is_consistent(output);
    reconcile_last_green(output, retained_playable_trusted)?;
    // `build_site` rendered the current run's own honest state before this
    // assembly ever ran, blind to whatever game the disposition above just
    // decided to keep alive. When this run produced no candidate of its own,
    // the page it wrote still shows the pending state; bring it into
    // agreement with the retained package this build really carries forward,
    // never with the current run's own commit.
    reconcile_playable_display(output, current, disposition, retained_playable_trusted)?;
    require_retained_history(output)?;
    validate_assembled_links(output)?;
    Ok(disposition)
}

/// Refuses an assembled tree whose history manifest outruns the images the
/// previous publication really supplied.
///
/// Only `pages-live` carries the visual history, so a build inherits a gallery
/// from its own predecessor and never from anywhere else. A first run handed a
/// gallery that names earlier points, and a later run whose predecessor lost
/// them, both publish a manifest whose images do not exist. The failure is
/// named here rather than left to the link checker, because the cause is the
/// inherited history and not the page that renders it.
fn require_retained_history(output: &Path) -> Result<(), SitegenError> {
    let (targets, misscoped) = history_frames(output);
    if !misscoped.is_empty() {
        return Err(SitegenError::HistoryFrameOutsideEntry { frames: misscoped });
    }
    let missing = targets
        .into_iter()
        .filter(|target| !output.join(target).is_file())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(SitegenError::MissingRetainedHistory { targets: missing })
}

/// What the current tree publishes about verification, independent of whether
/// it also published a game.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EvidencePublication {
    /// Promoted frames and the manifest that names them.
    Promoted,
    /// A sanitized projection alone: a failed run, or a green run that
    /// promoted no pixels.
    ProjectionOnly,
    /// No verification evidence at all.
    Absent,
}

/// Classifies what the current tree publishes about verification, refusing
/// to assume promoted frames and the gallery manifest are an atomic pair.
///
/// A build that really promoted frames always writes both together, so a
/// `screenshots/current` directory with no `gallery.json` beside it is not a
/// promoted run at all: it is a partial or inconsistent tree that assembly
/// must name rather than silently treat as a complete promotion.
fn evidence_publication(current: &Path) -> Result<EvidencePublication, SitegenError> {
    let has_current_frames = current.join(CURRENT_SCREENSHOTS).is_dir();
    let has_gallery = current.join(GALLERY_FILE).is_file();
    if has_current_frames && !has_gallery {
        return Err(SitegenError::PartialEvidencePublication {
            path: current.join(CURRENT_SCREENSHOTS),
        });
    }
    Ok(if has_current_frames {
        EvidencePublication::Promoted
    } else if current.join(VERIFICATION_FILE).is_file() {
        EvidencePublication::ProjectionOnly
    } else {
        EvidencePublication::Absent
    })
}

/// Carries named artifacts of a previous publication forward.
fn copy_previous_artifacts(
    previous: &Path,
    output: &Path,
    retained: &[&str],
) -> Result<(), SitegenError> {
    for relative in retained {
        let source = previous.join(relative);
        match fs::symlink_metadata(&source) {
            Ok(_) => copy_artifact(&source, &output.join(relative))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(SitegenError::Io {
                    path: source,
                    message: error.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Brings `last-green.json` back into agreement with the tree assembly
/// actually produced.
///
/// Assembly may retain the previous game while publishing new evidence, so the
/// manifest that survived describes a game that is still correct beside pixels
/// that are not. It is rewritten from the assembled tree and the evidence
/// published in it, never from a second record. A manifest this generator did
/// not write does not parse, and assembly leaves it exactly as it found it
/// rather than inventing one.
///
/// The declared `game_files` list is only resynced when `retained_playable_trusted`
/// is true. That flag is judged, by [`retained_playable_is_consistent`], from
/// exactly what the previous publication declared; if it already disagreed
/// with its own package, resyncing here would quietly repair that very
/// disagreement into a manifest a *later* publication's own consistency
/// check can no longer catch. Provenance found inconsistent must stay
/// unavailable, not be rehabilitated by the next assembly's resync. The
/// screenshot list and visual hash describe evidence, an independent
/// concern from the playable game, so they are always resynced regardless.
fn reconcile_last_green(
    output: &Path,
    retained_playable_trusted: bool,
) -> Result<(), SitegenError> {
    let path = output.join(LAST_GREEN_FILE);
    let Ok(json) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(mut manifest) = serde_json::from_str::<LastGreenManifest>(&json) else {
        return Ok(());
    };
    if retained_playable_trusted {
        manifest.game_files = published_files(output, "play")?;
    }
    manifest.screenshot_files = published_files(output, CURRENT_SCREENSHOTS)?;
    if let Some(hash) = promoted_visual_hash(output) {
        manifest.semantic_visual_hash = Some(hash);
    }
    let json = serde_json::to_string_pretty(&manifest).map_err(|error| SitegenError::Json {
        path: PathBuf::from(LAST_GREEN_FILE),
        message: error.to_string(),
    })?;
    write_file(&path, json.as_bytes())
}

/// The hash of the evidence the assembled tree publishes, when that evidence
/// succeeded.
///
/// A failed projection describes no pixels, so it supplies no hash and the
/// retained one keeps describing the retained frames. The published document
/// is read as a whole so that assembly never has to track the projection's
/// schema.
fn promoted_visual_hash(output: &Path) -> Option<String> {
    let json = fs::read_to_string(output.join(VERIFICATION_FILE)).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&json).ok()?;
    if value.get("succeeded")?.as_bool() != Some(true) {
        return None;
    }
    Some(value.get("semantic_visual_hash")?.as_str()?.to_owned())
}

/// Every published file below one site-relative directory, in stable order.
fn published_files(output: &Path, relative: &str) -> Result<Vec<PathBuf>, SitegenError> {
    let root = output.join(relative);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = collect_files(&root, output)?;
    files.sort();
    Ok(files)
}

/// Refuses a green replacement that would publish a broken game or silently
/// drop the last verified one.
///
/// A status-only site with no game at all is still a valid replacement, but
/// only while there is no previous game left to lose.
fn require_complete_replacement(
    current: &Path,
    previous: Option<&Path>,
) -> Result<(), SitegenError> {
    let package = current.join("play");
    if !package.is_dir() {
        let previous_has_game = previous.is_some_and(|previous| previous.join("play").is_dir());
        if previous_has_game {
            return Err(SitegenError::IncompletePlayablePackage {
                path: package,
                missing: vec!["play/".to_owned()],
            });
        }
        return Ok(());
    }

    let missing = missing_playable_parts(&package);
    if !missing.is_empty() {
        return Err(SitegenError::IncompletePlayablePackage {
            path: package,
            missing,
        });
    }
    Ok(())
}

/// Every required part a packaged browser game is missing, in declared order.
pub fn missing_playable_parts(package: &Path) -> Vec<String> {
    let mut missing = REQUIRED_PLAYABLE_FILES
        .into_iter()
        .filter(|required| !package.join(required).is_file())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let assets = package.join(REQUIRED_PLAYABLE_ASSETS);
    let has_assets =
        assets.is_dir() && collect_files(&assets, package).is_ok_and(|files| !files.is_empty());
    if !has_assets {
        missing.push(REQUIRED_PLAYABLE_ASSETS.to_owned());
    }
    missing
}

/// Whether a retained playable manifest, exactly as it arrived from the
/// previous publication, is safe to trust as provenance for the homepage.
///
/// A manifest is not "shape valid JSON" alone: `LastGreenManifest` has no
/// validation beyond serde's, so an unsafe commit label or a declared file
/// list that disagrees with the package it names must be caught here, before
/// [`reconcile_last_green`] resyncs those same file lists with reality and
/// silently erases the very inconsistency this exists to refuse. Invalid or
/// inconsistent retained metadata is treated exactly like no retained game
/// at all, never normalized into trusted provenance.
fn retained_playable_is_consistent(output: &Path) -> bool {
    let package = output.join("play");
    if !package.is_dir() || !missing_playable_parts(&package).is_empty() {
        return false;
    }
    let Ok(manifest_json) = fs::read_to_string(output.join(LAST_GREEN_FILE)) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<LastGreenManifest>(&manifest_json) else {
        return false;
    };
    if validate_commit("source_commit", &manifest.source_commit).is_err() {
        return false;
    }
    let Ok(actual_files) = published_files(output, "play") else {
        return false;
    };
    manifest.game_files == actual_files
}

pub fn validate_site_output(
    output: &Path,
    progress: &ProgressDocument,
) -> Result<(), SitegenError> {
    validate_site_output_in(&default_repository(), output, progress)
}

/// Validates one generated page against the repository it was published from.
pub fn validate_site_output_in(
    repository: &Path,
    output: &Path,
    progress: &ProgressDocument,
) -> Result<(), SitegenError> {
    let index_path = output.join("index.html");
    let html = fs::read_to_string(&index_path).map_err(|error| SitegenError::Io {
        path: index_path.clone(),
        message: error.to_string(),
    })?;
    let document = Html::parse_document(&html);
    // History images belong to earlier accepted points that assembly carries
    // forward. The gallery manifest published beside this page is what vouches
    // for them, so a history link that the manifest does not declare is still
    // a broken link.
    let retained = retained_history_targets(output);

    if document.select(&selector("main")).count() != 1 {
        return Err(invalid_html(&index_path, "expected exactly one <main>"));
    }

    let mut ids = BTreeSet::new();
    for element in document.select(&selector("[id]")) {
        let id = element
            .value()
            .attr("id")
            .expect("the selector guarantees an id");
        if !ids.insert(id.to_owned()) {
            return Err(invalid_html(&index_path, format!("duplicate id {id:?}")));
        }
    }

    for image in document.select(&selector("img")) {
        if image
            .value()
            .attr("alt")
            .is_none_or(|alt| alt.trim().is_empty())
        {
            return Err(SitegenError::MissingAltText {
                source: PathBuf::from("index.html"),
            });
        }
    }

    for element in document.select(&selector(
        "a[href], img[src], link[href], script[src], iframe[src]",
    )) {
        let attribute = if element.value().attr("href").is_some() {
            "href"
        } else {
            "src"
        };
        let target = element
            .value()
            .attr(attribute)
            .expect("the selector guarantees a target");
        validate_local_target(output, &ids, &retained, &index_path, target)?;
    }

    if let Some(found) = published_absolute_path(repository, &document, &html) {
        return Err(invalid_html(
            &index_path,
            format!("absolute local path is present: {found}"),
        ));
    }

    for script in document.select(&selector("script")) {
        if script.value().attr("src") != Some("site.js")
            || script.text().any(|text| !text.trim().is_empty())
        {
            return Err(invalid_html(
                &index_path,
                "only the declared external site script is allowed",
            ));
        }
    }

    let linked_tasks = document
        .select(&selector("[data-progress-task]"))
        .filter_map(|link| {
            let task = link.value().attr("data-progress-task")?;
            let href = link.value().attr("href")?;
            (href == format!("#plan-{task}")).then(|| task.to_owned())
        })
        .collect::<BTreeSet<_>>();
    for task in &progress.tasks {
        if !linked_tasks.contains(&task.id) || !ids.contains(&format!("plan-{}", task.id)) {
            return Err(invalid_html(
                &index_path,
                format!("task {} does not link to a rendered plan heading", task.id),
            ));
        }
    }

    Ok(())
}

/// Proves every link the assembled page makes resolves against the assembled
/// tree.
///
/// Generation validates a page against the files one build wrote, and accepts
/// history links its manifest declares because those images only arrive at
/// assembly. Assembly is where they arrive, and where it is decided which of
/// the previous files survive, so the same links are checked once more with no
/// allowance left: after assembly the file has to be there.
pub fn validate_assembled_links(output: &Path) -> Result<(), SitegenError> {
    let index_path = output.join("index.html");
    let html = match fs::read_to_string(&index_path) {
        Ok(html) => html,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(SitegenError::Io {
                path: index_path,
                message: error.to_string(),
            });
        }
    };
    let document = Html::parse_document(&html);
    let ids = document
        .select(&selector("[id]"))
        .filter_map(|element| element.value().attr("id").map(str::to_owned))
        .collect::<BTreeSet<_>>();

    for element in document.select(&selector(
        "a[href], img[src], link[href], script[src], iframe[src]",
    )) {
        let attribute = if element.value().attr("href").is_some() {
            "href"
        } else {
            "src"
        };
        let target = element
            .value()
            .attr(attribute)
            .expect("the selector guarantees a target");
        validate_local_target(output, &ids, &BTreeSet::new(), &index_path, target)?;
    }
    Ok(())
}

fn prepare_output(repository: &Path, output: &Path) -> Result<(), SitegenError> {
    validate_output_path_in(repository, output)?;
    if let Ok(metadata) = fs::symlink_metadata(output) {
        if metadata.is_dir() {
            fs::remove_dir_all(output).map_err(|error| SitegenError::Io {
                path: output.to_path_buf(),
                message: error.to_string(),
            })?;
        } else {
            fs::remove_file(output).map_err(|error| SitegenError::Io {
                path: output.to_path_buf(),
                message: error.to_string(),
            })?;
        }
    }
    fs::create_dir_all(output).map_err(|error| SitegenError::Io {
        path: output.to_path_buf(),
        message: error.to_string(),
    })
}

fn prepare_assembly_output(output: &Path) -> Result<(), SitegenError> {
    validate_output_path(output)?;
    match fs::symlink_metadata(output) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(SitegenError::UnsafeOutputPath {
                path: output.to_path_buf(),
            })
        }
        Ok(_) => {
            let mut entries = fs::read_dir(output).map_err(|error| SitegenError::Io {
                path: output.to_path_buf(),
                message: error.to_string(),
            })?;
            if entries.next().is_some() {
                return Err(SitegenError::OutputNotEmpty {
                    path: output.to_path_buf(),
                });
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(output)
            .map_err(|error| SitegenError::Io {
                path: output.to_path_buf(),
                message: error.to_string(),
            }),
        Err(error) => Err(SitegenError::Io {
            path: output.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

/// The build roots a packaged browser game may legitimately come from.
///
/// Local runs and `sitegen` invocations build into the repository `target/`
/// directory; the Pages workflow builds into the runner temporary directory.
/// Nothing else is trusted, so a package path can never reach the source tree
/// or an arbitrary location on the host.
pub fn trusted_playable_roots() -> Vec<PathBuf> {
    trusted_playable_roots_in(&default_repository())
}

/// The same roots, for a caller that knows which repository it is publishing
/// from.
pub fn trusted_playable_roots_in(repository: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(target) = fs::canonicalize(repository.join("target")) {
        roots.push(target);
    }
    if let Some(runner_temp) = std::env::var_os("RUNNER_TEMP")
        && let Ok(runner_temp) = fs::canonicalize(runner_temp)
        && !roots.contains(&runner_temp)
    {
        roots.push(runner_temp);
    }
    roots
}

/// Canonicalizes a packaged browser game and proves it is strictly inside one
/// of `trusted_roots`.
///
/// Absolute paths, relative parent escapes, source-tree paths, and symbolic
/// links that leave a trusted root all resolve to a canonical path outside
/// every root and are refused before a single byte is copied.
pub fn resolve_playable_package(
    directory: &Path,
    trusted_roots: &[PathBuf],
) -> Result<PathBuf, SitegenError> {
    let untrusted = || SitegenError::UntrustedPlayablePackage {
        path: directory.to_path_buf(),
    };
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_symlink() => return Err(untrusted()),
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(SitegenError::MissingInput {
                path: directory.to_path_buf(),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SitegenError::MissingInput {
                path: directory.to_path_buf(),
            });
        }
        Err(error) => {
            return Err(SitegenError::Io {
                path: directory.to_path_buf(),
                message: error.to_string(),
            });
        }
    }
    let canonical = fs::canonicalize(directory).map_err(|error| SitegenError::Io {
        path: directory.to_path_buf(),
        message: error.to_string(),
    })?;
    let contained = trusted_roots
        .iter()
        .any(|root| canonical != *root && canonical.starts_with(root));
    if !contained {
        return Err(untrusted());
    }
    Ok(canonical)
}

fn require_directory(path: &Path) -> Result<(), SitegenError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(SitegenError::MissingInput {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(SitegenError::MissingInput {
                path: path.to_path_buf(),
            })
        }
        Err(error) => Err(SitegenError::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

/// The packaged game and the manifest that describes it.
const PLAYABLE_ARTIFACTS: [&str; 2] = ["play", LAST_GREEN_FILE];

/// The published pixels and the manifest that names them.
const EVIDENCE_PIXELS: [&str; 2] = [SCREENSHOTS_ROOT, GALLERY_FILE];

/// Everything the last green publication said about verification.
const EVIDENCE_ARTIFACTS: [&str; 3] = [SCREENSHOTS_ROOT, GALLERY_FILE, VERIFICATION_FILE];

fn copy_site_tree(
    root: &Path,
    source: &Path,
    output: &Path,
    protected: &[&str],
) -> Result<(), SitegenError> {
    let mut entries = fs::read_dir(source)
        .map_err(|error| SitegenError::Io {
            path: source.to_path_buf(),
            message: error.to_string(),
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| SitegenError::Io {
            path: source.to_path_buf(),
            message: error.to_string(),
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("directory entries remain below the copied root");
        if is_protected_artifact(relative, protected) {
            continue;
        }
        copy_artifact(&path, &output.join(relative))?;
    }
    Ok(())
}

fn copy_artifact(source: &Path, destination: &Path) -> Result<(), SitegenError> {
    let metadata = fs::symlink_metadata(source).map_err(|error| SitegenError::Io {
        path: source.to_path_buf(),
        message: error.to_string(),
    })?;
    if metadata.file_type().is_symlink() {
        return Err(SitegenError::UnsafeOutputPath {
            path: source.to_path_buf(),
        });
    }
    if metadata.is_dir() {
        fs::create_dir_all(destination).map_err(|error| SitegenError::Io {
            path: destination.to_path_buf(),
            message: error.to_string(),
        })?;
        return copy_site_tree(source, source, destination, &[]);
    }
    if !metadata.is_file() {
        return Err(SitegenError::MissingPreviousArtifact {
            path: source.to_path_buf(),
        });
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| SitegenError::Io {
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;
    }
    fs::copy(source, destination)
        .map(|_| ())
        .map_err(|error| SitegenError::Io {
            path: destination.to_path_buf(),
            message: error.to_string(),
        })
}

fn is_protected_artifact(relative: &Path, protected: &[&str]) -> bool {
    relative
        .components()
        .next()
        .is_some_and(|component| match component {
            std::path::Component::Normal(name) => protected
                .iter()
                .any(|retained| name == std::ffi::OsStr::new(retained)),
            _ => false,
        })
}

pub fn validate_output_path(output: &Path) -> Result<(), SitegenError> {
    validate_output_path_in(&default_repository(), output)
}

/// Refuses an output directory that would publish into a repository's own
/// source tree.
///
/// Two roots are protected: the repository the caller declared, which is the
/// checkout this run is publishing from, and the one this binary was compiled
/// in, which is where a developer's own source tree lives. Declaring a
/// repository therefore only ever adds protection. A root that is not on this
/// machine at all — what a relocated binary's compiled-in path is — protects
/// nothing and is skipped rather than failing the run that no longer has it.
pub fn validate_output_path_in(repository: &Path, output: &Path) -> Result<(), SitegenError> {
    if output.as_os_str().is_empty() || output == Path::new(".") {
        return Err(SitegenError::UnsafeOutputPath {
            path: output.to_path_buf(),
        });
    }
    if fs::symlink_metadata(output).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(SitegenError::UnsafeOutputPath {
            path: output.to_path_buf(),
        });
    }

    let resolved = resolve_destination(output)?;
    let mut protected = Vec::new();
    for root in [repository.to_path_buf(), default_repository()] {
        let Ok(canonical) = fs::canonicalize(&root) else {
            continue;
        };
        if !protected.contains(&canonical) {
            protected.push(canonical);
        }
    }
    for root in protected {
        if resolved.starts_with(&root) && !resolved.starts_with(root.join("target")) {
            return Err(SitegenError::UnsafeOutputPath {
                path: output.to_path_buf(),
            });
        }
    }
    Ok(())
}

fn resolve_destination(path: &Path) -> Result<PathBuf, SitegenError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| SitegenError::Io {
                path: PathBuf::from("."),
                message: error.to_string(),
            })?
            .join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let name = ancestor
            .file_name()
            .ok_or_else(|| SitegenError::UnsafeOutputPath {
                path: path.to_path_buf(),
            })?;
        suffix.push(name.to_owned());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| SitegenError::UnsafeOutputPath {
                path: path.to_path_buf(),
            })?;
    }
    let mut resolved = fs::canonicalize(ancestor).map_err(|error| SitegenError::Io {
        path: ancestor.to_path_buf(),
        message: error.to_string(),
    })?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn copy_reference_assets(
    repository: &Path,
    manifest: &ReferenceManifest,
    output: &Path,
) -> Result<BTreeMap<String, PathBuf>, SitegenError> {
    let mut paths = BTreeMap::new();
    for asset in &manifest.assets {
        let source = repository.join(&asset.public_path);
        let file_name = Path::new(&asset.public_path).file_name().ok_or_else(|| {
            SitegenError::MissingInput {
                path: PathBuf::from(&asset.public_path),
            }
        })?;
        let relative = Path::new("reference").join(file_name);
        let target = output.join(&relative);
        let bytes = fs::read(&source).map_err(|error| SitegenError::Io {
            path: source,
            message: error.to_string(),
        })?;
        write_file(&target, &bytes)?;
        paths.insert(asset.public_path.clone(), relative);
    }
    Ok(paths)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), SitegenError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| SitegenError::Io {
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;
    }
    fs::write(path, bytes).map_err(|error| SitegenError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn render_status(
    inputs: &SiteInputs,
    current_task: Option<&ProgressTask>,
    source_commit: &str,
    updated_at: &str,
) -> String {
    let source_label =
        if [inputs.workflow.native, inputs.workflow.web].contains(&GateStatus::Failed) {
            format!("CURRENT SOURCE: FAILED AT {}", short_sha(source_commit))
        } else {
            short_sha(source_commit)
        };
    format!(
        r#"<div class="status-item"><span>Source</span><strong>{}</strong></div>
        <div class="status-item"><span>Native</span><strong class="status-{}">{}</strong></div>
        <div class="status-item"><span>Web</span><strong class="status-{}">{}</strong></div>
        <div class="status-item status-current"><span>Working now</span><strong>{}</strong></div>
        <div class="status-item"><span>Updated</span><strong>{}</strong></div>"#,
        escape_html(&source_label),
        gate_class(inputs.workflow.native),
        gate_label(inputs.workflow.native),
        gate_class(inputs.workflow.web),
        gate_label(inputs.workflow.web),
        escape_html(current_task.map_or("All planned work complete", |task| task.title.as_str())),
        escape_html(updated_at),
    )
}

/// Copies a verified browser package into `play/` and returns the published
/// relative paths in stable order.
fn copy_playable_build(
    repository: &Path,
    playable: Option<&PlayableBuild>,
    output: &Path,
) -> Result<Vec<PathBuf>, SitegenError> {
    let Some(playable) = playable else {
        return Ok(Vec::new());
    };
    let package =
        resolve_playable_package(&playable.directory, &trusted_playable_roots_in(repository))?;
    if let Some(missing) = missing_playable_parts(&package).first() {
        return Err(SitegenError::MissingInput {
            path: package.join(missing),
        });
    }

    let destination = output.join("play");
    fs::create_dir_all(&destination).map_err(|error| SitegenError::Io {
        path: destination.clone(),
        message: error.to_string(),
    })?;
    copy_site_tree(&package, &package, &destination, &[])?;

    let mut game_files = collect_files(&destination, output)?;
    game_files.sort();
    Ok(game_files)
}

/// Records what this build published, enumerated from the output it wrote.
///
/// The hash belongs to the frames that were actually promoted, so a run that
/// published no pixels records none, and the screenshot list is read back from
/// the published directory rather than predicted from the promotion plan.
fn write_last_green(
    playable: Option<&PlayableBuild>,
    evidence: Option<&PublishedEvidence>,
    output: &Path,
) -> Result<(), SitegenError> {
    let Some(playable) = playable else {
        return Ok(());
    };
    let manifest = LastGreenManifest {
        source_commit: playable.source_commit.clone(),
        semantic_visual_hash: evidence.map(|evidence| evidence.semantic_visual_hash.clone()),
        game_files: published_files(output, "play")?,
        screenshot_files: published_files(output, CURRENT_SCREENSHOTS)?,
    };
    let json = serde_json::to_string_pretty(&manifest).map_err(|error| SitegenError::Json {
        path: PathBuf::from(LAST_GREEN_FILE),
        message: error.to_string(),
    })?;
    write_file(&output.join(LAST_GREEN_FILE), json.as_bytes())
}

/// Every regular file below `root`, relative to `base`, in directory order.
fn collect_files(root: &Path, base: &Path) -> Result<Vec<PathBuf>, SitegenError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| SitegenError::Io {
            path: directory.clone(),
            message: error.to_string(),
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| SitegenError::Io {
                path: directory.clone(),
                message: error.to_string(),
            })?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(
                    path.strip_prefix(base)
                        .expect("collected files remain below the site output")
                        .to_path_buf(),
                );
            }
        }
    }
    Ok(files)
}

fn render_play(playable: Option<&PlayableBuild>) -> String {
    let Some(playable) = playable else {
        return render_pending_play();
    };
    format!(
        r#"<div class="play-frame play-frame-live">
          <iframe class="play-embed" src="play/index.html" title="Playable Cell Shift data centre build" loading="lazy"></iframe>
        </div>
        <div class="control-strip" aria-label="Game controls">
          <span><kbd>Arrow keys</kbd> Move</span>
          <span><kbd>Q</kbd>/<kbd>E</kbd> Orbit</span>
          <span><kbd>Space</kbd> Repair</span>
        </div>
        <dl class="provenance">
          <div><dt>Playable build</dt><dd><code>{}</code></dd></div>
          <div><dt>Proof</dt><dd><a href="{}">Browser gate run</a></dd></div>
          <div><dt>Direct link</dt><dd><a href="play/index.html">Open the playable build</a></dd></div>
        </dl>"#,
        escape_html(&short_sha(&playable.source_commit)),
        escape_html(&playable.run_url),
    )
}

/// The publication mode badge: what the current page actually carries.
fn render_mode(playable: bool, verified: bool) -> String {
    let (mode, detail) = match (playable, verified) {
        (true, true) => ("Verified", "Playable build and current evidence"),
        (true, false) => ("Playable", "Browser build without current evidence"),
        (false, true) => ("Evidence", "Current evidence without a playable build"),
        (false, false) => ("Status", "Game pending verification"),
    };
    format!(
        r#"<div class="hero-badge" aria-label="Current publication mode">
          <span>Mode</span>
          <strong>{mode}</strong>
          <small>{detail}</small>
        </div>"#
    )
}

fn render_pending_play() -> String {
    r#"<div class="play-frame" role="img" aria-label="Playable Cell Shift game area awaiting its first verified build">
          <div class="empty-state play-empty">
            <span class="eyebrow">Status-only launch</span>
            <h2>No verified playable build yet</h2>
            <p>The browser game will appear here only after the WASM and browser gates prove a real rendered frame.</p>
          </div>
        </div>
        <div class="control-strip" aria-label="Planned game controls">
          <span><kbd>Arrow keys</kbd> Move</span>
          <span><kbd>Q</kbd>/<kbd>E</kbd> Orbit</span>
          <span><kbd>Space</kbd> Repair</span>
        </div>"#
                .to_owned()
}

/// The playable panel for a build that carries no candidate of its own
/// forward, but whose assembly retained a previous publication's package.
///
/// Unlike [`render_play`], this never cites a workflow run: the retained
/// package was never produced or proven by the current run, so it carries no
/// run proof of its own, only the commit its own retained metadata names.
fn render_retained_play(source_commit: &str) -> String {
    format!(
        r#"<div class="play-frame play-frame-live">
          <iframe class="play-embed" src="play/index.html" title="Playable Cell Shift data centre build" loading="lazy"></iframe>
        </div>
        <div class="control-strip" aria-label="Game controls">
          <span><kbd>Arrow keys</kbd> Move</span>
          <span><kbd>Q</kbd>/<kbd>E</kbd> Orbit</span>
          <span><kbd>Space</kbd> Repair</span>
        </div>
        <dl class="provenance">
          <div><dt>Playable build</dt><dd><code>{}</code></dd></div>
          <div><dt>Retained from</dt><dd>A previous verified publication; this run did not verify it.</dd></div>
          <div><dt>Direct link</dt><dd><a href="play/index.html">Open the playable build</a></dd></div>
        </dl>"#,
        escape_html(&short_sha(source_commit)),
    )
}

/// The publication mode badge for a build displaying a retained game.
fn render_retained_mode(verified: bool) -> String {
    let detail = if verified {
        "Playable build retained from a previous publication; current evidence verified separately"
    } else {
        "Playable build retained from a previous publication; current run did not verify"
    };
    format!(
        r#"<div class="hero-badge" aria-label="Current publication mode">
          <span>Mode</span>
          <strong>Retained</strong>
          <small>{detail}</small>
        </div>"#
    )
}

/// Wraps one rendered section in a stable HTML comment pair so a later
/// assembly can find and, when it must, replace it without ever touching the
/// rest of the page it did not write.
fn mark_reconcilable(name: &str, html: &str) -> String {
    format!("<!--{name}-->{html}<!--/{name}-->")
}

/// The exact byte range of one `mark_reconcilable` section, including its
/// delimiting comments.
///
/// A well-formed section has exactly one opening marker and exactly one
/// closing marker for `name` in the whole document. Locating only the first
/// open and the first close after it — without checking for extras — lets a
/// duplicate opening marker, a duplicate closing marker, or a same-name
/// nested section (e.g. `<!--play-->A<!--play-->B<!--/play-->C<!--/play-->`)
/// silently produce a span that covers only *part* of the malformed markup.
/// A caller that then replaces that span leaves the rest — a stray opening
/// marker, or trailing content after the "real" close — dangling verbatim in
/// the output instead of ever being refused. Requiring exactly one of each
/// marker rejects all such malformed structures up front, before any span is
/// ever returned.
fn marked_span(html: &str, name: &str) -> Option<(usize, usize)> {
    let open = format!("<!--{name}-->");
    let close = format!("<!--/{name}-->");
    if html.matches(&open).count() != 1 || html.matches(&close).count() != 1 {
        return None;
    }
    let start = html.find(&open)?;
    let content_start = start + open.len();
    let close_start = html[content_start..].find(&close)? + content_start;
    Some((start, close_start + close.len()))
}

fn replace_marked(html: &str, name: &str, replacement: &str) -> String {
    match marked_span(html, name) {
        Some((start, end)) => format!("{}{replacement}{}", &html[..start], &html[end..]),
        None => html.to_owned(),
    }
}

/// Whether the current run's own verification evidence — read from the
/// `current` tree the run itself produced, before assembly retains or
/// overlays anything from a previous publication — proved the current
/// commit.
///
/// This deliberately never reads the assembled `output` tree: when the
/// current run publishes no verification projection of its own, assembly
/// retains the previous run's `verification.json` into `output` so that
/// evidence is not lost, but that retained document describes a different
/// run's success, not this one's. Reading `current` instead means a run that
/// attempted no verification of its own can never be credited with a
/// previous run's separately-recorded success.
fn current_evidence_succeeded(current: &Path) -> bool {
    let Ok(json) = fs::read_to_string(current.join(VERIFICATION_FILE)) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|value| value.get("succeeded")?.as_bool())
        .unwrap_or(false)
}

/// Brings the assembled homepage into agreement with the playable build the
/// disposition really kept alive.
///
/// `build_site` renders the current run's own honest state before it knows
/// anything this assembly will decide, so a run that produced no candidate of
/// its own always leaves the pending panel behind, even when a retained game
/// is about to be carried forward. This is the one place that reconciles the
/// two: it never touches a page that already shows a game of its own, and it
/// only ever attributes a retained game to the commit its own retained
/// metadata names, never to the current run's commit. A retained package or
/// manifest that is missing, incomplete, unparsable, or internally
/// inconsistent (judged by `retained_playable_trusted`, computed before this
/// runs) is treated exactly like no retained game at all, so the page stays
/// pending rather than inventing provenance for something that cannot be
/// trusted. The play and mode markers are replaced as one atomic pair: if
/// either is missing, malformed, or its span crosses or nests with the
/// other's, neither is touched, so an incorrect badge can never survive
/// beside a reconciled iframe and a single replacement can never silently
/// consume its counterpart's markers.
fn reconcile_playable_display(
    output: &Path,
    current: &Path,
    disposition: BuildDisposition,
    retained_playable_trusted: bool,
) -> Result<(), SitegenError> {
    if !matches!(
        disposition,
        BuildDisposition::RetainLastGreen | BuildDisposition::FailedRetainLastGreen
    ) {
        return Ok(());
    }
    let index_path = output.join("index.html");
    let Ok(html) = fs::read_to_string(&index_path) else {
        return Ok(());
    };
    let Some((play_start, play_end)) = marked_span(&html, "play") else {
        return Ok(());
    };
    // A build that already shows a game of its own is left exactly as it is,
    // regardless of whether a retained package would otherwise be trusted.
    if html[play_start..play_end].contains("play-embed") {
        return Ok(());
    }
    // The play and mode sections are replaced together or not at all: a
    // missing or malformed mode marker must never leave a stale badge beside
    // a freshly reconciled iframe.
    let Some((mode_start, mode_end)) = marked_span(&html, "mode") else {
        return Ok(());
    };
    // Each marker is located independently by an isolated string search, so a
    // malformed page can produce spans that cross or nest instead of sitting
    // entirely before or after one another (e.g. a stray `<!--mode-->` inside
    // what `play`'s search reports as its own span). Replacing one span in
    // that case would silently delete or truncate the other marker before it
    // is ever looked for, turning an atomic pair replacement into a partial
    // one. Two ranges are genuinely disjoint only when one ends at or before
    // the other begins; anything else is refused untouched.
    let disjoint = play_end <= mode_start || mode_end <= play_start;
    if !disjoint {
        return Ok(());
    }
    if !retained_playable_trusted {
        return Ok(());
    }
    let Ok(manifest_json) = fs::read_to_string(output.join(LAST_GREEN_FILE)) else {
        return Ok(());
    };
    let Ok(manifest) = serde_json::from_str::<LastGreenManifest>(&manifest_json) else {
        return Ok(());
    };

    let verified = current_evidence_succeeded(current);
    let patched = replace_marked(
        &html,
        "play",
        &mark_reconcilable("play", &render_retained_play(&manifest.source_commit)),
    );
    let patched = replace_marked(
        &patched,
        "mode",
        &mark_reconcilable("mode", &render_retained_mode(verified)),
    );
    write_file(&index_path, patched.as_bytes())
}

fn render_comparison(
    manifest: &ReferenceManifest,
    reference_paths: &BTreeMap<String, PathBuf>,
    evidence: Option<&PublishedEvidence>,
) -> String {
    let mut assets = manifest.assets.iter();
    let Some(key_art) = assets.next() else {
        return empty_state("Comparison references are not available.");
    };
    let Some(character_sheet) = assets.next() else {
        return empty_state("The character reference is not available.");
    };
    let key_path = reference_paths
        .get(&key_art.public_path)
        .map_or_else(String::new, |path| path.display().to_string());
    let character_path = reference_paths
        .get(&character_sheet.public_path)
        .map_or_else(String::new, |path| path.display().to_string());
    let current = evidence.and_then(|published| published.current.get("center"));
    let worker = evidence.and_then(|published| published.current.get(WORKER_FRAME_LABEL));

    let (chip, current_layer, current_note) = match current {
        Some(path) => (
            "Current frame verified",
            format!(
                r#"<img src="{}" alt="Current verified Cell Shift game frame at the healthy north-east heading">"#,
                escape_html(path)
            ),
            "The approved key art and the current verified game frame are shown under one slider.",
        ),
        None => (
            "Current frame pending",
            r#"<div class="comparison-pending">No verified current frame</div>"#.to_owned(),
            "The approved key art is available. The current verified game frame does not exist yet.",
        ),
    };
    let (worker_chip, worker_layer, worker_note) = match worker {
        Some(path) => (
            "Worker crop verified",
            format!(
                r#"<img src="{}" alt="Current verified technician crop taken from the reported worker rectangle">"#,
                escape_html(path)
            ),
            "The approved character sheet is shown beside the crop the report projected.",
        ),
        None => (
            "Worker pending",
            r#"<div class="worker-pending">No verified worker crop</div>"#.to_owned(),
            "The approved character sheet is shown beside a clear placeholder for the future verified worker crop.",
        ),
    };
    let browser = evidence
        .and_then(|published| published.browser.as_deref())
        .map(|path| {
            format!(
                r#"<figure class="browser-proof"><img src="{}" alt="Canvas region the headless browser captured from the packaged game"><figcaption>Headless browser canvas</figcaption></figure>"#,
                escape_html(path)
            )
        })
        .unwrap_or_default();

    format!(
        r#"<div class="comparison-grid">
          <article class="comparison-card comparison-card-wide">
            <div class="panel-heading"><span class="eyebrow">Key art / current frame</span><span class="pending-chip">{chip}</span></div>
            <div class="comparison-stage" data-comparison style="--comparison: 50%">
              <div class="comparison-layer comparison-reference">
                <img src="{}" alt="Approved Cel Shift key art reference">
              </div>
              <div class="comparison-layer comparison-current" data-comparison-current>
                {current_layer}
              </div>
              <input data-compare-control type="range" min="0" max="100" value="50" aria-label="Reveal approved key art versus the current frame">
            </div>
            <p class="sr-only">{current_note}</p>
            {}
          </article>
          <article class="comparison-card">
            <div class="panel-heading"><span class="eyebrow">Character target</span><span class="pending-chip">{worker_chip}</span></div>
            <div class="character-comparison">
              <img src="{}" alt="Approved Cel Shift technician character sheet">
              {worker_layer}
            </div>
            <p class="sr-only">{worker_note}</p>
            {}
            {browser}
          </article>
        </div>"#,
        escape_html(&key_path),
        render_provenance(key_art),
        escape_html(&character_path),
        render_provenance(character_sheet),
    )
}

fn render_provenance(asset: &ReferenceAsset) -> String {
    format!(
        r#"<dl class="provenance">
          <div><dt>Source</dt><dd><code>{}</code></dd></div>
          <div><dt>Repository</dt><dd><code>{}</code></dd></div>
          <div><dt>SHA-256</dt><dd><code>{}</code></dd></div>
        </dl>"#,
        escape_html(&asset.source_path),
        escape_html(&asset.public_path),
        escape_html(&asset.sha256),
    )
}

fn render_progress(progress: &ProgressDocument, repo: &RepoFacts) -> String {
    [
        (ProgressStatus::Done, "Done", "done"),
        (ProgressStatus::InProgress, "Working now", "working"),
        (ProgressStatus::Future, "Future", "future"),
    ]
    .into_iter()
    .map(|(status, title, class)| {
        let cards = progress
            .tasks
            .iter()
            .filter(|task| task.status == status)
            .map(|task| render_task(task, repo))
            .collect::<String>();
        format!(
            r#"<section class="progress-column progress-{class}">
          <div class="column-heading"><span>{title}</span><strong>{}</strong></div>
          {}
        </section>"#,
            progress
                .tasks
                .iter()
                .filter(|task| task.status == status)
                .count(),
            if cards.is_empty() {
                empty_state("No tasks in this state.")
            } else {
                cards
            }
        )
    })
    .collect()
}

fn render_task(task: &ProgressTask, repo: &RepoFacts) -> String {
    let dependency_text = if task.depends_on.is_empty() {
        "Starts the plan".to_owned()
    } else {
        format!("After {}", task.depends_on.join(", "))
    };
    let commit = task
        .completed_commit
        .as_deref()
        .and_then(|commit| resolve_commit_ref(commit, repo))
        .map(|commit| {
            format!(
                r#"<a class="commit-link" href="{}">Commit {}</a>"#,
                commit_url(&commit),
                short_sha(&commit)
            )
        })
        .unwrap_or_default();
    format!(
        r##"<article class="task-card">
          <a data-progress-task="{}" href="#plan-{}"><h3>{}</h3></a>
          <p>{}</p>
          <div class="task-meta"><span>{}</span>{}</div>
        </article>"##,
        escape_html(&task.id),
        escape_html(&task.id),
        escape_html(&task.title),
        escape_html(&task.summary),
        escape_html(&dependency_text),
        commit,
    )
}

fn render_screenshots(inputs: &SiteInputs, evidence: Option<&PublishedEvidence>) -> String {
    // A promotion copies every captured frame into the published tree, so
    // every one of them is rendered here. A frame this build published and
    // linked from nowhere would be weight the site serves and nobody can see.
    let current = evidence
        .and(inputs.verification.as_ref())
        .map(|evidence| render_current_frames(&evidence.summary))
        .unwrap_or_default();
    let gallery = evidence
        .map(|published| &published.gallery)
        .or(inputs.gallery.as_ref());
    let Some(gallery) = gallery.filter(|gallery| !gallery.entries.is_empty()) else {
        let note = if current.is_empty() {
            "No verified screenshots yet. The timeline starts after the first deterministic render passes."
        } else {
            "The timeline starts at the first accepted history point."
        };
        return format!("{current}{}", empty_state(note));
    };

    // A build only ever links pixels it published itself. The images of older
    // accepted points are retained by assembly, and the manifest published
    // beside this page is what vouches for them.
    let published = evidence.is_some();
    let latest_hash = gallery
        .entries
        .last()
        .map_or("", |entry| entry.semantic_visual_hash.as_str());
    let history = gallery
        .entries
        .iter()
        .rev()
        .map(|entry| {
            let images = if published {
                entry
                    .frames
                    .iter()
                    .map(|(label, path)| {
                        format!(
                            r#"<figure><img src="{}" alt="{} frame verified at commit {}" loading="lazy"><figcaption>{}</figcaption></figure>"#,
                            escape_html(path),
                            escape_html(label),
                            escape_html(&short_sha(&entry.source_commit)),
                            escape_html(label),
                        )
                    })
                    .collect::<String>()
            } else {
                String::new()
            };
            let gallery_note = if published {
                String::new()
            } else {
                r#"<p class="history-note">Screenshots for this point are retained from the last green publication.</p>"#.to_owned()
            };
            let newest = evidence.is_some_and(|value| value.appended)
                && entry.semantic_visual_hash == latest_hash;
            let chip = if newest {
                r#"<span class="pending-chip">New this build</span>"#
            } else {
                ""
            };
            format!(
                r#"<article class="screenshot-entry">
          <div class="timeline-dot"></div>
          <div>
            <span class="eyebrow">{} &middot; <time>{}</time></span>{chip}
            <p>Working on {}</p>
            <p class="hash"><code>{}</code></p>
            <div class="screenshot-strip">{images}</div>
            {}
            {gallery_note}
          </div>
        </article>"#,
                escape_html(&short_sha(&entry.source_commit)),
                escape_html(&entry.committed_at),
                escape_html(&entry.current_task),
                escape_html(&entry.semantic_visual_hash),
                render_deltas(&entry.metric_deltas),
            )
        })
        .collect::<String>();
    format!("{current}{history}")
}

/// Every frame this build promoted into `screenshots/current`.
fn render_current_frames(summary: &VerificationSummary) -> String {
    let figures = summary
        .frames
        .iter()
        .map(|frame| {
            format!(
                r#"<figure><img src="{CURRENT_SCREENSHOTS}/{}" alt="Verified {} capture at the {} heading" loading="lazy"><figcaption>{}</figcaption></figure>"#,
                escape_html(&frame.name),
                escape_html(&frame.stage),
                escape_html(&frame.heading),
                escape_html(&frame.stage),
            )
        })
        .collect::<String>();
    if figures.is_empty() {
        return String::new();
    }
    format!(
        r#"<article class="screenshot-entry">
          <div class="timeline-dot"></div>
          <div>
            <span class="eyebrow">This build &middot; every promoted frame</span>
            <p>The complete set of captures behind the evidence published on this page.</p>
            <div class="screenshot-strip">{figures}</div>
          </div>
        </article>"#
    )
}

/// The metric changes one history entry recorded, largest movement first.
fn render_deltas(deltas: &BTreeMap<String, f64>) -> String {
    let mut moved = deltas
        .iter()
        .filter(|(_, delta)| **delta != 0.0)
        .collect::<Vec<_>>();
    // A point can move a dozen metrics at once, so the list leads with the
    // movement a reader is looking for. Equal movements keep the manifest's
    // own stable name order, so the same history always renders the same way.
    moved.sort_by(|(left_name, left), (right_name, right)| {
        right
            .abs()
            .total_cmp(&left.abs())
            .then_with(|| left_name.cmp(right_name))
    });
    let items = moved
        .into_iter()
        .map(|(name, delta)| {
            format!(
                r#"<li><code>{}</code> <strong>{}</strong></li>"#,
                escape_html(name),
                escape_html(&format_delta(*delta))
            )
        })
        .collect::<String>();
    if items.is_empty() {
        return r#"<p class="history-note">No published metric moved.</p>"#.to_owned();
    }
    format!(r#"<ul class="delta-list">{items}</ul>"#)
}

fn render_challenges(progress: &ProgressDocument, repo: &RepoFacts) -> String {
    let mut challenges = progress.challenges.iter().collect::<Vec<_>>();
    challenges.sort_by_key(|challenge| match challenge.status {
        ChallengeStatus::Open => 0,
        ChallengeStatus::Resolved => 1,
    });
    if challenges.is_empty() {
        return empty_state("No challenges are recorded in the canonical progress document.");
    }

    challenges
                .into_iter()
                .map(|challenge| {
                    let status = match challenge.status {
                        ChallengeStatus::Open => "Open",
                        ChallengeStatus::Resolved => "Resolved",
                    };
                    let resolution = challenge
                        .resolution
                        .as_deref()
                        .map(|resolution| {
                            format!(
                                "<div><dt>Resolution</dt><dd>{}</dd></div>",
                                escape_html(resolution)
                            )
                        })
                        .unwrap_or_default();
                    let commit = challenge
                        .resolved_commit
                        .as_deref()
                        .and_then(|commit| resolve_commit_ref(commit, repo))
                        .map(|commit| {
                            format!(
                                r#"<a href="{}">Resolved in {}</a>"#,
                                commit_url(&commit),
                                short_sha(&commit)
                            )
                        })
                        .unwrap_or_default();
                    format!(
                        r#"<article class="challenge-card challenge-{}">
          <div class="panel-heading"><span class="eyebrow">{status}</span>{commit}</div>
          <h3>{}</h3>
          <dl><div><dt>Impact</dt><dd>{}</dd></div><div><dt>Approach</dt><dd>{}</dd></div>{resolution}</dl>
        </article>"#,
                        match challenge.status {
                            ChallengeStatus::Open => "open",
                            ChallengeStatus::Resolved => "resolved",
                        },
                        escape_html(&challenge.title),
                        escape_html(&challenge.impact),
                        escape_html(&challenge.approach),
                    )
                })
                .collect()
}

fn render_tests(inputs: &SiteInputs) -> String {
    let summary = inputs.verification.as_ref().map(|value| &value.summary);
    let gates = inputs
        .workflow
        .gates
        .iter()
        .chain(summary.iter().flat_map(|report| report.gates.iter()))
        .collect::<Vec<_>>();
    if gates.is_empty() && summary.is_none() {
        return empty_state("No gate results have been published.");
    }

    let rows = gates
                .into_iter()
                .map(|gate| {
                    let artifact = gate
                        .artifact_url
                        .as_deref()
                        .filter(|url| is_external_url(url))
                        .map(|url| format!(r#"<a href="{}">Artifact</a>"#, escape_html(url)))
                        .unwrap_or_else(|| "&mdash;".to_owned());
                    format!(
                        r#"<tr><th scope="row">{}</th><td><span class="gate gate-{}">{}</span></td><td>{} passed / {} failed</td><td>{}</td><td>{artifact}</td></tr>"#,
                        escape_html(&gate.name),
                        gate_class(gate.status),
                        gate_label(gate.status),
                        gate.passed,
                        gate.failed,
                        format_duration(gate.duration_ms),
                    )
                })
                .collect::<String>();
    let matrix = if rows.is_empty() {
        empty_state("No gate results have been published.")
    } else {
        format!(
            r#"<div class="table-wrap"><table><caption>Latest gate matrix</caption><thead><tr><th>Gate</th><th>Status</th><th>Checks</th><th>Duration</th><th>Evidence</th></tr></thead><tbody>{rows}</tbody></table></div>"#
        )
    };

    let baseline = inputs
        .gallery
        .as_ref()
        .and_then(|gallery| gallery.entries.last());
    let evidence_html = summary
        .map(|summary| render_verification_evidence(summary, baseline))
        .unwrap_or_default();

    format!(
        r#"{matrix}
        {evidence_html}
        <p class="section-link"><a href="{}">Open the workflow run</a></p>"#,
        escape_html(&inputs.workflow.run_url)
    )
}

/// The published metric table, failure list, and run provenance.
fn render_verification_evidence(
    summary: &VerificationSummary,
    baseline: Option<&GalleryEntry>,
) -> String {
    let metrics = summary
        .metrics
        .iter()
        .map(|(name, value)| {
            let change = baseline
                .and_then(|entry| entry.metrics.get(name))
                .map_or_else(
                    || "&mdash;".to_owned(),
                    |before| escape_html(&format_delta(value - before)),
                );
            format!(
                r#"<tr><th scope="row"><code>{}</code></th><td>{}</td><td>{change}</td></tr>"#,
                escape_html(name),
                escape_html(&format_metric(*value)),
            )
        })
        .collect::<String>();

    let failures = if summary.metric_failures.is_empty() {
        String::new()
    } else {
        let rows = summary
            .metric_failures
            .iter()
            .map(|failure| {
                format!(
                    r#"<tr><th scope="row"><code>{}</code></th><td>{}</td><td>{}</td></tr>"#,
                    escape_html(&failure.metric),
                    escape_html(&format_metric(failure.value)),
                    escape_html(&failure.expected),
                )
            })
            .collect::<String>();
        format!(
            r#"<div class="table-wrap"><table><caption>Failed metrics</caption><thead><tr><th>Metric</th><th>Measured</th><th>Required</th></tr></thead><tbody>{rows}</tbody></table></div>"#
        )
    };

    let stage = summary
        .failed_stage
        .as_deref()
        .map(|stage| {
            format!(
                r#"<p class="failure-stage">Verification stopped in stage <code>{}</code>. The full log stays in the workflow run.</p>"#,
                escape_html(stage)
            )
        })
        .unwrap_or_default();

    format!(
        r#"{stage}
        <div class="table-wrap"><table><caption>Published metrics</caption><thead><tr><th>Metric</th><th>Value</th><th>Change</th></tr></thead><tbody>{metrics}</tbody></table></div>
        {failures}
        {}
        <p class="section-link"><a href="{VERIFICATION_FILE}">Open the sanitized verification report</a></p>"#,
        render_run_provenance(summary),
    )
}

/// The exact source paths and hashes the verified run measured.
fn render_run_provenance(summary: &VerificationSummary) -> String {
    let groups = [
        ("Verification sources", &summary.hashes.sources),
        ("Generated assets", &summary.hashes.assets),
        ("Asset sources", &summary.hashes.asset_sources),
        ("Approved references", &summary.hashes.references),
    ]
    .into_iter()
    .filter(|(_, hashes)| !hashes.is_empty())
    .map(|(title, hashes)| {
        let rows = hashes
            .iter()
            .map(|(path, hash)| {
                format!(
                    r#"<tr><th scope="row"><code>{}</code></th><td><code>{}</code></td></tr>"#,
                    escape_html(path),
                    escape_html(hash)
                )
            })
            .collect::<String>();
        format!(
            r#"<div class="table-wrap"><table><caption>{}</caption><thead><tr><th>Path</th><th>SHA-256</th></tr></thead><tbody>{rows}</tbody></table></div>"#,
            escape_html(title)
        )
    })
    .collect::<String>();

    format!(
        r#"<dl class="provenance">
          <div><dt>Semantic hash</dt><dd><code>{}</code></dd></div>
          <div><dt>Camera</dt><dd><code>{} / {} / MSAA {}</code></dd></div>
        </dl>
        {groups}"#,
        escape_html(&summary.semantic_visual_hash),
        escape_html(&summary.camera.tonemapping),
        escape_html(&summary.camera.deband_dither),
        summary.camera.msaa_samples,
    )
}

/// A published metric value, without trailing noise on whole numbers.
fn format_metric(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1.0e15 {
        format!("{value:.0}")
    } else {
        format!("{value:.3}")
    }
}

/// A published metric change, always signed so direction is unambiguous.
fn format_delta(delta: f64) -> String {
    if delta == 0.0 {
        "no change".to_owned()
    } else if delta.fract() == 0.0 && delta.abs() < 1.0e15 {
        format!("{delta:+.0}")
    } else {
        format!("{delta:+.3}")
    }
}

fn render_commits(repo: &RepoFacts) -> String {
    if repo.commits.is_empty() {
        return empty_state("No commit summaries were provided.");
    }
    repo.commits
        .iter()
        .map(|commit| {
            let task = commit
                .task_id
                .as_deref()
                .map(|task| {
                    format!(
                        r##"<a href="#plan-{}">{}</a>"##,
                        escape_html(task),
                        escape_html(task)
                    )
                })
                .unwrap_or_else(|| "Unassociated".to_owned());
            format!(
                r#"<article class="commit-entry">
          <div class="commit-sha">{}</div>
          <div><h3>{}</h3><p><time>{}</time> · {task}</p></div>
        </article>"#,
                escape_html(&short_sha(&commit.sha)),
                escape_html(&commit.subject),
                escape_html(&commit.committed_at),
            )
        })
        .collect()
}

fn render_plan_html(markdown: &str) -> String {
    let parser = Parser::new_ext(
        markdown,
        Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES,
    );
    let mut output = String::new();
    let mut pending = Vec::new();
    let mut events = parser.peekable();

    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                html::push_html(&mut output, pending.drain(..));
                let mut inner = Vec::new();
                let mut heading_text = String::new();
                for event in events.by_ref() {
                    if matches!(event, Event::End(TagEnd::Heading(_))) {
                        break;
                    }
                    if let Event::Text(text) | Event::Code(text) = &event {
                        heading_text.push_str(text);
                    }
                    inner.push(sanitize_markdown_event(event));
                }
                let task_ids = task_ids_for_heading(heading_text.trim());
                let tag = heading_tag(level);
                if let Some((first, rest)) = task_ids.split_first() {
                    output.push_str(&format!(r#"<{tag} id="plan-{first}">"#));
                    html::push_html(&mut output, inner.into_iter());
                    output.push_str(&format!("</{tag}>"));
                    for task_id in rest {
                        output.push_str(&format!(
                            r#"<span class="plan-anchor" id="plan-{task_id}"></span>"#
                        ));
                    }
                } else {
                    output.push_str(&format!("<{tag}>"));
                    html::push_html(&mut output, inner.into_iter());
                    output.push_str(&format!("</{tag}>"));
                }
            }
            event => pending.push(sanitize_markdown_event(event)),
        }
    }
    html::push_html(&mut output, pending.into_iter());
    output
}

fn render_template(template: &str, replacements: &[(&str, String)]) -> String {
    let replacements = replacements
        .iter()
        .map(|(token, value)| (*token, value.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut rendered = String::with_capacity(template.len());
    let mut remainder = template;

    while let Some(start) = remainder.find("{{") {
        rendered.push_str(&remainder[..start]);
        let token_start = &remainder[start..];
        let Some(relative_end) = token_start.find("}}") else {
            rendered.push_str(token_start);
            return rendered;
        };
        let end = start + relative_end + 2;
        let token = &remainder[start..end];
        if let Some(value) = replacements.get(token) {
            rendered.push_str(value);
        } else {
            rendered.push_str(token);
        }
        remainder = &remainder[end..];
    }
    rendered.push_str(remainder);
    rendered
}

fn sanitize_markdown_event(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Html(value) | Event::InlineHtml(value) => {
            Event::Text(CowStr::from(value.into_string()))
        }
        event => event,
    }
}

fn heading_tag(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "h1",
        HeadingLevel::H2 => "h2",
        HeadingLevel::H3 => "h3",
        HeadingLevel::H4 => "h4",
        HeadingLevel::H5 => "h5",
        HeadingLevel::H6 => "h6",
    }
}

fn validate_local_target(
    output: &Path,
    ids: &BTreeSet<String>,
    retained: &BTreeSet<String>,
    source: &Path,
    target: &str,
) -> Result<(), SitegenError> {
    if target.is_empty() {
        return Err(invalid_html(source, "empty link or resource target"));
    }
    if is_external_url(target) || target.starts_with("mailto:") {
        return Ok(());
    }
    if target.starts_with('/') || target.starts_with("file:") || target.contains(":\\") {
        return Err(invalid_html(source, "absolute local path is present"));
    }
    if let Some(fragment) = target.strip_prefix('#') {
        if ids.contains(fragment) {
            return Ok(());
        }
        return Err(SitegenError::BrokenLocalLink {
            source: PathBuf::from("index.html"),
            target: PathBuf::from(target),
        });
    }

    let path = target
        .split(['?', '#'])
        .next()
        .map(PathBuf::from)
        .unwrap_or_default();
    if output.join(&path).is_file() || retained.contains(target) {
        Ok(())
    } else {
        Err(SitegenError::BrokenLocalLink {
            source: PathBuf::from("index.html"),
            target: path,
        })
    }
}

/// The first absolute local path the generated page publishes, if any.
///
/// A rule that names one platform's home directory only ever says something
/// about the machine the last leak came from: the same page rendered on macOS
/// carries `/Users/...`, on a runner `/home/runner/...`, and from a Windows
/// checkout a drive letter. What all of them have in common is that they are
/// absolute, so that is what is refused — across the text the page really
/// renders and across every attribute value it emits, whatever produced the
/// content. The repository roots are still named exactly, because a path
/// inside one is otherwise indistinguishable from the relative paths the site
/// publishes on purpose.
fn published_absolute_path(repository: &Path, document: &Html, html: &str) -> Option<String> {
    for root in [repository.to_path_buf(), default_repository()] {
        for candidate in [Some(root.clone()), fs::canonicalize(&root).ok()]
            .into_iter()
            .flatten()
        {
            let text = candidate.to_string_lossy().into_owned();
            if candidate.is_absolute() && text.len() > 1 && html.contains(&text) {
                return Some(text);
            }
        }
    }
    if html.contains("file://") {
        return Some("file://".to_owned());
    }

    for node in document.tree.nodes() {
        match node.value() {
            Node::Text(text) => {
                if let Some(found) = absolute_path_token(text) {
                    return Some(found);
                }
            }
            Node::Element(element) => {
                for (_, value) in element.attrs() {
                    if let Some(found) = absolute_path_token(value) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The first whitespace-delimited token of one published string that names an
/// absolute filesystem path.
///
/// An absolute path names at least two components below the root: that is what
/// separates `/home/runner/work` and `/Users/someone/checkout` from the single
/// URL path prefix (`/midcreek-cs-1/`) this project's own prose really talks
/// about, and from a lone separator or a prose slash. Every relative path the
/// site publishes on purpose fails the leading-root test outright.
fn absolute_path_token(value: &str) -> Option<String> {
    value.split_whitespace().find_map(|token| {
        // Only trailing punctuation is dropped: a leading `.` is part of the
        // relative paths the site publishes on purpose, and trimming it would
        // turn `../midcreek-concept/...` into something that looks absolute.
        let token = token
            .trim_start_matches(['"', '\'', '(', '[', '{', '`'])
            .trim_end_matches(['"', '\'', ')', ']', '}', ',', ';', ':', '.', '`']);
        let posix = token.strip_prefix('/').is_some_and(|rest| {
            rest.starts_with(|value: char| value.is_ascii_alphanumeric())
                && rest.split('/').filter(|part| !part.is_empty()).count() >= 2
        });
        let bytes = token.as_bytes();
        let windows = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\');
        (posix || windows).then(|| token.to_owned())
    })
}

/// Every history frame the published gallery manifest vouches for.
fn retained_history_targets(output: &Path) -> BTreeSet<String> {
    history_frames(output).0
}

/// The history frames the published manifest vouches for, and the ones it
/// declares outside the entry that owns them.
///
/// Every accepted point publishes into its own commit's directory, so a frame
/// path that merely starts with the history prefix is not evidence of that
/// point at all: it can name another entry's pixels, which exist, resolve, and
/// would satisfy both the link checker and the retained-history rule while
/// showing a different moment in time under this entry's commit and hash.
fn history_frames(output: &Path) -> (BTreeSet<String>, Vec<String>) {
    let Ok(json) = fs::read_to_string(output.join(GALLERY_FILE)) else {
        return (BTreeSet::new(), Vec::new());
    };
    let Ok(gallery) = serde_json::from_str::<GalleryManifest>(&json) else {
        return (BTreeSet::new(), Vec::new());
    };

    let mut declared = BTreeSet::new();
    let mut misscoped = Vec::new();
    for entry in gallery.entries {
        let root = format!("{HISTORY_SCREENSHOTS}/{}/", short_sha(&entry.source_commit));
        for path in entry.frames.into_values() {
            if !path.starts_with(HISTORY_SCREENSHOTS) {
                continue;
            }
            let owned = path
                .strip_prefix(&root)
                .is_some_and(|file| !file.is_empty() && !file.contains('/'));
            if owned {
                declared.insert(path);
            } else if !misscoped.contains(&path) {
                misscoped.push(path);
            }
        }
    }
    (declared, misscoped)
}

fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static selector should be valid")
}

fn invalid_html(path: &Path, message: impl Into<String>) -> SitegenError {
    SitegenError::InvalidHtml {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn empty_state(message: &str) -> String {
    format!(
        r#"<div class="empty-state"><span class="empty-mark">+</span><p>{}</p></div>"#,
        escape_html(message)
    )
}

/// The published link to one commit in this repository.
fn commit_url(commit: &str) -> String {
    format!("{REPOSITORY_URL}/commit/{commit}")
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

fn gate_label(status: GateStatus) -> &'static str {
    match status {
        GateStatus::Passed => "Passed",
        GateStatus::Failed => "Failed",
        GateStatus::SkippedDependency => "Not run",
    }
}

fn gate_class(status: GateStatus) -> &'static str {
    match status {
        GateStatus::Passed => "passed",
        GateStatus::Failed => "failed",
        GateStatus::SkippedDependency => "skipped",
    }
}

fn format_duration(duration_ms: u64) -> String {
    if duration_ms < 1_000 {
        format!("{duration_ms} ms")
    } else {
        format!("{:.2} s", duration_ms as f64 / 1_000.0)
    }
}

fn is_external_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}
