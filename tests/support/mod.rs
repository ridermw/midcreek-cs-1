use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use midcreek_cs_1::{
    sitegen::{
        BrowserGateReport, CommitSummary, GalleryManifest, ProgressDocument, ProgressError,
        ReferenceManifest, RepoFacts, SiteInputs, SiteManifest, SitegenError, VerificationEvidence,
        WorkflowSummary, build_site, validate_progress,
    },
    verification::VerificationReport,
};
use scraper::{Html, Selector};
use sha2::{Digest, Sha256};

static SITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path.as_ref()).expect("fixture should be readable")
}

pub fn sha256(path: impl AsRef<Path>) -> String {
    let bytes = fs::read(path.as_ref()).expect("file should be readable");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn sha256_text(path: impl AsRef<Path>) -> String {
    let source = read(path);
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    Sha256::digest(normalized.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn relative_url(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

#[cfg(unix)]
pub fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
pub fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(unix)]
pub fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
pub fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

pub fn bash_command() -> PathBuf {
    if cfg!(windows) {
        if let Ok(output) = std::process::Command::new("git")
            .arg("--exec-path")
            .output()
            && output.status.success()
        {
            let exec_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
            for ancestor in exec_path.ancestors() {
                for relative in ["bin/bash.exe", "usr/bin/bash.exe"] {
                    let candidate = ancestor.join(relative);
                    if candidate.is_file() {
                        return candidate;
                    }
                }
            }
        }
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
            if let Some(root) = std::env::var_os(variable) {
                let candidate = PathBuf::from(root).join(if variable == "LocalAppData" {
                    "Programs/Git/bin/bash.exe"
                } else {
                    "Git/bin/bash.exe"
                });
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        panic!("Git Bash should be installed on Windows");
    }
    PathBuf::from("bash")
}

pub fn python_command() -> PathBuf {
    for candidate in ["python3", "python"] {
        if std::process::Command::new(candidate)
            .args([
                "-c",
                "import sys; raise SystemExit(sys.version_info < (3, 8))",
            ])
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return PathBuf::from(candidate);
        }
    }
    panic!("Python 3.8 or newer should be installed");
}

pub fn fixture(name: &str) -> ProgressDocument {
    let path = fixture_root().join(name);
    serde_json::from_str(&read(path)).expect("fixture should match the progress schema")
}

pub fn validate_fixture(name: &str) -> Vec<ProgressError> {
    validate_progress(&fixture(name), &plan_ids(), &repo_facts())
        .expect_err("fixture should fail validation")
}

pub fn plan_ids() -> BTreeSet<String> {
    ["foundation-contracts", "pages-foundation"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

pub fn repo_facts() -> RepoFacts {
    let head_sha = "1111111111111111111111111111111111111111".to_owned();
    RepoFacts {
        known_commits: BTreeSet::from([
            head_sha.clone(),
            "2222222222222222222222222222222222222222".to_owned(),
        ]),
        head_sha,
        commits: Vec::new(),
    }
}

pub struct GeneratedSite {
    root: PathBuf,
    manifest: SiteManifest,
}

impl GeneratedSite {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn index_html(&self) -> String {
        read(self.root.join("index.html"))
    }

    pub fn manifest(&self) -> &SiteManifest {
        &self.manifest
    }
}

impl Drop for GeneratedSite {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn build_fixture_site(name: &str) -> Result<GeneratedSite, SitegenError> {
    let inputs = site_inputs(name);
    build_site_from_inputs(name, &inputs)
}

pub fn build_site_from_inputs(
    name: &str,
    inputs: &SiteInputs,
) -> Result<GeneratedSite, SitegenError> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    let output = std::env::temp_dir().join(format!(
        "midcreek-generated-site-{}-{unique}-{}-{}",
        std::process::id(),
        name,
        SITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let manifest = build_site(inputs, &output)?;

    Ok(GeneratedSite {
        root: output,
        manifest,
    })
}

pub fn site_inputs(name: &str) -> SiteInputs {
    let root = fixture_root().join(name);
    let progress = serde_json::from_str(&read(root.join("progress.json")))
        .expect("progress fixture should match the strict schema");
    let reference_manifest = serde_json::from_str::<ReferenceManifest>(&read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/manifest.json"),
    ))
    .expect("reference fixture should match the strict schema");
    let workflow = serde_json::from_str::<WorkflowSummary>(&read(root.join("workflow.json")))
        .expect("workflow fixture should match the strict schema");
    let repo = fixture_repo_facts(name);
    let (verification, gallery) = match name {
        "verified-game" => (Some(green_evidence()), Some(prior_gallery())),
        "failed-verification" => (Some(failed_evidence()), Some(prior_gallery())),
        _ => (None, None),
    };
    SiteInputs {
        progress,
        plan_markdown: read(root.join("plan.md")),
        reference_manifest,
        verification,
        gallery,
        workflow,
        repo,
        playable: None,
    }
}

/// The directory a real `--verify-output` run writes, as a committed fixture.
pub fn verification_root() -> PathBuf {
    fixture_root().join("verification")
}

/// The directory `scripts/web-smoke.sh` writes, as a committed fixture.
pub fn browser_root() -> PathBuf {
    fixture_root().join("browser")
}

/// One committed raw report, parsed through the game's own strict schema.
pub fn raw_report(file_name: &str) -> VerificationReport {
    serde_json::from_str(&read(verification_root().join(file_name)))
        .expect("the fixture report should match the game's canonical schema")
}

/// The committed raw browser gate document.
pub fn raw_browser() -> BrowserGateReport {
    serde_json::from_str(&read(browser_root().join("browser-gate.json")))
        .expect("the fixture gate summary should match the browser gate schema")
}

/// The sanitized projection of the green fixture run.
pub fn green_evidence() -> VerificationEvidence {
    let browser = raw_browser();
    VerificationEvidence::project(
        &raw_report("report.json"),
        &verification_root(),
        Some((&browser, browser_root().as_path())),
    )
    .expect("the green fixture should project")
}

/// The sanitized projection of the failed fixture run.
pub fn failed_evidence() -> VerificationEvidence {
    VerificationEvidence::project(
        &raw_report("failed-report.json"),
        &verification_root(),
        None,
    )
    .expect("a failed run still projects its public evidence")
}

/// The gallery a previous `pages-live` publication left behind.
pub fn prior_gallery() -> GalleryManifest {
    serde_json::from_str(&read(fixture_root().join("gallery/prior-gallery.json")))
        .expect("the gallery fixture should match the strict schema")
}

pub fn assert_has_element_id(html: &str, id: &str) {
    let document = Html::parse_document(html);
    let selector = Selector::parse(&format!("#{id}")).expect("ID selector should be valid");
    assert_eq!(
        document.select(&selector).count(),
        1,
        "expected exactly one element with id {id}"
    );
}

pub fn assert_text(html: &str, selector: &str, expected: &str) {
    let document = Html::parse_document(html);
    let selector = Selector::parse(selector).expect("fixture selector should be valid");
    let text = document
        .select(&selector)
        .flat_map(|node| node.text())
        .collect::<String>();
    assert!(
        text.contains(expected),
        "expected {selector:?} text to contain {expected:?}, got {text:?}"
    );
}

fn fixture_repo_facts(name: &str) -> RepoFacts {
    let head_sha = "1111111111111111111111111111111111111111".to_owned();
    let hostile = (name == "hostile-content").then_some("<script>alert(\"commit\")</script>");
    RepoFacts {
        known_commits: BTreeSet::from([
            head_sha.clone(),
            "2222222222222222222222222222222222222222".to_owned(),
        ]),
        head_sha: head_sha.clone(),
        commits: vec![
            CommitSummary {
                sha: head_sha,
                subject: hostile.unwrap_or("Render the progress hub").to_owned(),
                committed_at: "2026-08-29T21:30:00Z".to_owned(),
                task_id: Some("pages-foundation".to_owned()),
            },
            CommitSummary {
                sha: "2222222222222222222222222222222222222222".to_owned(),
                subject: "Establish reviewed contracts".to_owned(),
                committed_at: "2026-08-29T20:00:00Z".to_owned(),
                task_id: Some("foundation-contracts".to_owned()),
            },
        ],
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sitegen")
}
