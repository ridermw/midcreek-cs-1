mod support;

use midcreek_cs_1::sitegen::{
    Challenge, ChallengeStatus, ProgressError, ProgressStatus, ReferenceError, ReferenceManifest,
    plan_task_ids_from_markdown, resolve_commit_ref, validate_progress,
    validate_reference_manifest,
};
use std::{
    fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use support::{fixture, plan_ids, read, repo_facts, sha256, validate_fixture};

mod progress_contract {
    use super::*;

    #[test]
    fn accepts_one_dependency_ready_current_task() {
        let document = fixture("green-progress.json");
        assert!(validate_progress(&document, &plan_ids(), &repo_facts()).is_ok());
    }

    mod validate_cli {
        use super::*;

        #[test]
        fn extracts_every_progress_task_from_the_reviewed_plan() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let markdown = read(root.join("docs/implementation-plan.md"));

            assert_eq!(
                plan_task_ids_from_markdown(&markdown),
                [
                    "foundation-contracts",
                    "pages-foundation",
                    "autonomous-assets",
                    "data-hall",
                    "technician-movement",
                    "camera-orbit",
                    "operations-loop",
                    "operations-hud",
                    "pages-playable",
                    "autonomous-verification",
                    "pages-verification",
                    "pages-status-always",
                    "ci-baseline",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect()
            );
        }

        #[test]
        fn invalid_arguments_exit_with_code_two() {
            let output = Command::new(env!("CARGO_BIN_EXE_sitegen"))
                .output()
                .expect("sitegen should launch");

            assert_eq!(output.status.code(), Some(2));
        }

        #[test]
        fn invalid_content_exits_with_code_one_and_prints_each_error() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let output = Command::new(env!("CARGO_BIN_EXE_sitegen"))
                .current_dir(root)
                .args([
                    "validate",
                    "--progress",
                    "tests/fixtures/sitegen/two-current.json",
                    "--plan",
                    "docs/implementation-plan.md",
                    "--repository",
                    ".",
                ])
                .output()
                .expect("sitegen should launch");

            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
            assert_eq!(stderr.lines().count(), 2);
            assert!(stderr.contains(
                "task pages-foundation started before dependency foundation-contracts was done"
            ));
            assert!(stderr.contains("multiple tasks are in progress"));
        }

        #[test]
        fn valid_content_prints_the_current_task_and_exits_zero() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let output = Command::new(env!("CARGO_BIN_EXE_sitegen"))
                .current_dir(root)
                .args([
                    "validate",
                    "--progress",
                    "docs/progress.json",
                    "--plan",
                    "docs/implementation-plan.md",
                    "--repository",
                    ".",
                ])
                .output()
                .expect("sitegen should launch");

            assert_eq!(output.status.code(), Some(0));
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                "pages-foundation\n"
            );
            assert!(output.stderr.is_empty());
        }
    }

    mod publication_inputs {
        use super::*;

        #[test]
        fn canonical_progress_matches_the_approved_ordered_task_graph() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let document: midcreek_cs_1::sitegen::ProgressDocument =
                serde_json::from_str(&read(root.join("docs/progress.json")))
                    .expect("canonical progress should match its strict schema");
            let expected = [
                (
                    "foundation-contracts",
                    ProgressStatus::Done,
                    &[][..],
                    "Pinned the Bevy project and established reviewed contracts.",
                    Some("df0da32ab61a6cb6901bfd4b3cdbbcaec75bccc2"),
                ),
                (
                    "pages-foundation",
                    ProgressStatus::InProgress,
                    &["foundation-contracts"][..],
                    "Building the canonical progress model and status-only Pages site.",
                    None,
                ),
                (
                    "autonomous-assets",
                    ProgressStatus::Future,
                    &["pages-foundation"][..],
                    "Generate autonomous rigged and modular game assets after the status hub is live.",
                    None,
                ),
                (
                    "data-hall",
                    ProgressStatus::Future,
                    &["autonomous-assets"][..],
                    "Build the authored cel-shift data hall.",
                    None,
                ),
                (
                    "technician-movement",
                    ProgressStatus::Future,
                    &["data-hall"][..],
                    "Add rigged camera-relative technician movement.",
                    None,
                ),
                (
                    "camera-orbit",
                    ProgressStatus::Future,
                    &["technician-movement"][..],
                    "Add clamped Q/E four-way camera orbit.",
                    None,
                ),
                (
                    "operations-loop",
                    ProgressStatus::Future,
                    &["data-hall", "technician-movement"][..],
                    "Add recurring prioritized faults, tickets, and repair.",
                    None,
                ),
                (
                    "operations-hud",
                    ProgressStatus::Future,
                    &["camera-orbit", "operations-loop"][..],
                    "Add ticket HUD, controls, and rack badges.",
                    None,
                ),
                (
                    "pages-playable",
                    ProgressStatus::Future,
                    &["operations-hud"][..],
                    "Publish the playable WASM game.",
                    None,
                ),
                (
                    "autonomous-verification",
                    ProgressStatus::Future,
                    &["operations-hud"][..],
                    "Build deterministic gameplay and render verification.",
                    None,
                ),
                (
                    "pages-verification",
                    ProgressStatus::Future,
                    &["pages-playable", "autonomous-verification"][..],
                    "Publish comparisons, screenshots, challenges, and test evidence.",
                    None,
                ),
                (
                    "pages-status-always",
                    ProgressStatus::Future,
                    &["pages-verification"][..],
                    "Retain the last green game while publishing current status.",
                    None,
                ),
                (
                    "ci-baseline",
                    ProgressStatus::Future,
                    &["pages-status-always"][..],
                    "Publish and verify the final POC baseline.",
                    None,
                ),
            ];

            assert_eq!(document.schema_version, 1);
            assert_eq!(document.project, "Cell Shift Data Center POC");
            assert!(document.challenges.is_empty());
            assert_eq!(document.tasks.len(), expected.len());
            for (task, (id, status, dependencies, summary, commit)) in
                document.tasks.iter().zip(expected)
            {
                assert_eq!(task.id, id);
                assert_eq!(task.status, status);
                assert_eq!(task.depends_on, dependencies);
                assert_eq!(task.summary, summary);
                assert_eq!(task.completed_commit.as_deref(), commit);
            }
        }

        #[test]
        fn published_plan_matches_the_approved_master_plan() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            assert_eq!(
                sha256(root.join("docs/implementation-plan.md")),
                "06e367472db7cb960268c90fd6adf38e0b320cb4e2c8873c1a2f1b9320a8db2b"
            );
        }

        #[test]
        fn reference_manifest_matches_the_approved_pngs() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let manifest: ReferenceManifest =
                serde_json::from_str(&read(root.join("docs/reference/manifest.json")))
                    .expect("reference manifest should match its strict schema");
            let expected = [
                (
                    "Cel Shift key art",
                    "../midcreek-concept/themes/cel-shift/masters/key-art/04-diamond-bright.png",
                    "docs/reference/cel-shift-key-art.png",
                    "a30e12b63a36743015b1c73eeca6248a8b8ee974cf007f23666dc101f06c0e75",
                ),
                (
                    "Cel Shift character sheet",
                    "../midcreek-concept/themes/cel-shift/masters/animation/01-model-sheet.png",
                    "docs/reference/cel-shift-character-sheet.png",
                    "8a5a31e7bceb8ad16b3481d2bae89e7a32bb4edd0ef711b7d07a26f177cf6b25",
                ),
            ];

            assert_eq!(manifest.assets.len(), expected.len());
            for (asset, (name, source_path, public_path, expected_hash)) in
                manifest.assets.iter().zip(expected)
            {
                assert_eq!(asset.name, name);
                assert_eq!(asset.source_path, source_path);
                assert_eq!(asset.public_path, public_path);
                assert_eq!((asset.width, asset.height), (1536, 1024));
                assert_eq!(asset.sha256, expected_hash);
                assert_eq!(sha256(root.join(public_path)), expected_hash);
            }
        }

        #[test]
        fn reference_manifest_rejects_unknown_fields() {
            let json = r#"{
                "assets": [{
                    "name": "Cel Shift key art",
                    "source_path": "source.png",
                    "public_path": "public.png",
                    "sha256": "abc",
                    "width": 1,
                    "height": 1,
                    "extra": true
                }]
            }"#;

            assert!(serde_json::from_str::<ReferenceManifest>(json).is_err());
        }

        #[test]
        fn reference_validation_rejects_manifest_drift() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let mut manifest: ReferenceManifest =
                serde_json::from_str(&read(root.join("docs/reference/manifest.json"))).unwrap();
            manifest.assets[0].sha256 =
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".into();

            let errors = validate_reference_manifest(&manifest, root).unwrap_err();

            assert!(errors.iter().any(|error| matches!(
                error,
                ReferenceError::ManifestFieldMismatch { asset, field, .. }
                    if asset == "Cel Shift key art" && field == "sha256"
            )));
        }

        #[test]
        fn reference_validation_rejects_changed_destination_bytes() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let manifest: ReferenceManifest =
                serde_json::from_str(&read(root.join("docs/reference/manifest.json"))).unwrap();
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let temporary_root = std::env::temp_dir()
                .join(format!("midcreek-sitegen-{unique}-{}", std::process::id()));
            let reference_dir = temporary_root.join("docs/reference");
            fs::create_dir_all(&reference_dir).unwrap();
            for asset in &manifest.assets {
                fs::copy(
                    root.join(&asset.public_path),
                    temporary_root.join(&asset.public_path),
                )
                .unwrap();
            }
            fs::write(
                temporary_root.join("docs/reference/cel-shift-key-art.png"),
                b"changed",
            )
            .unwrap();

            let errors = validate_reference_manifest(&manifest, &temporary_root).unwrap_err();
            fs::remove_dir_all(&temporary_root).unwrap();

            assert!(errors.iter().any(|error| matches!(
                error,
                ReferenceError::AssetHashMismatch { path, .. }
                    if path == Path::new("docs/reference/cel-shift-key-art.png")
            )));
        }
    }

    #[test]
    fn rejects_two_current_tasks() {
        let errors = validate_fixture("two-current.json");
        assert!(errors.contains(&ProgressError::MultipleCurrentTasks));
    }

    #[test]
    fn rejects_done_task_without_commit() {
        let errors = validate_fixture("done-without-commit.json");
        assert!(errors.iter().any(|error| matches!(
            error,
            ProgressError::MissingCompletionCommit { task_id }
                if task_id == "pages-foundation"
        )));
    }

    #[test]
    fn resolves_head_to_the_workflow_commit() {
        let facts = repo_facts();
        let resolved = resolve_commit_ref("HEAD", &facts).unwrap();
        assert_eq!(resolved, facts.head_sha);
    }

    #[test]
    fn rejects_unknown_fields_in_every_progress_source_type() {
        let document_with_extra = r#"{
            "schema_version": 1,
            "project": "Cell Shift Data Center POC",
            "tasks": [],
            "challenges": [],
            "extra": true
        }"#;
        let task_with_extra = r#"{
            "schema_version": 1,
            "project": "Cell Shift Data Center POC",
            "tasks": [{
                "id": "foundation-contracts",
                "title": "Establish reviewed contracts",
                "status": "done",
                "depends_on": [],
                "summary": "Done.",
                "completed_commit": "1111111111111111111111111111111111111111",
                "extra": true
            }],
            "challenges": []
        }"#;
        let challenge_with_extra = r#"{
            "schema_version": 1,
            "project": "Cell Shift Data Center POC",
            "tasks": [],
            "challenges": [{
                "id": "render-variance",
                "title": "Render variance",
                "status": "open",
                "impact": "Hashes vary.",
                "approach": "Use bounded metrics.",
                "resolution": null,
                "resolved_commit": null,
                "extra": true
            }]
        }"#;

        for json in [document_with_extra, task_with_extra, challenge_with_extra] {
            assert!(
                serde_json::from_str::<midcreek_cs_1::sitegen::ProgressDocument>(json).is_err()
            );
        }
    }

    #[test]
    fn reports_task_errors_in_document_order() {
        let mut document = fixture("green-progress.json");
        document.tasks[0].completed_commit = None;
        document.tasks[1].status = ProgressStatus::Future;
        document.tasks[1].completed_commit =
            Some("2222222222222222222222222222222222222222".to_owned());

        let errors = validate_progress(&document, &plan_ids(), &repo_facts()).unwrap_err();
        let missing_index = errors
            .iter()
            .position(|error| {
                matches!(
                    error,
                    ProgressError::MissingCompletionCommit { task_id }
                        if task_id == "foundation-contracts"
                )
            })
            .unwrap();
        let unexpected_index = errors
            .iter()
            .position(|error| {
                matches!(
                    error,
                    ProgressError::UnexpectedCompletionCommit { task_id }
                        if task_id == "pages-foundation"
                )
            })
            .unwrap();

        assert!(missing_index < unexpected_index);
    }

    #[test]
    fn aggregates_missing_challenge_context_and_resolution() {
        let mut document = fixture("green-progress.json");
        document.challenges = vec![
            Challenge {
                id: "render-variance".to_owned(),
                title: "Render variance".to_owned(),
                status: ChallengeStatus::Open,
                impact: " ".to_owned(),
                approach: String::new(),
                resolution: None,
                resolved_commit: None,
            },
            Challenge {
                id: "browser-readiness".to_owned(),
                title: "Browser readiness".to_owned(),
                status: ChallengeStatus::Resolved,
                impact: "The game might not render.".to_owned(),
                approach: "Wait for the readiness signal.".to_owned(),
                resolution: Some(" ".to_owned()),
                resolved_commit: Some("3333333333333333333333333333333333333333".to_owned()),
            },
        ];

        assert_eq!(
            validate_progress(&document, &plan_ids(), &repo_facts()).unwrap_err(),
            [
                ProgressError::MissingChallengeContext {
                    challenge_id: "render-variance".to_owned(),
                    field: "impact".to_owned(),
                },
                ProgressError::MissingChallengeContext {
                    challenge_id: "render-variance".to_owned(),
                    field: "approach".to_owned(),
                },
                ProgressError::MissingChallengeResolution {
                    challenge_id: "browser-readiness".to_owned(),
                },
                ProgressError::MissingChallengeContext {
                    challenge_id: "browser-readiness".to_owned(),
                    field: "resolved_commit".to_owned(),
                },
            ]
        );
    }
}
