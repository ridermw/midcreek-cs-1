use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use midcreek_cs_1::sitegen::{
    BuildDisposition, GateStatus, SitegenError, WorkflowSummary, assemble_site,
};
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
fn status_only_run_retains_previous_game_without_failure_disposition() {
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
        "current-status-only",
        &[
            ("index.html", "CURRENT SOURCE: PASSED; WEB NOT RUN"),
            ("site.css", "current styles"),
        ],
    );
    let output = TempDirectory::new("status-only-output");
    let old_hash = sha256(previous.path().join("play/game_bg.wasm"));

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &status_only_workflow(),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::RetainLastGreen);
    assert_eq!(sha256(output.path().join("play/game_bg.wasm")), old_hash);
    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(index.contains("CURRENT SOURCE: PASSED; WEB NOT RUN"));
    assert!(!index.contains("FAILED"));
}

#[test]
fn native_failure_retains_previous_game_with_failure_disposition() {
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
fn web_failure_retains_previous_game_with_failure_disposition() {
    let previous = fixture_site(
        "previous-green-web-failure",
        &[("play/game_bg.wasm", "last-known-good-game")],
    );
    let current = fixture_site(
        "current-web-failure",
        &[("index.html", "CURRENT SOURCE: FAILED")],
    );
    let output = TempDirectory::new("web-failure-output");
    let old_hash = sha256(previous.path().join("play/game_bg.wasm"));

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Failed),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FailedRetainLastGreen);
    assert_eq!(sha256(output.path().join("play/game_bg.wasm")), old_hash);
}

