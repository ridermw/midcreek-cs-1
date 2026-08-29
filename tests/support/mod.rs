use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use midcreek_cs_1::sitegen::{
    CommitSummary, ProgressDocument, ProgressError, ReferenceManifest, RepoFacts, SiteInputs,
    SiteManifest, SitegenError, WorkflowSummary, build_site, validate_progress,
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
    let manifest = build_site(&inputs, &output)?;

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
    SiteInputs {
        progress,
        plan_markdown: read(root.join("plan.md")),
        reference_manifest,
        verification: None,
        workflow,
        repo,
    }
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
