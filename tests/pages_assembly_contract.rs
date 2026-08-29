use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use midcreek_cs_1::sitegen::{BuildDisposition, GateStatus, WorkflowSummary, assemble_site};
use sha2::{Digest, Sha256};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn first_run_without_game_publishes_status_only() {
    let current = fixture_site("status-only", &[("index.html", "CURRENT SOURCE: GREEN")]);
    let output = TempDirectory::new("first-run-output");

    let disposition = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FirstRunStatusOnly);
    assert!(output.path().join("index.html").exists());
    assert!(!output.path().join("play/game_bg.wasm").exists());
}

#[test]
fn failed_run_retains_previous_game() {
    let previous = fixture_site(
        "previous-green",
        &[
            ("index.html", "PREVIOUS SOURCE: GREEN"),
            ("play/game_bg.wasm", "last-known-good-game"),
            ("screenshots/center.png", "last-known-good-frame"),
            ("last-green.json", r#"{"source_commit":"old"}"#),
        ],
    );
    let current = fixture_site(
        "current-failed",
        &[
            ("index.html", "CURRENT SOURCE: FAILED"),
            ("site.css", "current styles"),
        ],
    );
    let output = TempDirectory::new("failed-output");
    let old_hash = sha256(previous.path().join("play/game_bg.wasm"));

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FailedRetainLastGreen);
    assert_eq!(sha256(output.path().join("play/game_bg.wasm")), old_hash);
    assert_eq!(
        fs::read_to_string(output.path().join("screenshots/center.png")).unwrap(),
        "last-known-good-frame"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("last-green.json")).unwrap(),
        r#"{"source_commit":"old"}"#
    );
    assert!(
        fs::read_to_string(output.path().join("index.html"))
            .unwrap()
            .contains("CURRENT SOURCE: FAILED")
    );
}

#[test]
fn assembly_never_deletes_a_nonempty_caller_directory() {
    let current = fixture_site("status-only", &[("index.html", "CURRENT SOURCE: GREEN")]);
    let output = TempDirectory::new("nonempty-output");
    fs::write(output.path().join("caller-owned.txt"), "keep me").unwrap();

    let result = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        output.path(),
    );

    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(output.path().join("caller-owned.txt")).unwrap(),
        "keep me"
    );
    assert!(!output.path().join("index.html").exists());
}

#[test]
fn invalid_current_site_does_not_create_the_output_directory() {
    let root = TempDirectory::new("missing-current");
    let current = root.path().join("missing");
    let output = root.path().join("output");

    let result = assemble_site(
        None,
        &current,
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        &output,
    );

    assert!(result.is_err());
    assert!(!output.exists());
}

mod workflow_contract {
    use super::*;

    #[test]
    fn triggers_only_for_main_pushes_and_manual_dispatch() {
        let workflow = workflow_source();

        assert!(workflow.contains("push:\n    branches: [main]"));
        assert!(workflow.contains("workflow_dispatch:"));
        assert_eq!(workflow.matches("branches:").count(), 1);
        assert!(!workflow.contains("pull_request"));
        assert!(!workflow.contains("pull_request_target"));
    }

    #[test]
    fn serializes_pages_publication_without_canceling_an_active_run() {
        let workflow = workflow_source();

        assert!(workflow.contains("concurrency:\n  group: pages\n  cancel-in-progress: false"));
        assert!(publish_job(&workflow).contains("if: always()"));
    }

    #[test]
    fn deploys_with_the_official_pages_actions_without_secret_references() {
        let workflow = workflow_source();

        assert!(workflow.contains("actions/upload-pages-artifact@"));
        assert!(workflow.contains("actions/deploy-pages@"));
        assert!(!workflow.contains("${{ secrets."));
    }

    #[test]
    fn grants_write_permissions_only_to_publish() {
        let workflow = workflow_source();
        let jobs = workflow
            .split_once("jobs:\n")
            .expect("workflow should declare jobs");
        let verify = verify_job(&workflow);
        let publish = publish_job(&workflow);

        assert!(!jobs.0.contains("permissions:"));
        assert!(verify.contains("permissions:\n      contents: read"));
        assert!(!verify.contains("contents: write"));
        assert!(!verify.contains("pages: write"));
        assert!(!verify.contains("id-token: write"));
        assert!(!verify.contains("actions: read"));
        for permission in [
            "contents: write",
            "pages: write",
            "id-token: write",
            "actions: read",
        ] {
            assert!(
                publish.contains(permission),
                "Publish should include {permission}"
            );
        }
    }

    fn workflow_source() -> String {
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/pages.yml"),
        )
        .expect("Pages workflow should be checked in")
    }

    fn verify_job(workflow: &str) -> &str {
        workflow
            .split_once("  verify:\n")
            .and_then(|(_, jobs)| jobs.split_once("  publish:\n"))
            .map(|(verify, _)| verify)
            .expect("workflow should contain Verify before Publish")
    }

    fn publish_job(workflow: &str) -> &str {
        workflow
            .split_once("  publish:\n")
            .map(|(_, publish)| publish)
            .expect("workflow should contain Publish")
    }
}

fn workflow_summary(native: GateStatus, web: GateStatus) -> WorkflowSummary {
    WorkflowSummary {
        source_commit: "1111111111111111111111111111111111111111".to_owned(),
        run_url: "https://github.com/ridermw/midcreek-cs-1/actions/runs/123".to_owned(),
        native,
        web,
        gates: Vec::new(),
    }
}

fn fixture_site(name: &str, files: &[(&str, &str)]) -> TempDirectory {
    let directory = TempDirectory::new(name);
    for (relative, contents) in files {
        let path = directory.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }
    directory
}

fn sha256(path: impl AsRef<Path>) -> String {
    let bytes = fs::read(path).unwrap();
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn new(name: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "midcreek-pages-{name}-{}-{unique}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