#[test]
fn assemble_cli_reports_status_only_retention_without_failure_label() {
    let previous = fixture_site(
        "previous-green-cli",
        &[("play/game_bg.wasm", "last-known-good-game")],
    );
    let current = fixture_site(
        "current-status-only-cli",
        &[("index.html", "CURRENT SOURCE: PASSED; WEB NOT RUN")],
    );
    let output = TempDirectory::new("status-only-cli-output");
    let result_path = fixture_root().join("pages/native-passed-web-skipped.json");

    let command = Command::new(env!("CARGO_BIN_EXE_sitegen"))
        .args([
            "assemble",
            "--previous",
            previous.path().to_str().unwrap(),
            "--current",
            current.path().to_str().unwrap(),
            "--result",
            result_path.to_str().unwrap(),
            "--output",
            output.path().to_str().unwrap(),
        ])
        .output()
        .expect("sitegen should launch");

    assert_eq!(command.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(command.stdout).unwrap(),
        "RetainLastGreen\n"
    );
    assert!(command.stderr.is_empty());
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

    #[test]
    fn installs_bevy_linux_prerequisites_before_cargo_in_both_jobs() {
        let workflow = workflow_source();

        for (name, job) in [
            ("Verify", verify_job(&workflow)),
            ("Publish", publish_job(&workflow)),
        ] {
            let install = job
                .find("apt-get install -y --no-install-recommends")
                .unwrap_or_else(|| panic!("{name} should install native build prerequisites"));
            let first_cargo = job
                .find("cargo ")
                .unwrap_or_else(|| panic!("{name} should run Cargo"));

            assert!(
                install < first_cargo,
                "{name} should install native build prerequisites before Cargo"
            );
            for package in [
                "pkg-config",
                "libwayland-dev",
                "libxkbcommon-dev",
                "libxkbcommon-x11-dev",
                "libudev-dev",
                "libx11-dev",
                "libxi-dev",
                "libxrandr-dev",
            ] {
                assert!(
                    job[install..first_cargo].contains(package),
                    "{name} should install {package} before Cargo"
                );
            }
        }
    }

    #[test]
    fn builds_the_web_game_only_after_verification_passes() {
        let workflow = workflow_source();
        let web = web_job(&workflow);

        assert!(web.contains("needs: verify"), "{web}");
        // A job-level condition sits at four spaces; step conditions are deeper.
        assert!(!web.contains("\n    if:"), "{web}");
        assert!(web.contains("permissions:\n      contents: read"), "{web}");
        assert!(!web.contains("contents: write"), "{web}");
    }

    #[test]
    fn the_web_job_installs_the_pinned_toolchain_and_runs_both_web_gates() {
        let workflow = workflow_source();
        let web = web_job(&workflow);

        for fragment in [
            "rustup target add wasm32-unknown-unknown",
            "cargo install wasm-bindgen-cli --version",
            "google-chrome-stable",
            "./scripts/build-web.sh",
            "./scripts/web-smoke.sh",
            "actions/upload-artifact@",
        ] {
            assert!(
                web.contains(fragment),
                "Build web should contain {fragment}"
            );
        }
    }

    #[test]
    fn the_web_job_resolves_the_preinstalled_chrome_instead_of_installing_chromium() {
        let workflow = workflow_source();
        let web = web_job(&workflow);

        assert!(
            !web.contains("chromium-browser"),
            "Build web must not install or hardcode chromium-browser: {web}"
        );
        assert!(
            !web.contains("/usr/bin/"),
            "Build web must not hardcode a browser path: {web}"
        );

        let resolved = web
            .find("command -v google-chrome-stable")
            .expect("Build web should resolve the preinstalled Chrome");
        let asserted = web
            .find("no preinstalled Google Chrome")
            .expect("Build web should fail loudly when Chrome is absent");
        let exported = web
            .find(r#"echo "CHROME=$chrome" >> "$GITHUB_ENV""#)
            .expect("Build web should export the resolved Chrome");
        let smoke = web
            .find("./scripts/web-smoke.sh")
            .expect("Build web should run the browser gate");

        assert!(web.contains("command -v google-chrome"), "{web}");
        assert!(
            resolved < asserted && asserted < exported && exported < smoke,
            "Chrome must be resolved and asserted before the browser gate: {web}"
        );
    }

    #[test]
    fn publish_waits_for_both_gates_and_promotes_only_a_green_playable_build() {
        let workflow = workflow_source();
        let publish = publish_job(&workflow);

        assert!(publish.contains("if: always()"), "{publish}");
        assert!(publish.contains("needs: [verify, build-web]"), "{publish}");
        assert!(publish.contains("needs.build-web.result"), "{publish}");
        assert!(publish.contains("actions/download-artifact@"), "{publish}");
        assert!(publish.contains("playable"), "{publish}");
    }

    fn workflow_source() -> String {
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/pages.yml"),
        )
        .expect("Pages workflow should be checked in")
    }

    fn verify_job(workflow: &str) -> &str {
        job(workflow, "verify")
    }

    fn publish_job(workflow: &str) -> &str {
        job(workflow, "publish")
    }

    fn web_job(workflow: &str) -> &str {
        job(workflow, "build-web")
    }

    /// One job body, from its declaration to the next job at the same indent.
    fn job<'source>(workflow: &'source str, name: &str) -> &'source str {
        let start = workflow
            .find(&format!("\n  {name}:\n"))
            .unwrap_or_else(|| panic!("workflow should declare the {name} job"))
            + name.len()
            + 5;
        let body = &workflow[start..];
        body.match_indices("\n  ")
            .find(|(offset, _)| {
                body[offset + 3..].split_once(':').is_some_and(|(head, _)| {
                    !head.is_empty()
                        && head
                            .chars()
                            .all(|value| value.is_ascii_lowercase() || value == '-')
                })
            })
            .map_or(body, |(offset, _)| &body[..offset])
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

