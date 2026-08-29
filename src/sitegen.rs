use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use image::ImageReader;
use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};
use scraper::{Html, Selector};
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

#[derive(Debug, Eq, PartialEq)]
pub enum SitegenError {
    Io { path: PathBuf, message: String },
    Json { path: PathBuf, message: String },
    Progress(Vec<ProgressError>),
    Reference(Vec<ReferenceError>),
    Markdown { path: PathBuf, message: String },
    MissingInput { path: PathBuf },
    UnsafeOutputPath { path: PathBuf },
    BrokenLocalLink { source: PathBuf, target: PathBuf },
    MissingAltText { source: PathBuf },
    InvalidHtml { path: PathBuf, message: String },
    MissingPreviousArtifact { path: PathBuf },
    OutputNotEmpty { path: PathBuf },
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

pub fn build_site(inputs: &SiteInputs, output: &Path) -> Result<SiteManifest, SitegenError> {
    validate_progress(
        &inputs.progress,
        &plan_task_ids_from_markdown(&inputs.plan_markdown),
        &inputs.repo,
    )
    .map_err(SitegenError::Progress)?;
    validate_reference_manifest(
        &inputs.reference_manifest,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )
    .map_err(SitegenError::Reference)?;
    prepare_output(output)?;

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
    let reference_paths = copy_reference_assets(&inputs.reference_manifest, output)?;

    let replacements = [
        ("{{PROJECT}}", escape_html(&inputs.progress.project)),
        (
            "{{STATUS}}",
            render_status(inputs, current_task, &source_commit, updated_at),
        ),
        ("{{PLAY}}", render_play()),
        (
            "{{COMPARISON}}",
            render_comparison(&inputs.reference_manifest, &reference_paths),
        ),
        (
            "{{PROGRESS}}",
            render_progress(&inputs.progress, &inputs.repo),
        ),
        (
            "{{SCREENSHOTS}}",
            render_screenshots(inputs.verification.as_ref()),
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
    validate_site_output(output, &inputs.progress)?;

    let mut generated_files = vec![
        PathBuf::from("index.html"),
        PathBuf::from("site.css"),
        PathBuf::from("site.js"),
    ];
    generated_files.extend(reference_paths.values().cloned());
    generated_files.sort();

    Ok(SiteManifest {
        source_commit,
        playable_commit: None,
        current_task: current_task.map(|task| task.id.clone()),
        generated_files,
        semantic_visual_hash: inputs
            .verification
            .as_ref()
            .map(|report| report.semantic_visual_hash.clone()),
    })
}

pub fn assemble_site(
    previous: Option<&Path>,
    current: &Path,
    workflow: &WorkflowSummary,
    output: &Path,
) -> Result<BuildDisposition, SitegenError> {
    require_directory(current)?;
    prepare_assembly_output(output)?;

    let green = workflow.native == GateStatus::Passed && workflow.web == GateStatus::Passed;
    let disposition = if green {
        BuildDisposition::GreenReplacement
    } else if let Some(previous) = previous {
        copy_retained_artifacts(previous, output)?;
        BuildDisposition::FailedRetainLastGreen
    } else {
        BuildDisposition::FirstRunStatusOnly
    };

    copy_site_tree(current, current, output, !green)?;
    Ok(disposition)
}

pub fn validate_site_output(
    output: &Path,
    progress: &ProgressDocument,
) -> Result<(), SitegenError> {
    let index_path = output.join("index.html");
    let html = fs::read_to_string(&index_path).map_err(|error| SitegenError::Io {
        path: index_path.clone(),
        message: error.to_string(),
    })?;
    let document = Html::parse_document(&html);

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

    for element in document.select(&selector("a[href], img[src], link[href], script[src]")) {
        let attribute = if element.value().attr("href").is_some() {
            "href"
        } else {
            "src"
        };
        let target = element
            .value()
            .attr(attribute)
            .expect("the selector guarantees a target");
        validate_local_target(output, &ids, &index_path, target)?;
    }

    if html.contains(env!("CARGO_MANIFEST_DIR"))
        || html.contains("/Users/")
        || html.contains("file://")
    {
        return Err(invalid_html(&index_path, "absolute local path is present"));
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

fn prepare_output(output: &Path) -> Result<(), SitegenError> {
    validate_output_path(output)?;
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

fn copy_retained_artifacts(previous: &Path, output: &Path) -> Result<(), SitegenError> {
    require_directory(previous)?;
    for relative in ["play", "screenshots", "last-green.json"] {
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

fn copy_site_tree(
    root: &Path,
    source: &Path,
    output: &Path,
    skip_retained: bool,
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
        if skip_retained && is_retained_artifact(relative) {
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
        return copy_site_tree(source, source, destination, false);
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

fn is_retained_artifact(relative: &Path) -> bool {
    matches!(
        relative.components().next(),
        Some(std::path::Component::Normal(name))
            if name == "play" || name == "screenshots" || name == "last-green.json"
    )
}

pub fn validate_output_path(output: &Path) -> Result<(), SitegenError> {
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

    let repository =
        fs::canonicalize(env!("CARGO_MANIFEST_DIR")).map_err(|error| SitegenError::Io {
            path: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            message: error.to_string(),
        })?;
    let resolved = resolve_destination(output)?;
    let target_root = repository.join("target");
    if resolved.starts_with(&repository) && !resolved.starts_with(target_root) {
        return Err(SitegenError::UnsafeOutputPath {
            path: output.to_path_buf(),
        });
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
    manifest: &ReferenceManifest,
    output: &Path,
) -> Result<BTreeMap<String, PathBuf>, SitegenError> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
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

fn render_play() -> String {
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

fn render_comparison(
    manifest: &ReferenceManifest,
    reference_paths: &BTreeMap<String, PathBuf>,
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

    format!(
        r#"<div class="comparison-grid">
          <article class="comparison-card comparison-card-wide">
            <div class="panel-heading"><span class="eyebrow">Key art / current frame</span><span class="pending-chip">Current frame pending</span></div>
            <div class="comparison-stage" data-comparison style="--comparison: 50%">
              <div class="comparison-layer comparison-reference">
                <img src="{}" alt="Approved Cel Shift key art reference">
              </div>
              <div class="comparison-layer comparison-current" data-comparison-current>
                <div class="comparison-pending">No verified current frame</div>
              </div>
              <input data-compare-control type="range" min="0" max="100" value="50" aria-label="Reveal approved key art versus the pending current frame">
            </div>
            <p class="sr-only">The approved key art is available. The current verified game frame does not exist yet.</p>
            {}
          </article>
          <article class="comparison-card">
            <div class="panel-heading"><span class="eyebrow">Character target</span><span class="pending-chip">Worker pending</span></div>
            <div class="character-comparison">
              <img src="{}" alt="Approved Cel Shift technician character sheet">
              <div class="worker-pending">No verified worker crop</div>
            </div>
            <p class="sr-only">The approved character sheet is shown beside a clear placeholder for the future verified worker crop.</p>
            {}
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
          <div><dt>SHA-256</dt><dd><code>{}</code></dd></div>
        </dl>"#,
        escape_html(&asset.source_path),
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
                        r#"<a class="commit-link" href="https://github.com/ridermw/midcreek-cs-1/commit/{commit}">Commit {}</a>"#,
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

fn render_screenshots(verification: Option<&VerificationSummary>) -> String {
    match verification {
                Some(report) if !report.frames.is_empty() => report
                    .frames
                    .iter()
                    .map(|(name, path)| {
                        format!(
                            r#"<article class="screenshot-entry"><div class="timeline-dot"></div><div><span class="eyebrow">{}</span><p>{}</p></div></article>"#,
                            escape_html(name),
                            escape_html(path)
                        )
                    })
                    .collect(),
                _ => empty_state(
                    "No verified screenshots yet. The timeline starts after the first deterministic render passes.",
                ),
            }
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
                                r#"<a href="https://github.com/ridermw/midcreek-cs-1/commit/{commit}">Resolved in {}</a>"#,
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
    let gates = inputs
        .workflow
        .gates
        .iter()
        .chain(
            inputs
                .verification
                .iter()
                .flat_map(|report| report.gates.iter()),
        )
        .collect::<Vec<_>>();
    if gates.is_empty() {
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
    format!(
        r#"<div class="table-wrap"><table><thead><tr><th>Gate</th><th>Status</th><th>Checks</th><th>Duration</th><th>Evidence</th></tr></thead><tbody>{rows}</tbody></table></div>
        <p class="section-link"><a href="{}">Open the workflow run</a></p>"#,
        escape_html(&inputs.workflow.run_url)
    )
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
    if output.join(&path).is_file() {
        Ok(())
    } else {
        Err(SitegenError::BrokenLocalLink {
            source: PathBuf::from("index.html"),
            target: path,
        })
    }
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
