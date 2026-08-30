use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use midcreek_cs_1::sitegen::{
    BuildDisposition, GateStatus, LastGreenManifest, SitegenError, WorkflowSummary, assemble_site,
    validate_assembled_links,
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
        let installer = repository().join("scripts/install-wasm-toolchain.sh");
        let source = fs::read_to_string(&installer).expect("the installer should be checked in");

        for fragment in [
            "./scripts/install-wasm-toolchain.sh",
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
        // The pinned toolchain is installed by a repository-owned script, so
        // the version the CLI is pinned to is read from the lockfile once
        // rather than duplicated into fragile workflow shell.
        assert!(source.contains("rustup target add wasm32-unknown-unknown"));
        assert!(source.contains("cargo install wasm-bindgen-cli --version"));
        assert!(source.contains(r#"awk '"#), "{source}");
        assert!(!web.contains("cargo install"), "{web}");
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

    // -----------------------------------------------------------------------
    // Complete outcome capture
    // -----------------------------------------------------------------------

    /// Every gate the design names has to appear, and every one of them has to
    /// go through the recording runner. A gate invoked directly would fail the
    /// step at the first red result and leave every later gate unmeasured.
    #[test]
    fn every_named_native_gate_is_measured_by_the_recording_runner() {
        let workflow = workflow_source();
        let verify = verify_job(&workflow);
        let gates = step(verify, "Run the native gates");
        let named = [
            "Published progress data",
            "Rust formatting",
            "Clippy lints",
            "Generated asset freshness",
            "Library and binary tests",
            "Asset contracts",
            "Application contracts",
            "Site generation contracts",
            "Pages assembly contracts",
            "Rendered image contracts",
            "Release build",
        ];

        for name in named {
            assert!(
                gates.contains(&format!(r#"run-gate.sh "$GATES" "{name}" --"#)),
                "the {name:?} gate should be measured: {gates}"
            );
        }
        assert_eq!(
            gates.matches("run-gate.sh").count(),
            named.len(),
            "every command in the gate step should be a named gate: {gates}"
        );
        assert!(
            !gates.contains("set -euo"),
            "the gate step must not abort at the first failure: {gates}"
        );
    }

    /// The site publishes `Rendered image contracts` as a real verdict, so it
    /// has to come from the serialized render test and nothing else.
    #[test]
    fn the_rendered_image_contract_gate_is_the_serialized_render_test() {
        let workflow = workflow_source();
        let gates = step(verify_job(&workflow), "Run the native gates");
        let start = gates
            .find(r#""Rendered image contracts" --"#)
            .expect("the render gate should be named");
        let command = gates[start..]
            .lines()
            .nth(1)
            .expect("the gate should name a command");

        assert!(
            command.contains("cargo test --test render_contract"),
            "{command}"
        );
        assert!(command.contains("--test-threads=1"), "{command}");
        assert!(command.contains("xvfb-run"), "{command}");
    }

    /// The evidence Publish may project has to be generated into a root the
    /// workflow owns, and the run's captured output must never travel with it.
    #[test]
    fn the_native_job_lifts_only_the_report_and_frames_into_its_evidence_root() {
        let workflow = workflow_source();
        let verify = verify_job(&workflow);
        let collect = step(verify, "Collect the verification evidence");

        assert!(
            verify.contains("RESULT: ${{ runner.temp }}/native"),
            "{verify}"
        );
        assert!(collect.contains("if: always()"), "{collect}");
        assert!(
            collect.contains(r#"destination="$RESULT/verification""#),
            "{collect}"
        );
        assert!(collect.contains(r#"cp "$source/report.json""#), "{collect}");
        assert!(collect.contains("-name '*.png'"), "{collect}");
        assert!(!collect.contains("stdout.log"), "{collect}");
        assert!(!collect.contains("stderr.log"), "{collect}");
    }

    /// Each job records what it measured, uploads it, and only then concludes.
    /// Removing any of the three steps, or its `if: always()`, loses the
    /// diagnostics of exactly the runs that need them most.
    #[test]
    fn each_job_uploads_its_result_before_it_concludes_a_failure() {
        let workflow = workflow_source();
        for (job, summarize, upload, verdict) in [
            (
                verify_job(&workflow),
                "Summarize the native gates",
                "Upload the native result",
                "Conclude the native verdict",
            ),
            (
                web_job(&workflow),
                "Summarize the web gates",
                "Upload the web result",
                "Conclude the web verdict",
            ),
        ] {
            assert!(
                step_offset(job, summarize) < step_offset(job, upload),
                "{summarize} should run before {upload}"
            );
            assert!(
                step_offset(job, upload) < step_offset(job, verdict),
                "{upload} should run before {verdict}"
            );
            for name in [summarize, upload, verdict] {
                assert!(
                    step(job, name).contains("if: always()"),
                    "{name} should run even after a failed gate"
                );
            }
            assert!(step(job, upload).contains("actions/upload-artifact@"));
            assert!(step(job, upload).contains("if-no-files-found: warn"));
        }
    }

    /// A job concludes once, in its verdict step. Any step from the first
    /// measured gate onwards that failed on its own would abandon the measured
    /// results before they were uploaded, and hide which gate really went red.
    ///
    /// Steps before the first gate are preconditions: they have measured
    /// nothing yet, so failing there abandons nothing, and Publish still
    /// publishes the job as failed.
    #[test]
    fn only_the_verdict_step_fails_a_job_once_a_gate_has_been_measured() {
        let workflow = workflow_source();
        for (job, gates, summarize, verdict) in [
            (
                verify_job(&workflow),
                "Run the native gates",
                "Summarize the native gates",
                "Conclude the native verdict",
            ),
            (
                web_job(&workflow),
                "Run the web gates",
                "Summarize the web gates",
                "Conclude the web verdict",
            ),
        ] {
            let summary = step(job, summarize);
            assert!(summary.contains("|| echo failed"), "{summary}");
            assert!(!summary.contains("set -euo"), "{summary}");

            let conclusion = step(job, verdict);
            assert!(
                conclusion.contains(r#"if [[ "$STATUS" != "passed" ]]"#),
                "{conclusion}"
            );
            assert!(conclusion.contains("exit 1"), "{conclusion}");

            let measured = &job[step_offset(job, gates)..];
            assert_eq!(
                measured.matches("exit 1").count(),
                1,
                "only the verdict may fail once a gate has been measured: {measured}"
            );
        }
    }

    /// The verdict reads the summary through an environment binding rather
    /// than by expanding a workflow expression into the shell.
    #[test]
    fn the_verdict_never_expands_an_expression_into_its_shell() {
        let workflow = workflow_source();
        for (job, verdict) in [
            (verify_job(&workflow), "Conclude the native verdict"),
            (web_job(&workflow), "Conclude the web verdict"),
        ] {
            let conclusion = step(job, verdict);
            let script = conclusion
                .split_once("run: |")
                .expect("the verdict should run a script")
                .1;
            assert!(conclusion.contains("STATUS: ${{ steps."), "{conclusion}");
            assert!(!script.contains("${{"), "{script}");
        }
    }

    // -----------------------------------------------------------------------
    // Status-always publication
    // -----------------------------------------------------------------------

    /// Publish runs whatever the upstream jobs did, so a download that cannot
    /// succeed must not stop the publication.
    #[test]
    fn publish_downloads_whichever_results_exist_without_failing() {
        let workflow = workflow_source();
        let publish = publish_job(&workflow);

        for name in ["Download the native result", "Download the web result"] {
            let body = step(publish, name);
            assert!(body.contains("continue-on-error: true"), "{name}: {body}");
            assert!(
                body.contains("actions/download-artifact@"),
                "{name}: {body}"
            );
        }
        assert!(step(publish, "Check out previous Pages site").contains("continue-on-error: true"));
        assert!(step(publish, "Check out previous Pages site").contains("ref: pages-live"));
    }

    /// Every upstream outcome, every downloaded artifact, and the previous
    /// publication are handed to one repository-owned decision rather than
    /// re-derived in workflow shell.
    #[test]
    fn publish_hands_every_upstream_outcome_to_one_repository_owned_decision() {
        let workflow = workflow_source();
        let body = step(publish_job(&workflow), "Build and assemble current status");

        for fragment in [
            "VERIFY_RESULT: ${{ needs.verify.result }}",
            "WEB_RESULT: ${{ needs.build-web.result }}",
            "NATIVE_DOWNLOAD: ${{ steps.native.outcome }}",
            "WEB_DOWNLOAD: ${{ steps.web.outcome }}",
            "PREVIOUS_CHECKOUT: ${{ steps.previous.outcome }}",
            "sitegen -- inputs",
            "--native-outcome",
            "--web-outcome",
            "--source-commit",
            "--run-url",
            "sitegen -- build",
            "sitegen -- assemble",
        ] {
            assert!(
                body.contains(fragment),
                "Publish should pass {fragment}: {body}"
            );
        }
        // The site is never generated from an expression the runner expanded
        // into the shell it runs.
        let script = body
            .split_once("run: |")
            .expect("Publish should run a script")
            .1;
        assert!(!script.contains("${{"), "{script}");
    }

    /// Retention is chosen by assembling against the previous publication.
    /// Assembling without it would republish a status page with no game.
    #[test]
    fn publish_assembles_the_current_status_against_the_previous_publication() {
        let workflow = workflow_source();
        let body = step(publish_job(&workflow), "Build and assemble current status");
        let assemble = body
            .find("sitegen -- assemble")
            .expect("Publish should assemble");

        assert!(
            body[assemble..].contains(r#""${previous_args[@]}""#),
            "assembly should select retention from the previous publication: {body}"
        );
        assert!(
            body[assemble..].contains(r#"--result "$pages_root/workflow.json""#),
            "assembly should read the merged result: {body}"
        );
        assert!(
            body.contains(r#"previous_args=(--previous "$GITHUB_WORKSPACE/previous-pages")"#),
            "{body}"
        );
    }

    /// The generated branch is disposable and must say so, and Pages must
    /// serve the assembled tree rather than anything else on the runner.
    #[test]
    fn the_generated_branch_is_marked_and_deployed_from_the_assembled_output() {
        let workflow = workflow_source();
        let publish = publish_job(&workflow);
        let body = step(publish, "Build and assemble current status");
        let notice = repository().join(".github/pages-live-README.md");

        assert!(body.contains(r#"touch "$output/.nojekyll""#), "{body}");
        assert!(body.contains("pages-live-README.md"), "{body}");
        let warning = fs::read_to_string(&notice).expect("the branch notice should be checked in");
        assert!(warning.contains("generated"), "{warning}");
        assert!(warning.contains("force-pushes"), "{warning}");

        assert!(
            step(publish, "Push generated Pages branch").contains("push --force origin pages-live")
        );
        assert!(
            step(publish, "Upload Pages artifact")
                .contains("path: ${{ runner.temp }}/pages/output"),
            "{publish}"
        );
        assert!(step(publish, "Deploy Pages").contains("actions/deploy-pages@"));
    }

    fn workflow_source() -> String {
        fs::read_to_string(repository().join(".github/workflows/pages.yml"))
            .expect("Pages workflow should be checked in")
    }

    fn repository() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// One step body, from its name to the next step at the same indent.
    fn step<'source>(job: &'source str, name: &str) -> &'source str {
        let marker = format!("- name: {name}\n");
        let start = job
            .find(&marker)
            .unwrap_or_else(|| panic!("the job should declare the {name:?} step"))
            + marker.len();
        let body = &job[start..];
        body.find("\n      - name: ")
            .map_or(body, |offset| &body[..offset])
    }

    /// Where one step begins, so an order between two steps can be asserted.
    fn step_offset(job: &str, name: &str) -> usize {
        job.find(&format!("- name: {name}\n"))
            .unwrap_or_else(|| panic!("the job should declare the {name:?} step"))
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

// ---------------------------------------------------------------------------
// Screenshot history and gallery retention
// ---------------------------------------------------------------------------

#[test]
fn a_green_replacement_keeps_the_previous_screenshot_history() {
    let previous = fixture_site(
        "previous-history",
        &[
            ("index.html", "PREVIOUS SOURCE: GREEN"),
            ("gallery.json", r#"{"entries":[{"source_commit":"old"}]}"#),
            (
                "screenshots/current/01-healthy-center-ne.png",
                "old current",
            ),
            (
                "screenshots/history/22222222/01-healthy-center-ne.png",
                "old history",
            ),
        ],
    );
    let mut current_files = vec![
        ("index.html", "CURRENT SOURCE: GREEN"),
        ("gallery.json", r#"{"entries":[{"source_commit":"new"}]}"#),
        (
            "screenshots/current/01-healthy-center-ne.png",
            "new current",
        ),
        (
            "screenshots/history/11111111/01-healthy-center-ne.png",
            "new history",
        ),
    ];
    current_files.extend_from_slice(COMPLETE_PACKAGE);
    let current = fixture_site("current-history", &current_files);
    let output = TempDirectory::new("history-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::GreenReplacement);
    assert_eq!(
        fs::read_to_string(
            output
                .path()
                .join("screenshots/history/22222222/01-healthy-center-ne.png")
        )
        .unwrap(),
        "old history",
        "a green publication must never lose the visual history"
    );
    assert_eq!(
        fs::read_to_string(
            output
                .path()
                .join("screenshots/history/11111111/01-healthy-center-ne.png")
        )
        .unwrap(),
        "new history"
    );
    assert_eq!(
        fs::read_to_string(
            output
                .path()
                .join("screenshots/current/01-healthy-center-ne.png")
        )
        .unwrap(),
        "new current",
        "the current frames are replaced wholesale"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("gallery.json")).unwrap(),
        r#"{"entries":[{"source_commit":"new"}]}"#
    );
}

#[test]
fn a_failed_run_retains_the_previous_gallery_and_screenshots() {
    let previous = fixture_site(
        "previous-gallery",
        &[
            ("index.html", "PREVIOUS SOURCE: GREEN"),
            ("gallery.json", r#"{"entries":[{"source_commit":"old"}]}"#),
            (
                "screenshots/current/01-healthy-center-ne.png",
                "old current",
            ),
            (
                "screenshots/history/22222222/01-healthy-center-ne.png",
                "old history",
            ),
            ("play/game_bg.wasm", "last-known-good-game"),
        ],
    );
    let current = fixture_site(
        "current-failed-gallery",
        &[("index.html", "CURRENT SOURCE: FAILED")],
    );
    let output = TempDirectory::new("failed-gallery-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FailedRetainLastGreen);
    assert_eq!(
        fs::read_to_string(output.path().join("gallery.json")).unwrap(),
        r#"{"entries":[{"source_commit":"old"}]}"#
    );
    assert_eq!(
        fs::read_to_string(
            output
                .path()
                .join("screenshots/current/01-healthy-center-ne.png")
        )
        .unwrap(),
        "old current"
    );
    assert_eq!(
        fs::read_to_string(
            output
                .path()
                .join("screenshots/history/22222222/01-healthy-center-ne.png")
        )
        .unwrap(),
        "old history"
    );
}

#[test]
fn a_first_green_run_without_a_previous_site_publishes_its_own_history() {
    let mut files = vec![
        ("index.html", "CURRENT SOURCE: GREEN"),
        ("gallery.json", r#"{"entries":[]}"#),
        (
            "screenshots/history/11111111/01-healthy-center-ne.png",
            "new history",
        ),
    ];
    files.extend_from_slice(COMPLETE_PACKAGE);
    let current = fixture_site("first-history", &files);
    let output = TempDirectory::new("first-history-output");

    let disposition = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::GreenReplacement);
    assert!(
        output
            .path()
            .join("screenshots/history/11111111/01-healthy-center-ne.png")
            .is_file()
    );
}

// ---------------------------------------------------------------------------
// Verified evidence is its own domain, retained independently of the game
// ---------------------------------------------------------------------------

/// The manifest a previous green publication left behind, naming one accepted
/// history point.
const PRIOR_GALLERY: &str = r#"{"entries":[{"semantic_visual_hash":"aaaaaaaa","source_commit":"2222222222222222222222222222222222222222","committed_at":"2026-08-28T00:00:00Z","current_task":"pages-verification","frames":{"center":"screenshots/history/22222222/01-healthy-center-ne.png"},"metrics":{},"metric_deltas":{}}]}"#;

/// The same manifest after a later green run opened a second point.
const TWO_POINT_GALLERY: &str = r#"{"entries":[{"semantic_visual_hash":"aaaaaaaa","source_commit":"2222222222222222222222222222222222222222","committed_at":"2026-08-28T00:00:00Z","current_task":"pages-verification","frames":{"center":"screenshots/history/22222222/01-healthy-center-ne.png"},"metrics":{},"metric_deltas":{}},{"semantic_visual_hash":"bbbbbbbb","source_commit":"1111111111111111111111111111111111111111","committed_at":"2026-08-30T00:00:00Z","current_task":"pages-status-always","frames":{"center":"screenshots/history/11111111/01-healthy-center-ne.png"},"metrics":{},"metric_deltas":{}}]}"#;

const OLD_HISTORY: &str = "screenshots/history/22222222/01-healthy-center-ne.png";
const NEW_HISTORY: &str = "screenshots/history/11111111/01-healthy-center-ne.png";
const CURRENT_FRAME: &str = "screenshots/current/01-healthy-center-ne.png";

/// A green projection, exactly as `verification.json` records one.
const GREEN_PROJECTION: &str = r#"{"succeeded":true,"semantic_visual_hash":"bbbbbbbb"}"#;

/// A failed projection: the current status, with no pixels behind it. It still
/// carries the run's semantic hash, exactly as `VerificationSummary` does.
const FAILED_PROJECTION: &str =
    r#"{"succeeded":false,"failed_stage":"repair","semantic_visual_hash":"cccccccc"}"#;

/// An index that links exactly the given site-relative targets.
fn index_linking(targets: &[&str]) -> String {
    let images = targets
        .iter()
        .map(|target| format!(r#"<img src="{target}" alt="a verified frame">"#))
        .collect::<String>();
    format!(
        "<!doctype html><html><head><title>Hub</title></head><body><main>{images}</main></body></html>"
    )
}

/// A previous publication carrying a complete green evidence set.
fn previous_with_evidence(name: &str, index: &str) -> TempDirectory {
    let mut files = vec![
        ("index.html", index),
        ("gallery.json", PRIOR_GALLERY),
        ("verification.json", GREEN_PROJECTION),
        (CURRENT_FRAME, "old current"),
        (OLD_HISTORY, "old history"),
        ("play/game_bg.wasm", "last-known-good-game"),
        ("play/index.html", "old shell"),
        (
            "last-green.json",
            r#"{"source_commit":"2222222222222222222222222222222222222222","semantic_visual_hash":"aaaaaaaa","game_files":["play/game_bg.wasm","play/index.html"],"screenshot_files":["screenshots/current/01-healthy-center-ne.png"]}"#,
        ),
    ];
    files.sort();
    fixture_site(name, &files)
}

#[test]
fn a_green_replacement_without_current_evidence_keeps_the_manifest_that_names_its_history() {
    let previous_index = index_linking(&[OLD_HISTORY, CURRENT_FRAME]);
    let previous = previous_with_evidence("prior-evidence", &previous_index);
    // The next green build carries no verification evidence at all: no
    // projection, no gallery, no screenshots.
    let current_index = index_linking(&[]);
    let mut files = vec![("index.html", current_index.as_str())];
    files.extend_from_slice(COMPLETE_PACKAGE);
    let current = fixture_site("green-without-evidence", &files);
    let output = TempDirectory::new("green-without-evidence-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::GreenReplacement);
    assert_eq!(
        fs::read_to_string(output.path().join("gallery.json")).unwrap(),
        PRIOR_GALLERY,
        "history images without the manifest that names them are orphans"
    );
    assert_eq!(
        fs::read_to_string(output.path().join(OLD_HISTORY)).unwrap(),
        "old history"
    );
    assert_eq!(
        fs::read_to_string(output.path().join(CURRENT_FRAME)).unwrap(),
        "old current",
        "a build that verified nothing must not drop the last verified frames"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("verification.json")).unwrap(),
        GREEN_PROJECTION,
        "the last published evidence stays beside the pixels it describes"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "verified-new-game",
        "the playable domain is still replaced wholesale"
    );
}

#[test]
fn two_later_builds_keep_the_earliest_history_named_and_renderable() {
    let first_index = index_linking(&[OLD_HISTORY, CURRENT_FRAME]);
    let first = previous_with_evidence("chain-first", &first_index);

    // Build two: green, but it verified nothing, so it publishes no evidence.
    let quiet_index = index_linking(&[]);
    let mut quiet_files = vec![("index.html", quiet_index.as_str())];
    quiet_files.extend_from_slice(COMPLETE_PACKAGE);
    let quiet = fixture_site("chain-quiet", &quiet_files);
    let second = TempDirectory::new("chain-second-output");
    assert_eq!(
        assemble_site(
            Some(first.path()),
            quiet.path(),
            &workflow_summary(GateStatus::Passed, GateStatus::Passed),
            second.path(),
        )
        .unwrap(),
        BuildDisposition::GreenReplacement
    );

    // Build three: green with new evidence. It writes only its own history
    // point, and its page links both points, exactly as the generator renders
    // a two-entry gallery.
    let loud_index = index_linking(&[OLD_HISTORY, NEW_HISTORY, CURRENT_FRAME]);
    let mut loud_files = vec![
        ("index.html", loud_index.as_str()),
        ("gallery.json", TWO_POINT_GALLERY),
        ("verification.json", GREEN_PROJECTION),
        (CURRENT_FRAME, "new current"),
        (NEW_HISTORY, "new history"),
    ];
    loud_files.extend_from_slice(COMPLETE_PACKAGE);
    let loud = fixture_site("chain-loud", &loud_files);
    let third = TempDirectory::new("chain-third-output");

    let disposition = assemble_site(
        Some(second.path()),
        loud.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        third.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::GreenReplacement);
    assert_eq!(
        fs::read_to_string(third.path().join(OLD_HISTORY)).unwrap(),
        "old history",
        "the earliest accepted point survives two later builds"
    );
    assert_eq!(
        fs::read_to_string(third.path().join(NEW_HISTORY)).unwrap(),
        "new history"
    );
    assert_eq!(
        fs::read_to_string(third.path().join("gallery.json")).unwrap(),
        TWO_POINT_GALLERY,
        "the manifest must still name the earliest point"
    );
    // Named and renderable: every link the assembled page makes resolves
    // against the assembled tree.
    validate_assembled_links(third.path()).unwrap();
}

#[test]
fn a_status_only_run_publishes_current_evidence_while_retaining_the_previous_game() {
    let previous_index = index_linking(&[OLD_HISTORY, CURRENT_FRAME]);
    let previous = previous_with_evidence("retain-game-prior", &previous_index);
    // Native verification passed and the web gate was skipped: there is no new
    // package, but the native run produced complete evidence.
    let current_index = index_linking(&[OLD_HISTORY, NEW_HISTORY, CURRENT_FRAME]);
    let current = fixture_site(
        "retain-game-current",
        &[
            ("index.html", current_index.as_str()),
            ("gallery.json", TWO_POINT_GALLERY),
            ("verification.json", GREEN_PROJECTION),
            (CURRENT_FRAME, "new current"),
            (NEW_HISTORY, "new history"),
        ],
    );
    let output = TempDirectory::new("retain-game-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &status_only_workflow(),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::RetainLastGreen);
    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game",
        "the playable domain is retained"
    );
    assert_eq!(
        fs::read_to_string(output.path().join(CURRENT_FRAME)).unwrap(),
        "new current",
        "the evidence domain is promoted on its own merit"
    );
    assert_eq!(
        fs::read_to_string(output.path().join(NEW_HISTORY)).unwrap(),
        "new history"
    );
    assert_eq!(
        fs::read_to_string(output.path().join(OLD_HISTORY)).unwrap(),
        "old history"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("gallery.json")).unwrap(),
        TWO_POINT_GALLERY
    );
    validate_assembled_links(output.path()).unwrap();
}

#[test]
fn a_status_only_run_without_evidence_retains_the_previous_evidence_and_game() {
    let previous_index = index_linking(&[OLD_HISTORY, CURRENT_FRAME]);
    let previous = previous_with_evidence("retain-both-prior", &previous_index);
    let current_index = index_linking(&[]);
    let current = fixture_site(
        "retain-both-current",
        &[("index.html", current_index.as_str())],
    );
    let output = TempDirectory::new("retain-both-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &status_only_workflow(),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::RetainLastGreen);
    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
    );
    assert_eq!(
        fs::read_to_string(output.path().join(CURRENT_FRAME)).unwrap(),
        "old current"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("gallery.json")).unwrap(),
        PRIOR_GALLERY
    );
    assert_eq!(
        fs::read_to_string(output.path().join("verification.json")).unwrap(),
        GREEN_PROJECTION
    );
}

#[test]
fn a_failed_run_publishes_its_failure_while_retaining_the_previous_pixels() {
    let previous_index = index_linking(&[OLD_HISTORY, CURRENT_FRAME]);
    let previous = previous_with_evidence("failure-prior", &previous_index);
    let current_index = index_linking(&[]);
    let current = fixture_site(
        "failure-current",
        &[
            ("index.html", current_index.as_str()),
            ("verification.json", FAILED_PROJECTION),
        ],
    );
    let output = TempDirectory::new("failure-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FailedRetainLastGreen);
    assert_eq!(
        fs::read_to_string(output.path().join("verification.json")).unwrap(),
        FAILED_PROJECTION,
        "a failure publishes its own status, not the last green one"
    );
    assert_eq!(
        fs::read_to_string(output.path().join(CURRENT_FRAME)).unwrap(),
        "old current"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("gallery.json")).unwrap(),
        PRIOR_GALLERY
    );
    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
    );
}

#[test]
fn a_web_failure_still_publishes_the_native_evidence_that_passed() {
    let previous_index = index_linking(&[OLD_HISTORY, CURRENT_FRAME]);
    let previous = previous_with_evidence("web-failure-prior", &previous_index);
    // Native verification passed and promoted its frames; only the browser
    // gate failed, so the previous package stays published.
    let current_index = index_linking(&[OLD_HISTORY, NEW_HISTORY, CURRENT_FRAME]);
    let current = fixture_site(
        "web-failure-current",
        &[
            ("index.html", current_index.as_str()),
            ("gallery.json", TWO_POINT_GALLERY),
            ("verification.json", GREEN_PROJECTION),
            (CURRENT_FRAME, "new current"),
            (NEW_HISTORY, "new history"),
        ],
    );
    let output = TempDirectory::new("web-failure-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Failed),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FailedRetainLastGreen);
    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
    );
    assert_eq!(
        fs::read_to_string(output.path().join(CURRENT_FRAME)).unwrap(),
        "new current"
    );
    validate_assembled_links(output.path()).unwrap();
}

#[test]
fn an_assembled_page_that_links_a_file_assembly_did_not_carry_is_refused() {
    let previous_index = index_linking(&[OLD_HISTORY, CURRENT_FRAME]);
    let previous = previous_with_evidence("dangling-prior", &previous_index);
    // The page links a history point no manifest declares and no build wrote.
    let current_index = index_linking(&["screenshots/history/deadbeef/01-healthy-center-ne.png"]);
    let current = fixture_site(
        "dangling-current",
        &[("index.html", current_index.as_str())],
    );
    let output = TempDirectory::new("dangling-output");

    let result = assemble_site(
        Some(previous.path()),
        current.path(),
        &status_only_workflow(),
        output.path(),
    );

    assert!(
        matches!(&result, Err(SitegenError::BrokenLocalLink { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn a_status_only_run_never_publishes_a_package_it_did_not_verify() {
    let previous_index = index_linking(&[OLD_HISTORY, CURRENT_FRAME]);
    let previous = previous_with_evidence("unverified-package-prior", &previous_index);
    // No gate verified this package, so it must never reach the site even
    // though the build left one behind.
    let current_index = index_linking(&[]);
    let current = fixture_site(
        "unverified-package-current",
        &[
            ("index.html", current_index.as_str()),
            ("play/game_bg.wasm", "unverified-game"),
            ("play/index.html", "unverified shell"),
            ("last-green.json", r#"{"source_commit":"unverified"}"#),
        ],
    );
    let output = TempDirectory::new("unverified-package-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &status_only_workflow(),
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::RetainLastGreen);
    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("play/index.html")).unwrap(),
        "old shell"
    );
    let metadata: LastGreenManifest =
        serde_json::from_str(&fs::read_to_string(output.path().join("last-green.json")).unwrap())
            .unwrap();
    assert_eq!(
        metadata.source_commit, "2222222222222222222222222222222222222222",
        "the retained game keeps the manifest that describes it"
    );
}

#[test]
fn a_failed_projection_never_relabels_the_retained_screenshots() {
    let previous_index = index_linking(&[OLD_HISTORY, CURRENT_FRAME]);
    let previous = previous_with_evidence("failed-hash-prior", &previous_index);
    let current_index = index_linking(&[]);
    let current = fixture_site(
        "failed-hash-current",
        &[
            ("index.html", current_index.as_str()),
            ("verification.json", FAILED_PROJECTION),
        ],
    );
    let output = TempDirectory::new("failed-hash-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        output.path(),
    )
    .unwrap();

    let metadata: LastGreenManifest =
        serde_json::from_str(&fs::read_to_string(output.path().join("last-green.json")).unwrap())
            .unwrap();

    assert_eq!(
        metadata.semantic_visual_hash.as_deref(),
        Some("aaaaaaaa"),
        "a failed projection describes no pixels, so it supplies no hash"
    );
    assert_eq!(
        metadata.screenshot_files,
        [PathBuf::from(
            "screenshots/current/01-healthy-center-ne.png"
        )],
        "the retained frames are still the ones the manifest names"
    );
}

#[test]
fn retained_last_green_metadata_is_reconciled_with_the_assembled_tree() {
    let previous_index = index_linking(&[OLD_HISTORY, CURRENT_FRAME]);
    let previous = previous_with_evidence("reconcile-prior", &previous_index);
    let current_index = index_linking(&[OLD_HISTORY, NEW_HISTORY, CURRENT_FRAME]);
    let current = fixture_site(
        "reconcile-current",
        &[
            ("index.html", current_index.as_str()),
            ("gallery.json", TWO_POINT_GALLERY),
            ("verification.json", GREEN_PROJECTION),
            (CURRENT_FRAME, "new current"),
            ("screenshots/current/worker-crop.png", "new crop"),
            (NEW_HISTORY, "new history"),
        ],
    );
    let output = TempDirectory::new("reconcile-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &status_only_workflow(),
        output.path(),
    )
    .unwrap();

    let metadata: LastGreenManifest =
        serde_json::from_str(&fs::read_to_string(output.path().join("last-green.json")).unwrap())
            .unwrap();

    assert_eq!(
        metadata.source_commit, "2222222222222222222222222222222222222222",
        "the retained manifest still describes the retained game"
    );
    assert_eq!(
        metadata.game_files,
        ["play/game_bg.wasm", "play/index.html"].map(PathBuf::from)
    );
    assert_eq!(
        metadata.screenshot_files,
        [
            "screenshots/current/01-healthy-center-ne.png",
            "screenshots/current/worker-crop.png",
        ]
        .map(PathBuf::from),
        "the manifest must enumerate the pixels the assembled tree actually holds"
    );
    assert_eq!(
        metadata.semantic_visual_hash.as_deref(),
        Some("bbbbbbbb"),
        "the promoted evidence supplies the hash of the promoted frames"
    );
}

// ---------------------------------------------------------------------------
// Inherited screenshot history
// ---------------------------------------------------------------------------

/// The visual history lives only on the generated branch, so the gallery a
/// build renders is the one its own predecessor published. A build handed a
/// history that no previous publication supplies would publish a manifest
/// vouching for images nobody can open.
#[test]
fn a_first_run_that_inherited_a_history_it_cannot_supply_is_refused() {
    let current = fixture_site(
        "inherited-history-first-run",
        &[
            ("index.html", &index_linking(&[OLD_HISTORY])),
            ("gallery.json", PRIOR_GALLERY),
            ("verification.json", GREEN_PROJECTION),
            (CURRENT_FRAME, "current frame"),
        ],
    );
    let output = TempDirectory::new("inherited-history-first-run-output");

    let error = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        output.path(),
    )
    .expect_err("a first run cannot supply a history it never published");

    match &error {
        SitegenError::MissingRetainedHistory { targets } => {
            assert_eq!(targets, &vec![OLD_HISTORY.to_owned()]);
        }
        other => panic!("expected a missing-history failure, got {other}"),
    }
    assert!(
        error.to_string().contains(OLD_HISTORY),
        "the failure should name the image nobody supplied: {error}"
    );
}

/// The same rule holds after the first run: a previous publication that lost
/// the images its own manifest names is a broken history, not a silent one.
#[test]
fn a_later_run_whose_predecessor_lost_its_history_is_refused() {
    let previous = fixture_site(
        "previous-without-history",
        &[
            ("index.html", "PREVIOUS SOURCE: GREEN"),
            ("gallery.json", PRIOR_GALLERY),
        ],
    );
    let current = fixture_site(
        "inherited-history-later-run",
        &[
            ("index.html", &index_linking(&[OLD_HISTORY])),
            ("gallery.json", PRIOR_GALLERY),
            ("verification.json", GREEN_PROJECTION),
            (CURRENT_FRAME, "current frame"),
        ],
    );
    let output = TempDirectory::new("inherited-history-later-run-output");

    let error = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        output.path(),
    )
    .expect_err("a history whose images are gone cannot be published");

    assert!(
        matches!(error, SitegenError::MissingRetainedHistory { .. }),
        "{error}"
    );
}

/// The same current tree publishes cleanly the moment its predecessor really
/// carries the images, so the rule catches missing history and nothing else.
#[test]
fn an_inherited_history_its_predecessor_supplies_publishes_normally() {
    let previous = fixture_site(
        "previous-with-history",
        &[
            ("index.html", "PREVIOUS SOURCE: GREEN"),
            ("gallery.json", PRIOR_GALLERY),
            (OLD_HISTORY, "old history"),
        ],
    );
    let current = fixture_site(
        "inherited-history-supplied",
        &[
            ("index.html", &index_linking(&[OLD_HISTORY])),
            ("gallery.json", PRIOR_GALLERY),
            ("verification.json", GREEN_PROJECTION),
            (CURRENT_FRAME, "current frame"),
        ],
    );
    let output = TempDirectory::new("inherited-history-supplied-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        output.path(),
    )
    .expect("a supplied history publishes");

    assert_eq!(disposition, BuildDisposition::RetainLastGreen);
    assert!(output.path().join(OLD_HISTORY).is_file());
}

// ---------------------------------------------------------------------------
// The gate runner
// ---------------------------------------------------------------------------

/// `scripts/run-gate.sh` is the only thing that decides what a job measured,
/// so what it records has to be exactly what the site can publish and nothing
/// a runner could smuggle through it.
mod gate_runner {
    use super::*;
    use midcreek_cs_1::sitegen::{GATE_RESULTS_FILE, GateStatus, gate_verdict, read_gate_records};

    #[test]
    fn a_failed_gate_is_recorded_and_still_lets_the_next_gate_run() {
        let root = TempDirectory::new("gate-runner-records");
        let results = root.path().join(GATE_RESULTS_FILE);

        let first = run_gate(&results, "Clippy lints", &["false"]);
        let second = run_gate(&results, "Release build", &["true"]);

        assert_eq!(
            first.status.code(),
            Some(0),
            "the runner never fails a step"
        );
        assert_eq!(second.status.code(), Some(0));

        let gates = read_gate_records(&fs::read_to_string(&results).unwrap()).unwrap();
        assert_eq!(
            gates
                .iter()
                .map(|gate| gate.name.as_str())
                .collect::<Vec<_>>(),
            ["Clippy lints", "Release build"]
        );
        assert_eq!(gates[0].status, GateStatus::Failed);
        assert_eq!(gates[0].failed, 1);
        assert_eq!(gates[1].status, GateStatus::Passed);
        assert_eq!(gates[1].passed, 1);
        assert_eq!(gate_verdict(&gates), GateStatus::Failed);
    }

    #[test]
    fn the_recorded_duration_is_the_gate_it_really_measured() {
        let root = TempDirectory::new("gate-runner-duration");
        let results = root.path().join(GATE_RESULTS_FILE);

        run_gate(&results, "Application contracts", &["sleep", "0.4"]);

        let gates = read_gate_records(&fs::read_to_string(&results).unwrap()).unwrap();
        assert!(
            gates[0].duration_ms >= 300,
            "a 400 ms gate should not be recorded as {} ms",
            gates[0].duration_ms
        );
        assert!(gates[0].duration_ms < 60_000, "{:?}", gates[0]);
    }

    /// The name is published verbatim inside JSON and then inside HTML, so the
    /// runner refuses anything that is not a plain label rather than trying to
    /// escape it on the way out.
    #[test]
    fn a_gate_name_that_could_break_out_of_its_record_is_refused() {
        let root = TempDirectory::new("gate-runner-name");
        let results = root.path().join(GATE_RESULTS_FILE);

        for name in [
            r#"Clippy","status":"passed"#,
            "<script>alert(1)</script>",
            "../../etc/passwd",
            "",
        ] {
            let attempt = run_gate(&results, name, &["true"]);
            assert_eq!(
                attempt.status.code(),
                Some(2),
                "{name:?} should be refused as a gate name"
            );
        }
        assert!(!results.exists(), "a refused gate records nothing");
    }

    #[test]
    fn a_job_that_measured_nothing_never_reports_success_by_omission() {
        assert_eq!(gate_verdict(&[]), GateStatus::Failed);
        assert!(read_gate_records("").unwrap().is_empty());
        assert!(read_gate_records("{\"name\":\"A\"}").is_err());
    }

    fn run_gate(results: &Path, name: &str, command: &[&str]) -> std::process::Output {
        Command::new("bash")
            .arg(repository().join("scripts/run-gate.sh"))
            .arg(results)
            .arg(name)
            .arg("--")
            .args(command)
            .output()
            .expect("the gate runner should launch")
    }

    fn repository() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }
}

// ---------------------------------------------------------------------------
// Publication inputs, for every upstream outcome
// ---------------------------------------------------------------------------

/// Publish always runs, so every combination of upstream outcomes has to end
/// in one publishable decision. These drive the real `sitegen inputs` command
/// exactly as the workflow does.
mod publication_inputs {
    use super::*;
    use midcreek_cs_1::sitegen::{
        GalleryManifest, GateStatus, GateSummary, JobResult, WorkflowSummary,
    };
    use serde_json::Value;

    const COMMIT: &str = "1111111111111111111111111111111111111111";
    const RUN_URL: &str = "https://github.com/ridermw/midcreek-cs-1/actions/runs/9";

    #[test]
    fn a_green_run_publishes_both_gate_sets_the_evidence_and_the_playable_game() {
        let run = Run::new("inputs-green");
        run.native(
            GateStatus::Passed,
            &[("Clippy lints", GateStatus::Passed)],
            true,
        );
        run.web(
            GateStatus::Passed,
            &[("Headless browser gate", GateStatus::Passed)],
            true,
        );
        run.previous_gallery(PRIOR_GALLERY);

        let (workflow, inputs) = run.execute("success", "success");

        assert_eq!(workflow.native, GateStatus::Passed);
        assert_eq!(workflow.web, GateStatus::Passed);
        assert_eq!(
            gate_names(&workflow),
            ["Clippy lints", "Headless browser gate"]
        );
        assert!(inputs["verification"]["report"].is_string(), "{inputs}");
        assert!(
            inputs["verification"]["browser"]["report"].is_string(),
            "{inputs}"
        );
        assert_eq!(
            inputs["playable"]["directory"].as_str().unwrap(),
            run.web_root().join("play").to_str().unwrap()
        );
        assert_eq!(
            inputs["playable"]["source_commit"].as_str().unwrap(),
            COMMIT
        );
        assert_eq!(run.gallery().entries.len(), 1);
    }

    /// A failed native job publishes its current failure and no evidence: the
    /// pixels a failing run happened to leave behind are never promoted.
    #[test]
    fn a_failed_native_job_publishes_its_gates_and_no_evidence() {
        let run = Run::new("inputs-native-failed");
        run.native(
            GateStatus::Failed,
            &[
                ("Clippy lints", GateStatus::Passed),
                ("Rendered image contracts", GateStatus::Failed),
            ],
            true,
        );

        let (workflow, inputs) = run.execute("failure", "skipped");

        assert_eq!(workflow.native, GateStatus::Failed);
        assert_eq!(workflow.web, GateStatus::SkippedDependency);
        assert!(inputs["verification"].is_null(), "{inputs}");
        assert!(inputs["playable"].is_null(), "{inputs}");
        assert_eq!(
            gate_names(&workflow),
            [
                "Clippy lints",
                "Rendered image contracts",
                "Web package and browser gate",
            ]
        );
        assert_eq!(
            gate(&workflow, "Rendered image contracts").status,
            GateStatus::Failed
        );
        // A skipped job is published as a skipped dependency, never as a job
        // whose result manifest went missing.
        assert_eq!(
            gate(&workflow, "Web package and browser gate").status,
            GateStatus::SkippedDependency
        );
        assert!(
            !workflow
                .gates
                .iter()
                .any(|gate| gate.name.contains("result manifest"))
        );
    }

    /// A render gate that failed after writing part of its report leaves an
    /// evidence directory behind. The failed job may not publish it.
    #[test]
    fn a_partial_report_from_a_failed_render_gate_is_never_published() {
        let run = Run::new("inputs-partial-report");
        run.native(
            GateStatus::Failed,
            &[("Rendered image contracts", GateStatus::Failed)],
            true,
        );
        fs::remove_file(run.native_root().join("verification/03-walk-ne.png")).unwrap();

        let (workflow, inputs) = run.execute("failure", "skipped");

        assert_eq!(workflow.native, GateStatus::Failed);
        assert!(inputs["verification"].is_null(), "{inputs}");
    }

    /// A job may declare evidence and still leave a directory the generator
    /// cannot project. Publish degrades to a status-only page and says so.
    #[test]
    fn evidence_that_no_longer_projects_becomes_a_published_failed_gate() {
        let run = Run::new("inputs-unprojectable");
        run.native(
            GateStatus::Passed,
            &[("Clippy lints", GateStatus::Passed)],
            true,
        );
        fs::remove_file(run.native_root().join("verification/03-walk-ne.png")).unwrap();

        let (workflow, inputs) = run.execute("success", "skipped");

        assert_eq!(workflow.native, GateStatus::Passed);
        assert!(inputs["verification"].is_null(), "{inputs}");
        assert_eq!(
            gate(&workflow, "Published verification evidence").status,
            GateStatus::Failed
        );
    }

    /// A failed web job keeps the native evidence that really passed, but
    /// never contributes its own browser proof or its package.
    #[test]
    fn a_failed_web_job_keeps_the_native_evidence_and_promotes_no_game() {
        let run = Run::new("inputs-web-failed");
        run.native(
            GateStatus::Passed,
            &[("Clippy lints", GateStatus::Passed)],
            true,
        );
        run.web(
            GateStatus::Failed,
            &[("Headless browser gate", GateStatus::Failed)],
            true,
        );

        let (workflow, inputs) = run.execute("success", "failure");

        assert_eq!(workflow.native, GateStatus::Passed);
        assert_eq!(workflow.web, GateStatus::Failed);
        assert!(inputs["verification"]["report"].is_string(), "{inputs}");
        assert!(inputs["verification"]["browser"].is_null(), "{inputs}");
        assert!(inputs["playable"].is_null(), "{inputs}");
    }

    /// A job that passed every gate it measured and then failed anyway is
    /// published as a failure, and the extra row says where it failed.
    #[test]
    fn a_job_that_failed_after_its_last_gate_is_still_published_as_failed() {
        let run = Run::new("inputs-late-failure");
        run.native(
            GateStatus::Passed,
            &[("Clippy lints", GateStatus::Passed)],
            true,
        );

        let (workflow, inputs) = run.execute("failure", "skipped");

        assert_eq!(workflow.native, GateStatus::Failed);
        assert!(inputs["verification"].is_null(), "{inputs}");
        assert_eq!(
            gate(&workflow, "Native verification").status,
            GateStatus::Failed
        );
    }

    /// A job that never uploaded a manifest publishes both the job it was and
    /// the manifest that never arrived, so a silent upload failure is visible.
    #[test]
    fn a_missing_result_artifact_publishes_the_gap_it_left() {
        let run = Run::new("inputs-missing-artifact");

        let (workflow, inputs) = run.execute("failure", "skipped");

        assert_eq!(workflow.native, GateStatus::Failed);
        assert_eq!(workflow.web, GateStatus::SkippedDependency);
        assert_eq!(
            gate_names(&workflow),
            [
                "Native verification",
                "Native verification result manifest",
                "Web package and browser gate",
            ]
        );
        assert!(inputs["verification"].is_null(), "{inputs}");
        assert!(inputs["playable"].is_null(), "{inputs}");
    }

    /// A manifest a runner could have forged is read exactly like one that
    /// never arrived, so it can never publish itself.
    #[test]
    fn a_result_manifest_carrying_an_untrusted_value_is_read_as_absent() {
        let run = Run::new("inputs-untrusted-manifest");
        fs::create_dir_all(run.native_root()).unwrap();
        fs::write(
            run.native_root().join("result.json"),
            r#"{"job":"verify","status":"passed","evidence":null,"gates":[{
                "name":"Clippy lints","status":"passed","passed":1,"failed":0,
                "duration_ms":10,"artifact_url":"https://example.invalid/steal"}]}"#,
        )
        .unwrap();

        let (workflow, _) = run.execute("success", "skipped");

        assert_eq!(workflow.native, GateStatus::Failed);
        assert!(
            !workflow
                .gates
                .iter()
                .any(|gate| gate.name == "Clippy lints")
        );
        assert_eq!(
            gate(&workflow, "Native verification result manifest").status,
            GateStatus::Failed
        );
    }

    /// A first run has no predecessor, so it starts from an explicitly empty
    /// history rather than from no history input at all.
    #[test]
    fn a_first_run_passes_an_empty_inherited_history() {
        let run = Run::new("inputs-first-run");
        run.native(
            GateStatus::Passed,
            &[("Clippy lints", GateStatus::Passed)],
            true,
        );

        let (_, inputs) = run.execute("success", "skipped");

        assert_eq!(inputs["gallery"].as_str().unwrap(), "gallery.json");
        assert_eq!(run.gallery(), GalleryManifest::default());
    }

    /// A later run inherits exactly what its predecessor published.
    #[test]
    fn a_later_run_inherits_the_history_its_predecessor_published() {
        let run = Run::new("inputs-prior-gallery");
        run.native(
            GateStatus::Passed,
            &[("Clippy lints", GateStatus::Passed)],
            true,
        );
        run.previous_gallery(PRIOR_GALLERY);

        let (_, inputs) = run.execute("success", "skipped");

        assert_eq!(inputs["gallery"].as_str().unwrap(), "gallery.json");
        assert_eq!(
            run.gallery(),
            serde_json::from_str::<GalleryManifest>(PRIOR_GALLERY).unwrap()
        );
    }

    /// A package that lost a file it needs is never promoted, and the gap is
    /// published rather than discovered by a visitor.
    #[test]
    fn an_incomplete_package_is_refused_and_published_as_a_failed_gate() {
        let run = Run::new("inputs-incomplete-package");
        run.native(
            GateStatus::Passed,
            &[("Clippy lints", GateStatus::Passed)],
            true,
        );
        run.web(
            GateStatus::Passed,
            &[("Headless browser gate", GateStatus::Passed)],
            true,
        );
        fs::remove_file(run.web_root().join("play/game_bg.wasm")).unwrap();

        let (workflow, inputs) = run.execute("success", "success");

        assert_eq!(workflow.web, GateStatus::Passed);
        assert!(inputs["playable"].is_null(), "{inputs}");
        assert_eq!(
            gate(&workflow, "Published playable package").status,
            GateStatus::Failed
        );
    }

    /// The commit timeline the site renders comes from the repository itself,
    /// never from anything a runner wrote.
    #[test]
    fn the_published_repository_facts_come_from_the_checkout() {
        let run = Run::new("inputs-repo-facts");
        run.native(
            GateStatus::Passed,
            &[("Clippy lints", GateStatus::Passed)],
            true,
        );
        run.execute("success", "skipped");

        let repo = read_value(&run.output().join("repo.json"));
        let commits = repo["commits"].as_array().unwrap();

        assert_eq!(repo["head_sha"].as_str().unwrap().len(), 40);
        assert!(!commits.is_empty() && commits.len() <= 20, "{repo}");
        for commit in commits {
            assert_eq!(commit["sha"].as_str().unwrap().len(), 40);
            assert!(commit["committed_at"].as_str().unwrap().contains('T'));
            assert!(commit["subject"].is_string());
            assert!(commit["task_id"].is_null());
        }
    }

    // -------------------------------------------------------------------
    // One `sitegen inputs` run and the artifacts it was handed
    // -------------------------------------------------------------------

    struct Run {
        root: TempDirectory,
        previous: std::cell::Cell<bool>,
    }

    impl Run {
        fn new(name: &str) -> Self {
            Self {
                root: TempDirectory::new(name),
                previous: std::cell::Cell::new(false),
            }
        }

        fn native_root(&self) -> PathBuf {
            self.root.path().join("native")
        }

        fn web_root(&self) -> PathBuf {
            self.root.path().join("web")
        }

        fn output(&self) -> PathBuf {
            self.root.path().join("pages")
        }

        /// Writes the artifact Verify would have uploaded.
        fn native(&self, status: GateStatus, gates: &[(&str, GateStatus)], evidence: bool) {
            let root = self.native_root();
            if evidence {
                copy_fixture(
                    "verification",
                    &root.join("verification"),
                    &["failed-report.json"],
                );
            }
            write_result(
                &root,
                "verify",
                status,
                gates,
                evidence.then_some("verification"),
            );
        }

        /// Writes the artifact Build web would have uploaded.
        fn web(&self, status: GateStatus, gates: &[(&str, GateStatus)], evidence: bool) {
            let root = self.web_root();
            if evidence {
                copy_fixture("browser", &root.join("browser"), &[]);
                write_package(&root.join("play"));
            }
            write_result(
                &root,
                "build-web",
                status,
                gates,
                evidence.then_some("browser"),
            );
        }

        /// Writes the history a previous `pages-live` publication left.
        fn previous_gallery(&self, gallery: &str) {
            let root = self.root.path().join("previous");
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("gallery.json"), gallery).unwrap();
            self.previous.set(true);
        }

        fn gallery(&self) -> GalleryManifest {
            serde_json::from_str(&fs::read_to_string(self.output().join("gallery.json")).unwrap())
                .unwrap()
        }

        fn execute(&self, native_outcome: &str, web_outcome: &str) -> (WorkflowSummary, Value) {
            let output = self.output();
            let mut command = Command::new(env!("CARGO_BIN_EXE_sitegen"));
            command.args([
                "inputs",
                "--repository",
                env!("CARGO_MANIFEST_DIR"),
                "--source-commit",
                COMMIT,
                "--run-url",
                RUN_URL,
                "--native-outcome",
                native_outcome,
                "--web-outcome",
                web_outcome,
                "--output",
                output.to_str().unwrap(),
            ]);
            if self.native_root().is_dir() {
                command.args(["--native", self.native_root().to_str().unwrap()]);
            }
            if self.web_root().is_dir() {
                command.args(["--web", self.web_root().to_str().unwrap()]);
            }
            if self.previous.get() {
                command.args([
                    "--previous",
                    self.root.path().join("previous").to_str().unwrap(),
                ]);
            }

            let finished = command.output().expect("sitegen should launch");
            assert_eq!(
                finished.status.code(),
                Some(0),
                "sitegen inputs should always publish: {}",
                String::from_utf8_lossy(&finished.stderr)
            );

            let workflow = serde_json::from_str::<WorkflowSummary>(
                &fs::read_to_string(output.join("workflow.json")).unwrap(),
            )
            .expect("the merged result should match the published schema");
            (workflow, read_value(&output.join("inputs.json")))
        }
    }

    fn write_result(
        directory: &Path,
        job: &str,
        status: GateStatus,
        gates: &[(&str, GateStatus)],
        evidence: Option<&str>,
    ) {
        fs::create_dir_all(directory).unwrap();
        let manifest = JobResult {
            job: job.to_owned(),
            status,
            gates: gates
                .iter()
                .map(|(name, status)| GateSummary {
                    name: (*name).to_owned(),
                    status: *status,
                    passed: u32::from(*status == GateStatus::Passed),
                    failed: u32::from(*status == GateStatus::Failed),
                    duration_ms: 1_500,
                    artifact_url: None,
                })
                .collect(),
            evidence: evidence.map(str::to_owned),
        };
        fs::write(
            directory.join("result.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    /// Copies one committed fixture directory into an artifact root.
    fn copy_fixture(name: &str, destination: &Path, skip: &[&str]) {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sitegen")
            .join(name);
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(&source).unwrap() {
            let entry = entry.unwrap();
            let file_name = entry.file_name();
            if skip.iter().any(|skipped| *skipped == file_name) {
                continue;
            }
            fs::copy(entry.path(), destination.join(file_name)).unwrap();
        }
    }

    fn write_package(directory: &Path) {
        fs::create_dir_all(directory.join("assets")).unwrap();
        for file in [
            "index.html",
            "play.js",
            "play.css",
            "game.js",
            "game_bg.wasm",
        ] {
            fs::write(directory.join(file), file).unwrap();
        }
        fs::write(directory.join("assets/hall.glb"), "hall").unwrap();
    }

    fn read_value(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn gate_names(workflow: &WorkflowSummary) -> Vec<&str> {
        workflow
            .gates
            .iter()
            .map(|gate| gate.name.as_str())
            .collect()
    }

    fn gate<'summary>(workflow: &'summary WorkflowSummary, name: &str) -> &'summary GateSummary {
        workflow
            .gates
            .iter()
            .find(|gate| gate.name == name)
            .unwrap_or_else(|| panic!("expected a {name:?} gate in {:?}", gate_names(workflow)))
    }
}

// ---------------------------------------------------------------------------
// What a result manifest may carry
// ---------------------------------------------------------------------------

/// A result manifest is written on a runner and read by the thing that
/// generates a public page, so it is the one place a command line, a captured
/// stream, an environment value, a local path, or somebody else's URL could
/// cross into the site. None of them may.
mod result_manifest_safety {
    use midcreek_cs_1::sitegen::{
        GateStatus, GateSummary, JobOutcome, JobReport, JobResult, WorkflowSummary,
        merge_job_results, validate_job_result, validate_workflow_summary,
    };

    #[test]
    fn a_manifest_that_grew_a_field_is_refused_instead_of_read() {
        for extra in [
            r#""command":"cargo test""#,
            r#""stdout":"thread panicked at src/world.rs:12""#,
            r#""environment":{"GITHUB_TOKEN":"ghp_x"}"#,
        ] {
            let json = format!(
                r#"{{"job":"verify","status":"passed","gates":[],"evidence":null,{extra}}}"#
            );
            assert!(
                serde_json::from_str::<JobResult>(&json).is_err(),
                "a manifest carrying {extra} must not parse"
            );
        }
    }

    #[test]
    fn a_job_name_this_workflow_never_declared_is_refused() {
        assert!(validate_job_result(&manifest("smuggle", None, &[])).is_err());
        assert!(validate_job_result(&manifest("verify", None, &[])).is_ok());
        assert!(validate_job_result(&manifest("build-web", None, &[])).is_ok());
    }

    #[test]
    fn an_evidence_directory_that_leaves_its_artifact_is_refused() {
        for evidence in [
            "../../etc",
            "/etc/passwd",
            "verification/../..",
            "",
            r"verification\..",
        ] {
            assert!(
                validate_job_result(&manifest("verify", Some(evidence), &[])).is_err(),
                "{evidence:?} must not be accepted as an evidence directory"
            );
        }
        assert!(validate_job_result(&manifest("verify", Some("verification"), &[])).is_ok());
    }

    #[test]
    fn a_gate_name_carrying_a_path_or_a_stream_is_refused() {
        for name in [
            "/Users/someone/checkout",
            "target/render-contract/primary",
            "thread 'x' panicked\nat src/world.rs",
            "",
        ] {
            assert!(
                validate_job_result(&manifest("verify", None, &[(name, None)])).is_err(),
                "{name:?} must not be published as a gate name"
            );
        }
    }

    #[test]
    fn a_link_that_leaves_this_repositorys_host_is_refused() {
        for url in [
            "https://example.invalid/steal",
            "javascript:alert(1)",
            "http://github.com/ridermw/midcreek-cs-1",
            "file:///etc/passwd",
            "https://github.com/",
        ] {
            assert!(
                validate_job_result(&manifest("verify", None, &[("Clippy lints", Some(url))]))
                    .is_err(),
                "{url:?} must not be published as an artifact link"
            );
            let mut summary = workflow_summary(GateStatus::Passed, GateStatus::Passed);
            summary.run_url = url.to_owned();
            assert!(
                validate_workflow_summary(&summary).is_err(),
                "{url:?} must not be published as a run link"
            );
        }
    }

    #[test]
    fn a_source_commit_that_is_not_a_full_sha_is_refused() {
        for commit in ["HEAD", "1111111", "", "../../refs/heads/main"] {
            let mut summary = workflow_summary(GateStatus::Passed, GateStatus::Passed);
            summary.source_commit = commit.to_owned();
            assert!(
                validate_workflow_summary(&summary).is_err(),
                "{commit:?} must not be published as a source commit"
            );
        }
    }

    /// Merging is the last chance to refuse an unsafe value, so it validates
    /// rather than trusting whatever the jobs uploaded.
    #[test]
    fn merging_refuses_an_unsafe_value_rather_than_publishing_it() {
        let native = JobReport {
            outcome: JobOutcome::Success,
            result: Some(manifest(
                "verify",
                None,
                &[("Clippy lints", Some("https://example.invalid/steal"))],
            )),
        };
        let web = JobReport::absent(JobOutcome::Skipped);

        let merged = merge_job_results(
            "1111111111111111111111111111111111111111",
            "https://github.com/ridermw/midcreek-cs-1/actions/runs/1",
            &native,
            &web,
        );

        assert!(merged.is_err(), "{merged:?}");
    }

    /// An unreviewed or empty job outcome is read as a failure, because
    /// publishing an outcome nobody reviewed as a success is the one mistake
    /// this workflow may not make.
    #[test]
    fn an_unknown_job_outcome_is_read_as_a_failure() {
        for value in ["", "neutral", "SUCCESS", "action_required"] {
            assert_eq!(JobOutcome::parse(value), JobOutcome::Failure, "{value:?}");
        }
        assert_eq!(JobOutcome::parse("success"), JobOutcome::Success);
        assert_eq!(JobOutcome::parse("skipped"), JobOutcome::Skipped);
        assert_eq!(JobOutcome::parse("cancelled"), JobOutcome::Cancelled);
        assert_eq!(JobOutcome::Cancelled.status(), GateStatus::Failed);
    }

    /// The published status only ever agrees with success. Either the job or
    /// its own manifest saying otherwise publishes a failure.
    #[test]
    fn a_job_and_its_manifest_both_have_to_agree_on_success() {
        let passed = manifest("verify", Some("verification"), &[]);
        let failed = JobResult {
            status: GateStatus::Failed,
            ..passed.clone()
        };

        let green = JobReport {
            outcome: JobOutcome::Success,
            result: Some(passed.clone()),
        };
        assert_eq!(green.status(), GateStatus::Passed);
        assert_eq!(green.evidence(), Some("verification"));

        for report in [
            JobReport {
                outcome: JobOutcome::Failure,
                result: Some(passed),
            },
            JobReport {
                outcome: JobOutcome::Success,
                result: Some(failed),
            },
            JobReport::absent(JobOutcome::Success),
        ] {
            assert_eq!(report.status(), GateStatus::Failed, "{report:?}");
            assert_eq!(report.evidence(), None, "{report:?}");
        }
    }

    fn manifest(job: &str, evidence: Option<&str>, gates: &[(&str, Option<&str>)]) -> JobResult {
        JobResult {
            job: job.to_owned(),
            status: GateStatus::Passed,
            gates: gates
                .iter()
                .map(|(name, artifact_url)| GateSummary {
                    name: (*name).to_owned(),
                    status: GateStatus::Passed,
                    passed: 1,
                    failed: 0,
                    duration_ms: 10,
                    artifact_url: artifact_url.map(str::to_owned),
                })
                .collect(),
            evidence: evidence.map(str::to_owned),
        }
    }

    fn workflow_summary(native: GateStatus, web: GateStatus) -> WorkflowSummary {
        super::workflow_summary(native, web)
    }
}
