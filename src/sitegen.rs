use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use image::ImageReader;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::design::{
    CHARACTER_SHEET_REFERENCE_PATH, CHARACTER_SHEET_SHA256, KEY_ART_REFERENCE_PATH, KEY_ART_SHA256,
};

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

#[derive(Clone, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationSummary {
    pub semantic_visual_hash: String,
    pub frames: BTreeMap<String, String>,
    pub gates: Vec<GateSummary>,
    pub metrics: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SiteInputs {
    pub progress: ProgressDocument,
    pub plan_markdown: String,
    pub reference_manifest: ReferenceManifest,
    pub verification: Option<VerificationSummary>,
    pub workflow: WorkflowSummary,
    pub repo: RepoFacts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildDisposition {
    GreenReplacement,
    FailedRetainLastGreen,
    FirstRunStatusOnly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SiteManifest {
    pub source_commit: String,
    pub playable_commit: Option<String>,
    pub current_task: Option<String>,
    pub generated_files: Vec<PathBuf>,
    pub semantic_visual_hash: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GalleryEntry {
    pub semantic_visual_hash: String,
    pub source_commit: String,
    pub committed_at: String,
    pub current_task: String,
    pub frames: BTreeMap<String, String>,
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
    pub semantic_visual_hash: String,
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

#[derive(Debug)]
pub enum SitegenError {
    Io { path: PathBuf, message: String },
    Json { path: PathBuf, message: String },
    Progress(Vec<ProgressError>),
    Markdown { path: PathBuf, message: String },
    MissingInput { path: PathBuf },
    UnsafeOutputPath { path: PathBuf },
    BrokenLocalLink { source: PathBuf, target: PathBuf },
    MissingAltText { source: PathBuf },
    InvalidHtml { path: PathBuf, message: String },
    MissingPreviousArtifact { path: PathBuf },
}

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
                .and_then(|commit| resolve_commit_ref(commit, repo))
                .is_none()
            {
                errors.push(ProgressError::MissingChallengeContext {
                    challenge_id: challenge.id.clone(),
                    field: "resolved_commit".to_owned(),
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
    let mapped = match heading {
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
    };

    ids.extend(mapped.iter().map(|id| (*id).to_owned()));
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
