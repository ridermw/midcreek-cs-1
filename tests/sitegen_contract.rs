mod support;

use midcreek_cs_1::sitegen::{
    Challenge, ChallengeStatus, ProgressError, ProgressStatus, ReferenceError, ReferenceManifest,
    SitegenError, build_site, plan_task_ids_from_markdown, resolve_commit_ref,
    validate_output_path, validate_progress, validate_reference_manifest, validate_site_output,
};
use std::{
    fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use support::{
    assert_has_element_id, assert_text, build_fixture_site, build_site_from_inputs, fixture,
    plan_ids, read, repo_facts, sha256, site_inputs, validate_fixture,
};

mod generated_site_contract {
    use super::*;

    #[test]
    fn renders_every_required_section() {
        let output = build_fixture_site("green").unwrap();
        let html = output.index_html();

        for id in [
            "build-status",
            "play",
            "comparison",
            "progress",
            "screenshots",
            "plan",
            "challenges",
            "tests",
            "commits",
        ] {
            assert_has_element_id(&html, id);
        }
    }

    #[test]
    fn reports_that_no_verified_playable_build_exists() {
        let html = build_fixture_site("green").unwrap().index_html();

        assert_text(&html, "#play", "No verified playable build yet");
        assert!(!html.contains("<canvas"));
        assert!(!html.contains("<iframe"));
    }

    #[test]
    fn labels_the_current_source_when_verification_failed() {
        let mut inputs = site_inputs("green");
        inputs.workflow.native = midcreek_cs_1::sitegen::GateStatus::Failed;
        let html = build_site_from_inputs("failed-native", &inputs)
            .unwrap()
            .index_html();

        assert_text(&html, "#build-status", "CURRENT SOURCE: FAILED AT 11111111");
    }

    #[test]
    fn preserves_ascii_diagrams_as_preformatted_text() {
        let html = build_fixture_site("green").unwrap().index_html();

        assert!(html.contains("<pre><code class=\"language-text\">"));
        assert!(html.contains("main push"));
    }

    #[test]
    fn escapes_progress_and_commit_content() {
        let html = build_fixture_site("hostile-content").unwrap().index_html();

        assert!(!html.contains("<script>alert("));
        assert!(html.contains("&lt;script&gt;alert("));
    }

    #[test]
    fn renders_template_tokens_from_source_content_as_text() {
        let html = build_fixture_site("hostile-content").unwrap().index_html();

        assert!(html.contains("{{PLAY}}"));
        assert_has_element_id(&html, "play");
        assert_eq!(html.matches("No verified playable build yet").count(), 1);
    }

    #[test]
    fn comparison_images_are_accessible_and_copied_locally() {
        let site = build_fixture_site("green").unwrap();
        let html = site.index_html();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("#comparison img").unwrap();
        let images = document.select(&selector).collect::<Vec<_>>();

        assert_eq!(images.len(), 2);
        for image in images {
            let alt = image.value().attr("alt").unwrap_or_default();
            let source = image.value().attr("src").unwrap_or_default();
            assert!(!alt.trim().is_empty());
            assert!(!source.starts_with('/'));
            assert!(
                site.root().join(source).is_file(),
                "{source} was not copied"
            );
        }
    }

    #[test]
    fn progress_tasks_link_to_rendered_plan_headings() {
        let html = build_fixture_site("green").unwrap().index_html();
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse("#progress a[href^=\"#plan-\"]").unwrap();
        let links = document.select(&selector).collect::<Vec<_>>();

        assert_eq!(links.len(), 3);
        for link in links {
            let target = link.value().attr("href").unwrap();
            assert_has_element_id(&html, target.trim_start_matches('#'));
        }
    }

    #[test]
    fn emits_only_declared_site_files() {
        let site = build_fixture_site("green").unwrap();
        let generated = &site.manifest().generated_files;

        assert_eq!(
            generated,
            &[
                Path::new("index.html").to_path_buf(),
                Path::new("reference/cel-shift-character-sheet.png").to_path_buf(),
                Path::new("reference/cel-shift-key-art.png").to_path_buf(),
                Path::new("site.css").to_path_buf(),
                Path::new("site.js").to_path_buf(),
            ]
        );
    }

    #[test]
    fn build_cli_renders_the_fixture_site() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output_dir = std::env::temp_dir().join(format!(
            "midcreek-sitegen-cli-{}-{unique}",
            std::process::id()
        ));
        let output = Command::new(env!("CARGO_BIN_EXE_sitegen"))
            .current_dir(root)
            .args([
                "build",
                "--inputs",
                "tests/fixtures/sitegen/green/inputs.json",
                "--output",
                output_dir.to_str().unwrap(),
            ])
            .output()
            .expect("sitegen should launch");

        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_has_element_id(&read(output_dir.join("index.html")), "build-status");
        fs::remove_dir_all(output_dir).unwrap();
    }

    #[test]
    fn comparison_script_clamps_pointer_and_keyboard_updates() {
        let site = build_fixture_site("green").unwrap();
        let script = site.root().join("site.js");
        let harness = r#"
const fs = require("fs");
const listeners = {};
const properties = {};
const comparison = {
  style: { setProperty: (name, value) => { properties[name] = value; } }
};
const control = {
  value: "250",
  attributes: {},
  closest: () => comparison,
  addEventListener: (name, callback) => { listeners[name] = callback; },
  setAttribute: function(name, value) { this.attributes[name] = value; }
};
global.document = {
  querySelectorAll: (selector) =>
    selector === "[data-compare-control]" ? [control] : [],
  querySelector: () => null
};
global.window = {};
eval(fs.readFileSync(process.argv[1], "utf8"));
if (properties["--comparison"] !== "100%") process.exit(10);
if (control.attributes["aria-valuenow"] !== "100") process.exit(11);
control.value = "-20";
listeners.input();
if (properties["--comparison"] !== "0%") process.exit(12);
control.value = "40";
listeners.change();
if (properties["--comparison"] !== "40%") process.exit(13);
"#;
        let output = Command::new("node")
            .args(["-e", harness, script.to_str().unwrap()])
            .output()
            .expect("Node should execute the dependency-free site script");

        assert_eq!(
            output.status.code(),
            Some(0),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

mod output_validation_contract {
    use super::*;

    #[test]
    fn rejects_more_than_one_main_element() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace("</body>", "<main id=\"extra-main\"></main></body>")
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));
        assert!(
            matches!(result, Err(SitegenError::InvalidHtml { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_duplicate_element_ids() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace(
                "<section class=\"panel section-panel\" id=\"play\">",
                "<section class=\"panel section-panel\" id=\"build-status\">",
            )
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));
        assert!(
            matches!(result, Err(SitegenError::InvalidHtml { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_images_without_alt_text() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace("alt=\"Approved Cel Shift key art reference\"", "alt=\"\"")
        });

        assert_eq!(
            validate_site_output(site.root(), &fixture("green/progress.json")),
            Err(SitegenError::MissingAltText {
                source: Path::new("index.html").to_path_buf(),
            })
        );
    }

    #[test]
    fn rejects_missing_local_resources() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace("href=\"site.css\"", "href=\"missing.css\"")
        });

        assert_eq!(
            validate_site_output(site.root(), &fixture("green/progress.json")),
            Err(SitegenError::BrokenLocalLink {
                source: Path::new("index.html").to_path_buf(),
                target: Path::new("missing.css").to_path_buf(),
            })
        );
    }

    #[test]
    fn rejects_absolute_local_paths() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace("href=\"site.css\"", "href=\"/Users/example/site.css\"")
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));
        assert!(
            matches!(result, Err(SitegenError::InvalidHtml { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_inline_script_content() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace("</body>", "<script>alert(1)</script></body>")
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));
        assert!(
            matches!(result, Err(SitegenError::InvalidHtml { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_progress_tasks_that_do_not_link_to_their_plan_heading() {
        let site = build_fixture_site("green").unwrap();
        mutate_index(&site, |html| {
            html.replace(
                "data-progress-task=\"pages-foundation\" href=\"#plan-pages-foundation\"",
                "data-progress-task=\"pages-foundation\" href=\"#play\"",
            )
        });

        let result = validate_site_output(site.root(), &fixture("green/progress.json"));
        assert!(
            matches!(result, Err(SitegenError::InvalidHtml { .. })),
            "{result:?}"
        );
    }

    fn mutate_index(site: &support::GeneratedSite, mutation: impl FnOnce(String) -> String) {
        let path = site.root().join("index.html");
        fs::write(&path, mutation(read(&path))).unwrap();
    }
}

mod build_safety_contract {
    use super::*;

    #[test]
    fn rejects_source_directories_as_site_output() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"));

        assert_eq!(
            validate_output_path(&repository.join("docs")),
            Err(SitegenError::UnsafeOutputPath {
                path: repository.join("docs"),
            })
        );
    }

    #[test]
    fn rejects_reference_paths_outside_the_approved_manifest() {
        let mut inputs = site_inputs("green");
        inputs.reference_manifest.assets[0].public_path = "../../Cargo.toml".to_owned();
        let output =
            std::env::temp_dir().join(format!("midcreek-unsafe-reference-{}", std::process::id()));

        let result = build_site(&inputs, &output);
        let _ = fs::remove_dir_all(output);

        assert!(matches!(result, Err(SitegenError::Reference(_))));
    }
}

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
                "autonomous-verification\n"
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
                    ProgressStatus::Done,
                    &["foundation-contracts"][..],
                    "Published the status, plan, challenge, comparison, test, and commit sections as the status-only Pages hub.",
                    Some("HEAD"),
                ),
                (
                    "autonomous-assets",
                    ProgressStatus::Done,
                    &["pages-foundation"][..],
                    "Generated every cel-shift asset autonomously from declarative RON: an 11-bone rigged technician with Idle, Walk and Repair clips, a merged eight-cabinet rack row with server slots and status lights, a cooling unit, the red utility cart and yellow step stool, and overhead tray plus black hose modules.",
                    Some("HEAD"),
                ),
                (
                    "data-hall",
                    ProgressStatus::Done,
                    &["autonomous-assets"][..],
                    "Loaded every generated GLB through an explicit Loading, Ready, and Failed asset state with no procedural fallback, cached one unit mesh per primitive shape and one unlit material per palette role, and spawned the authored 40 m square hall: a polished floor inside low perimeter walls, four rack rows separated by three traversable aisles, four cooling units, three overhead trays with hose drops, painted yellow aisle and walkway markings, the red service cart, and the yellow step stool. Visual and collider lists stay separate and are joined by stable PropId, the colliders are extracted once into a cached vector, and a room-wide flood fill proves every aisle checkpoint shares one walkable component with the player spawn.",
                    Some("HEAD"),
                ),
                (
                    "technician-movement",
                    ProgressStatus::Done,
                    &["data-hall"][..],
                    "Spawned the generated 11-bone technician only after the assets and the hall were both ready, bound every required rig node into explicit PlayerParts with the rest transform captured before any clip played, and moved it from the real ButtonInput<KeyCode> arrow state. A public ViewBasis resource, initialised to the reviewed NorthEast 45-degree view, is the only screen-to-world interface movement reads, so the orbit task becomes its runtime updater without changing a movement rule. Opposite arrows cancel and diagonals are normalized, world X and world Z resolve separately against the cached colliders and radius-aware room bounds so the technician slides along rack faces, and facing plus the Idle and Walk clips follow the accepted displacement rather than the request. The idle transition stops Walk, explicitly restores every PlayerParts rest transform, and only then plays Idle; the Repair clip is bound for the operations task and nothing plays it yet. Missing, duplicate, and stale rig nodes are typed failures that stop movement instead of being silently skipped.",
                    Some("HEAD"),
                ),
                (
                    "camera-orbit",
                    ProgressStatus::Done,
                    &["technician-movement"][..],
                    "Spawned the one orthographic game camera with the reviewed fixed 26 m by 14.625 m rectangle, 57-degree elevation, zero roll, and the initial NorthEast 45-degree yaw, which is all the authored unlit hall needs to be visible. Real Q and E just_pressed frames retarget the desired heading immediately, opposite keys on one frame cancel exactly, and the yaw eases with smoothstep at a constant 90 degrees per 0.30 seconds, so a retarget mid-tween starts at the interpolated yaw and its duration scales with the shortest remaining angle instead of restarting a fixed clock. CameraPlugin is now the sole runtime updater of ViewBasis and publishes the live interpolated basis in UpdateOrbitIntent, before MovePlayer reads it, so the technician walks along the camera it can actually see mid-orbit. Every frame the ground quadrilateral is cast from the current yaw, its axis-aligned extents are subtracted from the active blueprint's own room.coverage, and the followed technician is clamped into what remains before the transform is derived, so an overridden hall is followed by the coverage it declares rather than by the authored constant. The walkable room stays exactly 40 m, but the camera is clamped against the rendered coverage the new visual apron fills, so it overhangs the room freely and follows every legal player position exactly: every room corner is centred with the full 360 px of margin at every heading and at every exact tween midpoint, while the whole view stays inside the apron and zoom, elevation, and roll stay fixed. Framing is measured against the technician's whole generated spatial envelope rather than its ground origin -- the rest pose and every sampled Idle, Walk, and Repair pose, cel outlines included, 0.7998 m of radius and a 1.9704 m crown -- and every corner of it keeps at least 32 logical pixels of viewport margin, calibrated against an independent 49.23077 px per metre edge point.",
                    Some("HEAD"),
                ),
                (
                    "operations-loop",
                    ProgressStatus::Done,
                    &["data-hall", "technician-movement"][..],
                    "Attached the documented rack state machine to the four authored rack-row HallProp entities by stable PropId, joined to their cached collider rectangles once, and drove it from one seeded ChaCha8 fault stream. The scheduler is a pure sequence generator: every four simulated seconds an opportunity matures and the timer stops accumulating, a full queue pauses it without consuming a single word of randomness, and a drawn candidate whose rack already holds a ticket or is still resolving or cooling down is held rather than rerolled, so the exact rack and severity order is the same whatever the player does and only its timing moves. Tickets carry stable monotonic identifiers and sort Critical before Warning, then by creation tick, then by rack; the queue enforces the maximum of three active tickets and one ticket per rack and reports which rack it refused. A real Space press gathers only open faults, measures distance to each rack's collider rectangle rather than its centre, and selects by severity, then distance, then creation tick, then rack, so an out-of-range press changes nothing and is recorded as a named rejection instead of a silent no-op. Starting a repair in UpdateOperations locks movement in the same frame, before MovePlayer runs, zeroes the published motion, plays the generated Repair clip, and exposes the blue wrench state for the HUD task while the camera keeps orbiting and following. Repair completes on its own after three seconds and releases the lock, the healthy indicator shows for two seconds, the ticket then leaves the queue as the rack begins an eight second cooldown, and only a fully recovered rack is eligible again.",
                    Some("HEAD"),
                ),
                (
                    "operations-hud",
                    ProgressStatus::Done,
                    &["camera-orbit", "operations-loop"][..],
                    "Added src/hud.rs, which draws the whole operations HUD from the live operations model and owns no gameplay state of its own: every frame it reads TicketQueue, RackOperations, RackRoster, MovementLock, LastInteraction, and the real camera, and writes only presentation components plus one observable HudReport, so there is no second ticket model to drift. The top-left stack renders up to three rows straight out of the queue's own global priority order rather than re-sorting it, each row carrying a severity chip, a rack-state chip, a short real label such as T0002 R01 Critical, and a dwell-progress bar; shape carries meaning beside colour, with a square critical chip against a round warning chip and sharp, rounded, and pill badges for fault, repair, and resolved. The status line is derived from live state alone, preferring a running repair, then a real out-of-range rejection while that rejection is still true of the one rack it named, then the queue; move_closer_still_stands re-reads the live queue, that rack's live state, its roster collider, and the technician's live position every frame, so the prompt clears the moment that rack's ticket leaves the queue, that rack stops being Faulted, or the technician walks inside repair range, and another rack's open ticket can never keep a stale prompt alive. Badges are fixed-size screen-space UI nodes anchored every frame from a stable world point 2.4 m above each rack's collider centre through the real Camera::world_to_viewport, reading the camera's own Transform rather than its propagated GlobalTransform so they never lag a tween by a frame, a substitution the camera query enforces with Without<ChildOf> so a parented camera is refused as unusable and its badges are hidden rather than projected through a local transform pretending to be a global one; a visible anchor always gets a fully visible badge because the badge box is clamped inside the viewport and its thin leader line is rotated to end exactly on the projected anchor, while an anchor that leaves the viewport hides explicitly and a refused projection is recorded as a typed error. The bottom-right strip names Arrows, Q, E, and Space and shows which are live, turning the Space cap hard-hat blue and flattening the Arrows cap while a repair holds the technician still. Every colour is a typed PaletteRole, the 216 px queue stack and 40 px control strip keep a 16 px margin and stay outside the central 50% x 50% play rectangle at both 1280x720 and 960x540, and HudReport reports a rack that lost its RackOperations, a ticket whose rack is unknown, a missing row node, a missing badge node exactly once rather than once per write and again per hide, an unusable game camera, and a camera with no viewport, each under its own BadgeVisibility variant so no failure is ever explained with another's name. Shape, glyph, and progress are node-level contracts: the app tests read Node::border_radius off the severity chips and off all three badge states, the glyph text off the real RackBadgeLabel node, and the progress bar's Node::width against the live dwell progress, and each is covered by a mutation that fails when the write is deleted or swapped.",
                    Some("HEAD"),
                ),
                (
                    "pages-playable",
                    ProgressStatus::Done,
                    &["operations-hud"][..],
                    "Packaged the production game for wasm32-unknown-unknown with a pinned wasm-bindgen, added a wasm-only WebReadyPlugin handshake, published it under play/ with last-green metadata, and proved it in headless Chromium.",
                    Some("HEAD"),
                ),
                (
                    "autonomous-verification",
                    ProgressStatus::InProgress,
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

            let challenges = document
                .challenges
                .iter()
                .map(|challenge| challenge.id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                challenges,
                vec![
                    "byte-reproducible-assets-without-a-dcc",
                    "loud-asset-loading-without-a-gpu",
                    "rig-handles-that-survive-instance-respawn",
                    "clamped-orbit-cannot-frame-a-room-corner",
                    "seeded-faults-that-player-timing-cannot-perturb",
                    "screen-space-badges-that-survive-orbit-and-resize",
                    "a-browser-gate-that-cannot-pass-on-a-blank-page",
                    "a-hundred-megabyte-wasm-game-cannot-be-published",
                ]
            );
            for challenge in &document.challenges {
                assert!(!challenge.title.trim().is_empty());
                assert!(!challenge.impact.trim().is_empty());
                assert!(!challenge.approach.trim().is_empty());
                assert!(
                    challenge
                        .resolution
                        .as_deref()
                        .is_some_and(|resolution| !resolution.trim().is_empty()),
                    "{} must record what was actually done about it",
                    challenge.id
                );
                // An open challenge still carries its full context, but it has
                // no commit that closed it, because nothing has.
                match challenge.status {
                    ChallengeStatus::Resolved => {
                        assert_eq!(challenge.resolved_commit.as_deref(), Some("HEAD"))
                    }
                    _ => assert_eq!(
                        challenge.resolved_commit, None,
                        "{} is not resolved, so it must not claim a resolving commit",
                        challenge.id
                    ),
                }
            }
            assert_eq!(
                document
                    .challenges
                    .iter()
                    .filter(|challenge| challenge.status != ChallengeStatus::Resolved)
                    .map(|challenge| challenge.id.as_str())
                    .collect::<Vec<_>>(),
                Vec::<&str>::new(),
                "the corner-framing gap was resolved by the rendered-coverage apron, so no challenge is open"
            );
        }

        #[test]
        fn published_plan_matches_the_approved_master_plan() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            assert_eq!(
                sha256(root.join("docs/implementation-plan.md")),
                "567400415c0a6296ba765de2cfae0a3f0575170a4f754a320f50ef69202ff49a"
            );
        }

        #[test]
        fn published_plan_contains_no_absolute_local_paths() {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let plan = read(root.join("docs/implementation-plan.md"));
            let has_windows_drive_root = plan.as_bytes().windows(3).any(|window| {
                window[0].is_ascii_alphabetic()
                    && window[1] == b':'
                    && matches!(window[2], b'/' | b'\\')
            });

            assert!(!plan.contains("/Users/"), "plan contains a macOS user path");
            assert!(!plan.contains("file://"), "plan contains a file URL");
            assert!(
                !has_windows_drive_root,
                "plan contains a Windows drive-root path"
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

mod playable_publication_contract {
    use super::*;
    use midcreek_cs_1::sitegen::PlayableBuild;
    use std::path::PathBuf;

    const GREEN_COMMIT: &str = "1111111111111111111111111111111111111111";

    #[test]
    fn a_green_playable_build_is_copied_under_play_and_embedded_in_the_hub() {
        let package = package("complete", PACKAGE_FILES);
        let mut inputs = site_inputs("green");
        inputs.playable = Some(playable(package.path()));

        let site = build_site_from_inputs("playable-green", &inputs).unwrap();
        let html = site.index_html();

        assert!(site.root().join("play/index.html").is_file());
        assert!(site.root().join("play/game_bg.wasm").is_file());
        assert!(site.root().join("play/assets/generated/rack.glb").is_file());
        assert!(html.contains("play/index.html"), "{html}");
        assert!(!html.contains("No verified playable build yet"));
        assert_eq!(
            site.manifest().playable_commit.as_deref(),
            Some(GREEN_COMMIT)
        );
    }

    #[test]
    fn a_green_playable_build_records_last_green_metadata() {
        let package = package("metadata", PACKAGE_FILES);
        let mut inputs = site_inputs("green");
        inputs.playable = Some(playable(package.path()));

        let site = build_site_from_inputs("playable-metadata", &inputs).unwrap();
        let metadata: midcreek_cs_1::sitegen::LastGreenManifest =
            serde_json::from_str(&read(site.root().join("last-green.json"))).unwrap();

        assert_eq!(metadata.source_commit, GREEN_COMMIT);
        assert_eq!(metadata.semantic_visual_hash, None);
        assert_eq!(
            metadata.game_files,
            [
                "play/assets/generated/rack.glb",
                "play/game.js",
                "play/game_bg.wasm",
                "play/index.html",
                "play/play.js",
            ]
            .map(PathBuf::from)
        );
        assert!(metadata.screenshot_files.is_empty());
    }

    #[test]
    fn a_package_without_the_wasm_payload_is_refused_instead_of_published() {
        let package = package(
            "no-wasm",
            &[
                ("index.html", "<!doctype html>"),
                ("play.js", "// bootstrap"),
                ("game.js", ""),
            ],
        );
        let mut inputs = site_inputs("green");
        inputs.playable = Some(playable(package.path()));

        let result = build_site_from_inputs("playable-broken", &inputs);

        assert!(
            matches!(
                &result,
                Err(SitegenError::MissingInput { path })
                    if path.file_name().is_some_and(|name| name == "game_bg.wasm")
            ),
            "{:?}",
            result.as_ref().err()
        );
    }

    #[test]
    fn a_package_that_escapes_its_directory_is_refused() {
        let package = package("escaping", PACKAGE_FILES);
        let mut inputs = site_inputs("green");
        let mut build = playable(package.path());
        build.directory = package.path().join("../..");
        inputs.playable = Some(build);

        let result = build_site_from_inputs("playable-escaping", &inputs);

        assert!(
            result.is_err(),
            "{:?}",
            result.map(|site| site.index_html())
        );
    }

    const PACKAGE_FILES: &[(&str, &str)] = &[
        ("index.html", "<!doctype html><html><body></body></html>"),
        ("game.js", "export default function init() {}"),
        ("game_bg.wasm", "\0asm"),
        ("play.js", "// bootstrap"),
        ("assets/generated/rack.glb", "glTF"),
    ];

    fn playable(directory: &Path) -> PlayableBuild {
        PlayableBuild {
            directory: directory.to_path_buf(),
            source_commit: GREEN_COMMIT.to_owned(),
            run_url: "https://example.invalid/run/1".to_owned(),
        }
    }

    struct Package(PathBuf);

    impl Package {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Package {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn package(name: &str, files: &[(&str, &str)]) -> Package {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "midcreek-web-package-{}-{unique}-{name}",
            std::process::id()
        ));
        for (relative, contents) in files {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        Package(root)
    }
}

mod web_source_contract {
    use super::*;

    fn repository() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    fn source(relative: &str) -> String {
        read(repository().join(relative))
    }

    #[test]
    fn the_play_template_declares_explicit_browser_states() {
        let html = source("site/templates/play.html");

        assert!(html.contains(r#"data-game-state="loading""#), "{html}");
        assert!(html.contains(r#"id="browser-errors""#), "{html}");
        assert!(html.contains(r#"id="game-canvas""#), "{html}");
    }

    #[test]
    fn the_bootstrap_captures_errors_and_unhandled_rejections() {
        let js = source("site/static/play.js");

        assert!(js.contains(r#"window.addEventListener("error""#), "{js}");
        assert!(
            js.contains(r#"window.addEventListener("unhandledrejection""#),
            "{js}"
        );
        assert!(js.contains("browser-errors"), "{js}");
    }

    #[test]
    fn the_bootstrap_prevents_scrolling_only_for_the_reviewed_keys_while_focused() {
        let js = source("site/static/play.js");

        for key in [
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "KeyQ",
            "KeyE",
            "Space",
        ] {
            assert!(js.contains(key), "the bootstrap must scope {key}");
        }
        assert!(js.contains("preventDefault"), "{js}");
        assert!(js.contains("document.activeElement"), "{js}");
    }

    #[test]
    fn the_browser_shell_only_uses_relative_urls() {
        for relative in ["site/templates/play.html", "site/static/play.js"] {
            let text = source(relative);
            for absolute in [r#"src="/"#, r#"href="/"#, r#"from "/"#, r#"import("/"#] {
                assert!(
                    !text.contains(absolute),
                    "{relative} must work below a project path prefix, found {absolute}"
                );
            }
        }
    }

    #[test]
    fn the_game_wires_the_browser_handshake_only_for_wasm() {
        let lib = source("src/lib.rs");

        assert!(lib.contains(r#"#[cfg(target_arch = "wasm32")]"#), "{lib}");
        assert!(lib.contains("web::WebReadyPlugin"), "{lib}");
    }

    #[test]
    fn the_web_build_pins_the_locked_wasm_bindgen_version() {
        let locked = locked_version("wasm-bindgen");
        let manifest = source("Cargo.toml");
        let script = source("scripts/build-web.sh");

        assert!(
            manifest.contains(&format!(r#"wasm-bindgen = "={locked}""#)),
            "Cargo.toml must pin wasm-bindgen to the locked {locked}"
        );
        assert!(script.contains("Cargo.lock"), "{script}");
        assert!(script.contains("wasm-bindgen --version"), "{script}");
    }

    #[test]
    fn the_web_build_runs_the_reviewed_packaging_pipeline() {
        let script = source("scripts/build-web.sh");

        for fragment in [
            "set -euo pipefail",
            "--release",
            "wasm32-unknown-unknown",
            "--target web",
            "--no-typescript",
            "assets/generated",
            "site/templates/play.html",
            "site/static/play.js",
        ] {
            assert!(
                script.contains(fragment),
                "build-web.sh must contain {fragment}"
            );
        }
    }

    #[test]
    fn the_browser_gate_enforces_the_reviewed_checks() {
        let script = source("scripts/web-smoke.sh");
        let driver = source("scripts/browser_gate.py");

        assert!(script.contains("set -euo pipefail"), "{script}");
        assert!(script.contains("trap "), "cleanup must be trapped");
        assert!(script.contains("--remote-debugging-port"), "{script}");
        for fragment in [
            "READY_TIMEOUT_SECONDS = 30",
            "data-game-state",
            "browser-errors",
            "Input.dispatchKeyEvent",
            "Page.captureScreenshot",
            "scrollY",
        ] {
            assert!(
                driver.contains(fragment),
                "browser_gate.py must contain {fragment}"
            );
        }
    }

    #[test]
    fn the_web_scripts_are_executable() {
        for relative in ["scripts/build-web.sh", "scripts/web-smoke.sh"] {
            let metadata = fs::metadata(repository().join(relative)).unwrap();
            let mode = std::os::unix::fs::PermissionsExt::mode(&metadata.permissions());
            assert_eq!(mode & 0o111, 0o111, "{relative} must be executable");
        }
    }

    fn locked_version(package: &str) -> String {
        let lock = source("Cargo.lock");
        let marker = format!("name = \"{package}\"\nversion = \"");
        let start = lock.find(&marker).expect("package should be locked") + marker.len();
        lock[start..]
            .split('"')
            .next()
            .expect("locked version should terminate")
            .to_owned()
    }
}