fn status_only_workflow() -> WorkflowSummary {
    serde_json::from_str(
        &fs::read_to_string(fixture_root().join("pages/native-passed-web-skipped.json")).unwrap(),
    )
    .unwrap()
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
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

#[test]
fn a_green_run_replaces_the_previous_playable_build_and_last_green_metadata() {
    let previous = fixture_site(
        "previous-green-game",
        &[
            ("index.html", "PREVIOUS SOURCE: GREEN"),
            ("play/game_bg.wasm", "last-known-good-game"),
            ("play/index.html", "old shell"),
            ("screenshots/center.png", "last-known-good-frame"),
            ("last-green.json", r#"{"source_commit":"old"}"#),
        ],
    );
    let mut current_files = vec![
        ("index.html", "CURRENT SOURCE: GREEN"),
        ("screenshots/center.png", "verified-new-frame"),
        ("last-green.json", r#"{"source_commit":"new"}"#),
    ];
    current_files.extend_from_slice(COMPLETE_PACKAGE);
    let current = fixture_site("current-green-game", &current_files);
    let output = TempDirectory::new("green-replacement-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::GreenReplacement);
    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "verified-new-game"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("last-green.json")).unwrap(),
        r#"{"source_commit":"new"}"#
    );
    assert_eq!(
        fs::read_to_string(output.path().join("screenshots/center.png")).unwrap(),
        "verified-new-frame"
    );
}

/// A complete current `play/` package, exactly as `build-web.sh` produces it.
const COMPLETE_PACKAGE: &[(&str, &str)] = &[
    ("play/index.html", "new shell"),
    ("play/play.js", "// bootstrap"),
    ("play/play.css", "/* shell */"),
    ("play/game.js", "export default function init() {}"),
    ("play/game_bg.wasm", "verified-new-game"),
    ("play/assets/generated/rack.glb", "glTF"),
];

#[test]
fn a_green_run_with_an_incomplete_package_refuses_to_wipe_the_previous_game() {
    let previous = fixture_site(
        "previous-green-complete",
        &[
            ("index.html", "PREVIOUS SOURCE: GREEN"),
            ("play/index.html", "old shell"),
            ("play/play.js", "// old bootstrap"),
            ("play/play.css", "/* old shell */"),
            ("play/game.js", "// old bindings"),
            ("play/game_bg.wasm", "last-known-good-game"),
            ("play/assets/generated/rack.glb", "glTF"),
            ("last-green.json", r#"{"source_commit":"old"}"#),
        ],
    );
    // The current green site lost its WASM payload, its stylesheet, and every
    // generated asset. Replacing the previous game with it would publish a
    // broken build and destroy the last known good one.
    let current = fixture_site(
        "current-green-incomplete",
        &[
            ("index.html", "CURRENT SOURCE: GREEN"),
            ("play/index.html", "new shell"),
            ("play/play.js", "// bootstrap"),
            ("play/game.js", "export default function init() {}"),
        ],
    );
    let root = TempDirectory::new("incomplete-green-root");
    let output = root.path().join("output");

    let result = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        &output,
    );

    match result {
        Err(SitegenError::IncompletePlayablePackage { path, missing }) => {
            assert_eq!(path, current.path().join("play"));
            assert_eq!(missing, ["play.css", "game_bg.wasm", "assets"]);
        }
        other => panic!("{other:?}"),
    }
    assert!(!output.exists(), "the output must be left untouched");
    assert_eq!(
        fs::read_to_string(previous.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
    );
}

#[test]
fn a_green_run_that_lost_its_package_entirely_refuses_to_wipe_the_previous_game() {
    let previous = fixture_site(
        "previous-green-lost",
        &[
            ("index.html", "PREVIOUS SOURCE: GREEN"),
            ("play/index.html", "old shell"),
            ("play/play.js", "// old bootstrap"),
            ("play/play.css", "/* old shell */"),
            ("play/game.js", "// old bindings"),
            ("play/game_bg.wasm", "last-known-good-game"),
            ("play/assets/generated/rack.glb", "glTF"),
        ],
    );
    let current = fixture_site(
        "current-green-no-package",
        &[("index.html", "CURRENT SOURCE: GREEN")],
    );
    let root = TempDirectory::new("lost-green-root");
    let output = root.path().join("output");

    let result = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        &output,
    );

    match result {
        Err(SitegenError::IncompletePlayablePackage { path, missing }) => {
            assert_eq!(path, current.path().join("play"));
            assert_eq!(missing, ["play/"]);
        }
        other => panic!("{other:?}"),
    }
    assert!(!output.exists(), "the output must be left untouched");
    assert_eq!(
        fs::read_to_string(previous.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
    );
}

#[test]
fn a_green_first_run_without_any_game_still_publishes_status_only() {
    let current = fixture_site(
        "current-green-status-only",
        &[("index.html", "CURRENT SOURCE: GREEN")],
    );
    let output = TempDirectory::new("green-status-only-output");

    let disposition = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::GreenReplacement);
    assert!(output.path().join("index.html").exists());
}
