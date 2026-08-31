use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use midcreek_cs_1::sitegen::{
    BuildDisposition, CurrentPublication, GateStatus, LastGreenManifest, SitegenError,
    WorkflowSummary, assemble_site, validate_assembled_links,
};
use sha2::{Digest, Sha256};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

fn bash_command() -> PathBuf {
    if cfg!(windows) {
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

#[test]
fn first_run_without_game_publishes_status_only() {
    let current = fixture_site("status-only", &[("index.html", "CURRENT SOURCE: GREEN")]);
    let output = TempDirectory::new("first-run-output");

    let disposition = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FailedRetainLastGreen);
    assert_eq!(sha256(output.path().join("play/game_bg.wasm")), old_hash);
}

// ---------------------------------------------------------------------------
// The homepage displayed by assembly must agree with the game assembly
// really retained, distinct from whatever the failing current run produced.
// ---------------------------------------------------------------------------

/// A previous publication's complete package plus the retained metadata that
/// names the commit it was built from.
fn previous_retained_playable(name: &str) -> TempDirectory {
    let mut files = vec![(
        "last-green.json",
        r#"{"source_commit":"2222222222222222222222222222222222222222","semantic_visual_hash":"aaaaaaaa","game_files":["play/assets/generated/rack.glb","play/game.js","play/game_bg.wasm","play/index.html","play/play.css","play/play.js"],"screenshot_files":[]}"#,
    )];
    files.extend_from_slice(COMPLETE_PACKAGE);
    fixture_site(name, &files)
}

/// A `current` tree exactly as `build_site` leaves one when the run produced
/// no playable candidate of its own: the reconcilable `{{PLAY}}`/`{{MODE}}`
/// sections are marked, and the pending panel carries no `play-embed` iframe.
/// The status grid mirrors `render_status`'s own shape for a failed run at
/// `workflow_summary`'s commit, so tests can assert it survives reconciliation
/// untouched.
const PENDING_CURRENT_PAGE: &str = r#"<!doctype html><html><head><title>Hub</title></head><body><main>
<div class="status-grid"><div class="status-item"><span>Source</span><strong>CURRENT SOURCE: FAILED AT 11111111</strong></div>
<div class="status-item"><span>Native</span><strong class="status-failed">Failed</strong></div>
<div class="status-item"><span>Web</span><strong class="status-skipped">Not run</strong></div></div>
<!--play--><div class="play-frame" role="img" aria-label="pending"><div class="empty-state play-empty"><h2>No verified playable build yet</h2></div></div><!--/play-->
<!--mode--><div class="hero-badge"><strong>Status</strong><small>Game pending verification</small></div><!--/mode-->
</main></body></html>"#;

#[test]
fn a_failed_run_with_a_complete_retained_package_shows_the_playable_iframe() {
    let previous = previous_retained_playable("previous-retained-playable");
    let current = fixture_site(
        "current-failed-no-candidate",
        &[("index.html", PENDING_CURRENT_PAGE)],
    );
    let output = TempDirectory::new("failed-retained-playable-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        // The current run's own commit, distinct from the retained one.
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FailedRetainLastGreen);
    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(
        index.contains(r#"<iframe class="play-embed" src="play/index.html""#),
        "the retained package must be embedded: {index}"
    );
    assert!(
        index.contains(r#"<a href="play/index.html">"#),
        "a direct play link must point at the retained package: {index}"
    );
    assert!(
        !index.contains("No verified playable build yet"),
        "the pending panel must not survive a real retained package: {index}"
    );
    assert!(
        index.contains(r#"<dt>Playable build</dt><dd><code>22222222</code></dd>"#),
        "the retained commit must be named honestly: {index}"
    );
    // Production only ever renders the short SHA, so the meaningful refusal
    // is that the *current* run's own short SHA is never credited as the
    // retained build, not merely that the full 40-char SHA is absent (it
    // never appears anywhere, bug or not).
    assert!(
        !index.contains(r#"<dt>Playable build</dt><dd><code>11111111</code></dd>"#),
        "the failed current commit must never be credited with the retained game: {index}"
    );
    assert!(
        index.contains(">Retained<"),
        "the mode badge must distinguish a retained game from a current one: {index}"
    );
    assert!(!index.contains(">Status<"), "{index}");
    // The current run's own failing source and gate status must survive
    // reconciliation untouched: reconciling the playable panel must never
    // touch the status grid that reports the current run's own truth.
    assert!(
        index.contains("CURRENT SOURCE: FAILED AT 11111111"),
        "the current run's own failing source must be preserved verbatim: {index}"
    );
    assert!(
        index.contains(r#"<strong class="status-failed">Failed</strong>"#),
        "the current run's own gate status must be preserved verbatim: {index}"
    );
    // The retained iframe path really resolves against the assembled tree.
    validate_assembled_links(output.path()).unwrap();
}

#[test]
fn a_status_only_retention_without_failure_also_shows_the_playable_iframe() {
    let previous = previous_retained_playable("previous-retained-playable-status-only");
    let current = fixture_site(
        "current-status-only-no-candidate",
        &[("index.html", PENDING_CURRENT_PAGE)],
    );
    let output = TempDirectory::new("status-only-retained-playable-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &status_only_workflow(),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::RetainLastGreen);
    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(
        index.contains(r#"<iframe class="play-embed" src="play/index.html""#),
        "{index}"
    );
}

#[test]
fn a_run_that_already_shows_its_own_game_is_left_exactly_as_it_is() {
    let previous = previous_retained_playable("previous-retained-playable-superseded");
    let current = fixture_site(
        "current-with-its-own-game",
        &[(
            "index.html",
            r#"<!doctype html><html><head><title>Hub</title></head><body><main>
<!--play--><div class="play-frame play-frame-live"><iframe class="play-embed" src="play/index.html"></iframe></div><!--/play-->
<!--mode--><div class="hero-badge"><strong>Verified</strong></div><!--/mode-->
</main></body></html>"#,
        )],
    );
    let output = TempDirectory::new("already-playable-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &status_only_workflow(),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(index.contains(">Verified<"), "{index}");
}

#[test]
fn malformed_retained_metadata_leaves_the_homepage_pending() {
    let mut files = vec![("last-green.json", "not json")];
    files.extend_from_slice(COMPLETE_PACKAGE);
    let previous = fixture_site("previous-malformed-metadata", &files);
    let current = fixture_site(
        "current-failed-malformed-metadata",
        &[("index.html", PENDING_CURRENT_PAGE)],
    );
    let output = TempDirectory::new("malformed-metadata-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(
        index.contains("No verified playable build yet"),
        "a retained package with unparsable metadata must never be trusted: {index}"
    );
    assert!(!index.contains("play-embed"), "{index}");
}

#[test]
fn an_incomplete_retained_package_leaves_the_homepage_pending() {
    let previous = fixture_site(
        "previous-incomplete-package",
        &[
            (
                "last-green.json",
                r#"{"source_commit":"2222222222222222222222222222222222222222","semantic_visual_hash":null,"game_files":["play/index.html"],"screenshot_files":[]}"#,
            ),
            ("play/index.html", "old shell"),
        ],
    );
    let current = fixture_site(
        "current-failed-incomplete-retained",
        &[("index.html", PENDING_CURRENT_PAGE)],
    );
    let output = TempDirectory::new("incomplete-retained-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(
        index.contains("No verified playable build yet"),
        "an incomplete retained package must never be embedded: {index}"
    );
}

#[test]
fn a_retained_source_commit_that_is_not_a_full_sha_leaves_the_homepage_pending() {
    let mut files = vec![(
        "last-green.json",
        r#"{"source_commit":"not-a-real-commit-sha","semantic_visual_hash":"aaaaaaaa","game_files":["play/assets/generated/rack.glb","play/game.js","play/game_bg.wasm","play/index.html","play/play.css","play/play.js"],"screenshot_files":[]}"#,
    )];
    files.extend_from_slice(COMPLETE_PACKAGE);
    let previous = fixture_site("previous-unsafe-commit-text", &files);
    let current = fixture_site(
        "current-failed-unsafe-commit-text",
        &[("index.html", PENDING_CURRENT_PAGE)],
    );
    let output = TempDirectory::new("unsafe-commit-text-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(
        index.contains("No verified playable build yet"),
        "a retained commit that is not a full SHA must never be trusted as provenance: {index}"
    );
    assert!(!index.contains("play-embed"), "{index}");
}

#[test]
fn retained_metadata_declaring_a_file_list_inconsistent_with_its_package_leaves_the_homepage_pending()
 {
    // The package on disk is structurally complete (every required file and
    // the assets directory are present), but the manifest's own declared
    // file list disagrees with it by naming a file the package does not
    // actually contain alongside the real ones. `missing_playable_parts`
    // alone cannot catch this: it only checks the fixed required set, never
    // cross-checks the manifest's own declared list against reality.
    let mut files = vec![(
        "last-green.json",
        r#"{"source_commit":"2222222222222222222222222222222222222222","semantic_visual_hash":"aaaaaaaa","game_files":["play/assets/generated/rack.glb","play/decoy-file-not-really-published.json","play/game.js","play/game_bg.wasm","play/index.html","play/play.css","play/play.js"],"screenshot_files":[]}"#,
    )];
    files.extend_from_slice(COMPLETE_PACKAGE);
    let previous = fixture_site("previous-inconsistent-file-list", &files);
    let current = fixture_site(
        "current-failed-inconsistent-file-list",
        &[("index.html", PENDING_CURRENT_PAGE)],
    );
    let output = TempDirectory::new("inconsistent-file-list-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(
        index.contains("No verified playable build yet"),
        "a manifest whose declared file list disagrees with its package must never be trusted: {index}"
    );
    assert!(!index.contains("play-embed"), "{index}");
}

/// A pending current page whose `{{PLAY}}` and `{{MODE}}` markers are
/// malformed so that their located spans cross: the `mode` open marker sits
/// inside what `marked_span("play", ..)` reports as the play span, and the
/// `mode` close marker sits after it, so the two ranges overlap instead of
/// nesting cleanly one after the other.
const CROSSING_MARKERS_PAGE: &str = r#"<!doctype html><html><head><title>Hub</title></head><body><main>
<!--play-->PLAY_BODY<!--mode-->MODE_BODY<!--/play-->TAIL<!--/mode-->
</main></body></html>"#;

#[test]
fn crossing_play_and_mode_markers_are_refused_without_partial_reconciliation() {
    let previous = previous_retained_playable("previous-retained-playable-crossing-markers");
    let current = fixture_site(
        "current-failed-crossing-markers",
        &[("index.html", CROSSING_MARKERS_PAGE)],
    );
    let output = TempDirectory::new("crossing-markers-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert_eq!(
        index, CROSSING_MARKERS_PAGE,
        "crossing/nested play and mode marker spans must be refused verbatim, \
         never partially reconciled: {index}"
    );
}

/// A `{{PLAY}}` section with two opening markers before its single closing
/// marker. `marked_span`'s naive first-open/first-close search locates the
/// span from the *first* open to the *first* close, so a replacement would
/// consume only `<!--play-->PLAY_ONE<!--play-->PLAY_TWO<!--/play-->` and
/// leave the literal `TAIL` text stranded in the assembled page. The `mode`
/// marker pair is well-formed and disjoint from that (wrongly) located play
/// span, so this defect is isolated from the already-covered play/mode
/// crossing case above: it is a defect entirely within one marker name.
const DUPLICATE_OPENING_PLAY_MARKERS: &str = r#"<!doctype html><html><head><title>Hub</title></head><body><main>
<!--play-->PLAY_ONE<!--play-->PLAY_TWO<!--/play-->TAIL
<!--mode-->MODE_BODY<!--/mode-->
</main></body></html>"#;

/// A `{{PLAY}}` section with two closing markers after its single opening
/// marker. The naive search locates `<!--play-->PLAY_ONE<!--/play-->` as the
/// span, stranding the literal `TAIL<!--/play-->` text after it.
const DUPLICATE_CLOSING_PLAY_MARKERS: &str = r#"<!doctype html><html><head><title>Hub</title></head><body><main>
<!--play-->PLAY_ONE<!--/play-->TAIL<!--/play-->
<!--mode-->MODE_BODY<!--/mode-->
</main></body></html>"#;

/// A `{{PLAY}}` section nested inside another same-named section:
/// `<!--play-->A<!--play-->B<!--/play-->C<!--/play-->`. The naive search
/// locates only the inner `<!--play-->B<!--/play-->` pair, stranding both the
/// leading duplicate opening marker and the trailing `C<!--/play-->` text.
const SAME_NAME_NESTED_PLAY_MARKERS: &str = r#"<!doctype html><html><head><title>Hub</title></head><body><main>
<!--play-->A<!--play-->B<!--/play-->C<!--/play-->
<!--mode-->MODE_BODY<!--/mode-->
</main></body></html>"#;

#[test]
fn duplicate_or_nested_same_name_play_markers_are_refused_without_partial_reconciliation() {
    for malformed_page in [
        DUPLICATE_OPENING_PLAY_MARKERS,
        DUPLICATE_CLOSING_PLAY_MARKERS,
        SAME_NAME_NESTED_PLAY_MARKERS,
    ] {
        let previous = previous_retained_playable("previous-retained-playable-duplicate-markers");
        let current = fixture_site(
            "current-failed-duplicate-markers",
            &[("index.html", malformed_page)],
        );
        let output = TempDirectory::new("duplicate-markers-output");

        assemble_site(
            Some(previous.path()),
            current.path(),
            &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
            CurrentPublication::Generated,
            output.path(),
        )
        .unwrap();

        let index = fs::read_to_string(output.path().join("index.html")).unwrap();
        assert_eq!(
            index, *malformed_page,
            "a play marker with more than one opening or closing tag for the \
             same name must be refused verbatim, never partially \
             reconciled: {index}"
        );
    }
}

/// Two chained `assemble_site` runs, the second treating the first's output
/// as its own `previous`, so a test can prove whether an inconsistency this
/// round is still caught the *next* round rather than being silently
/// resynced away.
fn assemble_twice_with_untrusted_previous(
    previous: &Path,
    intermediate_name: &str,
    final_name: &str,
) -> TempDirectory {
    let current_one = fixture_site(
        &format!("{intermediate_name}-current"),
        &[("index.html", PENDING_CURRENT_PAGE)],
    );
    let intermediate = TempDirectory::new(intermediate_name);
    assemble_site(
        Some(previous),
        current_one.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        intermediate.path(),
    )
    .unwrap();

    let current_two = fixture_site(
        &format!("{final_name}-current"),
        &[("index.html", PENDING_CURRENT_PAGE)],
    );
    let output = TempDirectory::new(final_name);
    assemble_site(
        Some(intermediate.path()),
        current_two.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();
    output
}

#[test]
fn an_inconsistent_retained_file_list_is_never_rehabilitated_by_the_next_publication() {
    // The manifest's declared file list disagrees with its own package (an
    // extra file that does not exist), exactly like
    // `retained_metadata_declaring_a_file_list_inconsistent_with_its_package_leaves_the_homepage_pending`.
    // That test only proves *this* round stays pending. This test proves the
    // inconsistency itself is not quietly repaired into trustworthy
    // provenance for the *next* round to pick up.
    let mut files = vec![(
        "last-green.json",
        r#"{"source_commit":"2222222222222222222222222222222222222222","semantic_visual_hash":"aaaaaaaa","game_files":["play/assets/generated/rack.glb","play/decoy-file-not-really-published.json","play/game.js","play/game_bg.wasm","play/index.html","play/play.css","play/play.js"],"screenshot_files":[]}"#,
    )];
    files.extend_from_slice(COMPLETE_PACKAGE);
    let previous = fixture_site("previous-inconsistent-file-list-chained", &files);

    let output = assemble_twice_with_untrusted_previous(
        previous.path(),
        "inconsistent-file-list-intermediate",
        "inconsistent-file-list-final",
    );

    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(
        index.contains("No verified playable build yet"),
        "an inconsistency detected one round must still be refused the next round, \
         not silently rehabilitated by the intervening resync: {index}"
    );
    assert!(!index.contains("play-embed"), "{index}");
}

#[test]
fn current_evidence_is_never_credited_from_a_run_that_attempted_no_verification_of_its_own() {
    let previous = previous_retained_playable("previous-retained-playable-with-succeeded-evidence");
    // The previous publication's own run succeeded. A naive read of the
    // assembled tree's retained `verification.json` (rather than what the
    // *current* run itself produced) would see exactly this document and
    // wrongly credit it to the current run.
    fs::write(previous.path().join("verification.json"), GREEN_PROJECTION).unwrap();
    let current = fixture_site(
        // No verification.json at all: this run attempted no verification of
        // its own (`EvidencePublication::Absent`), distinct from a run whose
        // own verification failed.
        "current-failed-no-verification-attempted",
        &[("index.html", PENDING_CURRENT_PAGE)],
    );
    let output = TempDirectory::new("no-verification-attempted-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FailedRetainLastGreen);
    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(
        index.contains(r#"<iframe class="play-embed" src="play/index.html""#),
        "the retained package must still be embedded: {index}"
    );
    assert!(
        index.contains("current run did not verify"),
        "a run that attempted no verification of its own must not claim success: {index}"
    );
    assert!(
        !index.contains("verified separately"),
        "the previous run's own retained success must never be credited to a current run \
         that attempted no verification at all: {index}"
    );
}

#[test]
fn incomplete_successful_current_evidence_is_refused() {
    let previous = previous_retained_playable("previous-retained-playable-malformed-evidence");
    let current = fixture_site(
        "current-malformed-verification-summary",
        &[
            ("index.html", PENDING_CURRENT_PAGE),
            ("verification.json", r#"{"succeeded":true}"#),
        ],
    );
    let output = TempDirectory::new("malformed-current-evidence-output");

    let result = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    );

    assert!(
        matches!(&result, Err(SitegenError::Json { .. })),
        "{result:?}"
    );
}

#[test]
fn a_missing_mode_marker_leaves_the_play_panel_unreconciled_too() {
    let previous = previous_retained_playable("previous-retained-playable-for-atomicity");
    // The play marker is present and reconcilable, but the mode marker is
    // absent entirely (a malformed or hand-edited page). Reconciliation must
    // treat the two markers as one atomic unit: either both are replaced, or
    // neither is, never an embedded iframe beside a badge no one updated.
    let current = fixture_site(
        "current-failed-missing-mode-marker",
        &[(
            "index.html",
            r#"<!doctype html><html><head><title>Hub</title></head><body><main>
<!--play--><div class="play-frame" role="img" aria-label="pending"><div class="empty-state play-empty"><h2>No verified playable build yet</h2></div></div><!--/play-->
<div class="hero-badge"><strong>Status</strong><small>Game pending verification</small></div>
</main></body></html>"#,
        )],
    );
    let output = TempDirectory::new("missing-mode-marker-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(
        !index.contains("play-embed"),
        "the play panel must never be reconciled without its paired mode badge: {index}"
    );
    assert!(
        index.contains("No verified playable build yet"),
        "an unreconciled pair must leave the original pending panel intact: {index}"
    );
}

// ---------------------------------------------------------------------------
// The hub iframe path itself is validated after assembly, not only the
// anchor links beside it.
// ---------------------------------------------------------------------------

#[test]
fn a_broken_iframe_target_is_refused_after_assembly() {
    let mut current_files = vec![(
        "index.html",
        r#"<!doctype html><html><head><title>Hub</title></head><body><main><iframe class="play-embed" src="play/missing.html" title="game"></iframe></main></body></html>"#,
    )];
    current_files.extend_from_slice(COMPLETE_PACKAGE);
    let current = fixture_site("current-broken-iframe", &current_files);
    let output = TempDirectory::new("broken-iframe-output");

    let result = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        CurrentPublication::Generated,
        output.path(),
    );

    assert!(
        matches!(&result, Err(SitegenError::BrokenLocalLink { .. })),
        "{result:?}"
    );
}

// ---------------------------------------------------------------------------
// The public assembly API must never leak a game a disposition promises it
// never publishes, and must never assume promoted evidence is atomic.
// ---------------------------------------------------------------------------

#[test]
fn first_run_status_only_never_publishes_an_inconsistent_current_playable_package() {
    let mut current_files = vec![("index.html", "CURRENT SOURCE: FAILED")];
    current_files.extend_from_slice(COMPLETE_PACKAGE);
    let current = fixture_site("current-inconsistent-first-run", &current_files);
    let output = TempDirectory::new("first-run-inconsistent-output");

    let disposition = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Failed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FirstRunStatusOnly);
    assert!(!output.path().join("play").exists());
    assert!(!output.path().join("last-green.json").exists());
}

#[test]
fn promoted_frames_without_a_gallery_manifest_are_refused_as_partial_evidence() {
    let current = fixture_site(
        "current-partial-evidence",
        &[
            ("index.html", "CURRENT SOURCE: GREEN"),
            ("screenshots/current/01-healthy-center-ne.png", "frame"),
        ],
    );
    let output = TempDirectory::new("partial-evidence-output");

    let result = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    );

    assert!(
        matches!(
            &result,
            Err(SitegenError::PartialEvidencePublication { .. })
        ),
        "{result:?}"
    );
}

#[test]
fn successful_projection_without_promoted_artifacts_is_refused_as_partial_evidence() {
    let current = fixture_site(
        "current-successful-projection-only",
        &[
            ("index.html", "CURRENT SOURCE: GREEN"),
            ("verification.json", GREEN_PROJECTION),
        ],
    );
    let output = TempDirectory::new("successful-projection-only-output");

    let result = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    );

    assert!(
        matches!(
            &result,
            Err(SitegenError::PartialEvidencePublication { .. })
        ),
        "{result:?}"
    );
}

#[test]
fn promoted_artifacts_without_a_projection_are_refused_as_partial_evidence() {
    let current = fixture_site(
        "current-promoted-artifacts-without-projection",
        &[
            ("index.html", "CURRENT SOURCE: GREEN"),
            ("gallery.json", PRIOR_GALLERY),
            (CURRENT_FRAME, "frame"),
        ],
    );
    let output = TempDirectory::new("promoted-artifacts-without-projection-output");

    let result = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    );

    assert!(
        matches!(
            &result,
            Err(SitegenError::PartialEvidencePublication { .. })
        ),
        "{result:?}"
    );
}

#[test]
fn promoted_artifacts_with_an_incomplete_projection_are_refused() {
    let current = fixture_site(
        "current-promoted-artifacts-with-incomplete-projection",
        &[
            ("index.html", "CURRENT SOURCE: GREEN"),
            ("gallery.json", r#"{"entries":[]}"#),
            ("verification.json", r#"{"succeeded":true}"#),
            (CURRENT_FRAME, "frame"),
        ],
    );
    let output = TempDirectory::new("promoted-artifacts-with-incomplete-projection-output");

    let result = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    );

    assert!(
        matches!(&result, Err(SitegenError::Json { .. })),
        "{result:?}"
    );
}

/// `assemble` reads nothing out of a repository, so the workflow hands it
/// none — and the workflow may not grow an option for one. It still may not
/// publish into a source tree, and a binary run from anywhere but its own
/// build directory cannot learn where that tree is from the path it was
/// compiled in. The checkout is therefore discovered at run time: the
/// workspace the runner exported, or the checkout the working directory sits
/// inside.
#[test]
fn a_relocated_assemble_refuses_to_publish_into_the_checkout_it_runs_in() {
    let checkout = fixture_site(
        "relocated-checkout",
        &[
            (".git/HEAD", "ref: refs/heads/main\n"),
            ("docs/plan.md", "#"),
        ],
    );
    let current = fixture_site(
        "relocated-current",
        &[("index.html", "CURRENT SOURCE: PASSED; WEB NOT RUN")],
    );
    let result_path = fixture_root().join("pages/native-passed-web-skipped.json");
    let assemble = |working_directory: &Path, workspace: Option<&Path>, output: PathBuf| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sitegen"));
        command.current_dir(working_directory).args([
            "assemble",
            "--current",
            current.path().to_str().unwrap(),
            "--result",
            result_path.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ]);
        match workspace {
            Some(workspace) => command.env("GITHUB_WORKSPACE", workspace),
            None => command.env_remove("GITHUB_WORKSPACE"),
        };
        command.output().expect("sitegen should launch")
    };

    let from_inside = assemble(checkout.path(), None, checkout.path().join("docs/site"));
    assert_eq!(
        from_inside.status.code(),
        Some(1),
        "the checkout the command runs inside is a source tree: {}",
        String::from_utf8_lossy(&from_inside.stdout)
    );
    assert!(
        String::from_utf8_lossy(&from_inside.stderr).contains("refusing unsafe output path"),
        "{}",
        String::from_utf8_lossy(&from_inside.stderr)
    );

    let exported = assemble(
        current.path(),
        Some(checkout.path()),
        checkout.path().join("docs/exported"),
    );
    assert_eq!(
        exported.status.code(),
        Some(1),
        "the exported workspace is a source tree wherever the command runs: {}",
        String::from_utf8_lossy(&exported.stdout)
    );

    // Nothing else could have refused those two: with neither the working
    // directory nor an exported workspace inside it, this tree is just a
    // directory, and the compiled-in checkout says nothing about it.
    let unknown = assemble(
        current.path(),
        None,
        checkout.path().join("docs/unprotected"),
    );
    assert_eq!(
        unknown.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );

    // The discovered checkout still publishes from its own build root, which
    // is what a relocated run really does.
    let build_root = assemble(checkout.path(), None, checkout.path().join("target/pages"));
    assert_eq!(
        build_root.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&build_root.stderr)
    );
    assert!(checkout.path().join("target/pages/index.html").is_file());
}

/// The checkout is *discovered*, so an exported `GITHUB_WORKSPACE` is a hint,
/// and an empty, relative, missing, or non-directory one is no hint at all: it
/// must fall through to discovery rather than replace it.
///
/// The empty string is the dangerous shape. It names no directory, but its
/// `.git` probe resolves relative to the working directory, so a run standing
/// inside a checkout "found" a workspace of `""`, stopped looking, and lost
/// the very source tree it was standing in — leaving the compiled-in path as
/// the only protected root, which on a runner is not on the machine at all.
#[test]
fn an_unusable_workspace_hint_cannot_suppress_checkout_discovery() {
    let checkout = fixture_site(
        "workspace-hint-checkout",
        &[
            (".git/HEAD", "ref: refs/heads/main\n"),
            ("docs/plan.md", "#"),
            ("notes.txt", "a file, not a workspace"),
        ],
    );
    let current = fixture_site(
        "workspace-hint-current",
        &[("index.html", "CURRENT SOURCE: PASSED; WEB NOT RUN")],
    );
    let result_path = fixture_root().join("pages/native-passed-web-skipped.json");

    for (case, workspace) in [
        ("empty", String::new()),
        ("relative", ".".to_owned()),
        ("relative below the checkout", "docs".to_owned()),
        ("missing", "/midcreek-no-such-exported-workspace".to_owned()),
        (
            "not a directory",
            checkout.path().join("notes.txt").display().to_string(),
        ),
    ] {
        let output = checkout
            .path()
            .join(format!("docs/site-{}", case.replace(' ', "-")));
        let finished = Command::new(env!("CARGO_BIN_EXE_sitegen"))
            .current_dir(checkout.path())
            .env("GITHUB_WORKSPACE", &workspace)
            .args([
                "assemble",
                "--current",
                current.path().to_str().unwrap(),
                "--result",
                result_path.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])
            .output()
            .expect("sitegen should launch");

        assert_eq!(
            finished.status.code(),
            Some(1),
            "an {case} workspace must not lose the checkout the run stands in: {} {}",
            String::from_utf8_lossy(&finished.stdout),
            String::from_utf8_lossy(&finished.stderr)
        );
        assert!(
            String::from_utf8_lossy(&finished.stderr).contains("refusing unsafe output path"),
            "{}",
            String::from_utf8_lossy(&finished.stderr)
        );
        assert!(
            !output.exists(),
            "nothing may be published into the discovered checkout"
        );
    }
}

#[test]
fn a_stale_workspace_hint_cannot_override_the_checkout_containing_the_command() {
    let active = fixture_site(
        "active-workspace-checkout",
        &[
            (".git/HEAD", "ref: refs/heads/main\n"),
            ("docs/plan.md", "#"),
        ],
    );
    let stale = fixture_site(
        "stale-workspace-checkout",
        &[
            (".git/HEAD", "ref: refs/heads/main\n"),
            ("docs/plan.md", "#"),
        ],
    );
    let current = fixture_site(
        "stale-workspace-current",
        &[("index.html", "CURRENT SOURCE: PASSED; WEB NOT RUN")],
    );
    let output = active.path().join("docs/site");
    let result_path = fixture_root().join("pages/native-passed-web-skipped.json");

    let finished = Command::new(env!("CARGO_BIN_EXE_sitegen"))
        .current_dir(active.path())
        .env("GITHUB_WORKSPACE", stale.path())
        .args([
            "assemble",
            "--current",
            current.path().to_str().unwrap(),
            "--result",
            result_path.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("sitegen should launch");

    assert_eq!(
        finished.status.code(),
        Some(1),
        "the active checkout must take precedence over a stale workspace hint: {} {}",
        String::from_utf8_lossy(&finished.stdout),
        String::from_utf8_lossy(&finished.stderr)
    );
    assert!(
        !output.exists(),
        "nothing may be published into the active checkout"
    );
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

/// A symbolic link at the top of the current tree has never been copied. A
/// link buried three directories down reaches the same `copy_artifact` only
/// through the recursion, so the containment rule has to be proved where it is
/// actually easy to lose: nested, below a directory that is itself perfectly
/// ordinary.
#[test]
fn a_nested_symlink_inside_the_current_tree_is_refused_rather_than_followed() {
    let outside = fixture_site("nested-symlink-target", &[("secret.txt", "not ours")]);
    let current = fixture_site(
        "current-nested-symlink",
        &[
            ("index.html", "CURRENT SOURCE: GREEN"),
            ("evidence/deep/real.txt", "published"),
        ],
    );
    let link = current.path().join("evidence/deep/secret.txt");
    symlink_file(&outside.path().join("secret.txt"), &link)
        .expect("symbolic link support is required for containment contracts");
    assert_eq!(
        fs::read_to_string(&link).unwrap(),
        "not ours",
        "the link really resolves, so only the containment rule can refuse it"
    );
    let output = TempDirectory::new("nested-symlink-output");

    let result = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    );

    assert!(
        matches!(&result, Err(SitegenError::UnsafeOutputPath { path }) if path == &link),
        "{result:?}"
    );
    assert!(
        !output.path().join("evidence/deep/secret.txt").exists(),
        "a refused link must never reach the published tree"
    );
}

/// A history entry may only name images inside its own commit's directory.
/// A path that merely starts with the history prefix can point at another
/// entry's pixels, and every check that follows — the link checker and the
/// retained-history rule — would be satisfied by a file that belongs to a
/// different point in time.
#[test]
fn a_history_frame_that_points_into_another_entrys_directory_is_refused() {
    let previous = fixture_site(
        "previous-scoped-history",
        &[
            ("index.html", "PREVIOUS SOURCE: GREEN"),
            ("gallery.json", PRIOR_GALLERY),
            (OLD_HISTORY, "old history"),
        ],
    );
    // The second entry belongs to commit 1111..., but its frame names the
    // image the 2222... entry published. The file is really there.
    let crossed = PRIOR_GALLERY.replace(
        r#"{"entries":["#,
        &format!(
            r#"{{"entries":[{{"semantic_visual_hash":"bbbbbbbb","source_commit":"1111111111111111111111111111111111111111","committed_at":"2026-08-30T00:00:00Z","current_task":"pages-status-always","frames":{{"center":"{OLD_HISTORY}"}},"metrics":{{}},"metric_deltas":{{}}}},"#
        ),
    );
    let current = fixture_site(
        "current-crossed-history",
        &[
            ("index.html", &index_linking(&[OLD_HISTORY])),
            ("gallery.json", &crossed),
            ("verification.json", GREEN_PROJECTION),
            (CURRENT_FRAME, "current frame"),
        ],
    );
    let output = TempDirectory::new("crossed-history-output");

    let error = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .expect_err("an entry may not publish another entry's pixels as its own");

    match &error {
        SitegenError::HistoryFrameOutsideEntry { frames } => {
            assert_eq!(frames, &vec![OLD_HISTORY.to_owned()]);
        }
        other => panic!("expected a misscoped-history failure, got {other}"),
    }
    assert!(error.to_string().contains(OLD_HISTORY), "{error}");
}

#[test]
fn a_history_frame_that_points_outside_history_is_refused() {
    let gallery = PRIOR_GALLERY.replace(OLD_HISTORY, CURRENT_FRAME);
    let current = fixture_site(
        "current-history-pointing-at-current",
        &[
            ("index.html", &index_linking(&[CURRENT_FRAME])),
            ("gallery.json", &gallery),
            ("verification.json", GREEN_PROJECTION),
            (CURRENT_FRAME, "current frame"),
        ],
    );
    let output = TempDirectory::new("history-pointing-at-current-output");

    let error = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::SkippedDependency),
        CurrentPublication::Generated,
        output.path(),
    )
    .expect_err("a history entry may not publish current pixels as its own");

    assert!(
        matches!(
            &error,
            SitegenError::HistoryFrameOutsideEntry { frames }
                if frames == &vec![CURRENT_FRAME.to_owned()]
        ),
        "{error:?}"
    );
}

#[test]
fn assemble_cli_refuses_a_publication_kind_it_does_not_recognise() {
    let current = fixture_site(
        "current-unknown-publication",
        &[("index.html", "CURRENT SOURCE: PASSED; WEB NOT RUN")],
    );
    let output = TempDirectory::new("unknown-publication-output");
    let result_path = fixture_root().join("pages/native-passed-web-skipped.json");

    let command = Command::new(env!("CARGO_BIN_EXE_sitegen"))
        .args([
            "assemble",
            "--current",
            current.path().to_str().unwrap(),
            "--result",
            result_path.to_str().unwrap(),
            "--publication",
            "green",
            "--output",
            output.path().to_str().unwrap(),
        ])
        .output()
        .expect("sitegen should launch");

    assert_eq!(command.status.code(), Some(2));
    let stderr = String::from_utf8(command.stderr).unwrap();
    assert!(
        stderr.contains("--publication must be generated or degraded"),
        "{stderr}"
    );
    assert!(!output.path().join("index.html").exists());
}

#[test]
fn assemble_cli_publishes_a_degraded_tree_over_a_retained_game() {
    let previous = fixture_site(
        "previous-green-degraded-cli",
        &[
            ("play/index.html", "old shell"),
            ("play/game_bg.wasm", "last-known-good-game"),
        ],
    );
    let current = fixture_site("current-degraded-cli", &DEGRADED_STATUS_PAGE);
    let output = TempDirectory::new("degraded-cli-output");
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
            "--publication",
            "degraded",
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
    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
    fn builds_the_web_game_after_verify_even_when_verification_fails() {
        let workflow = workflow_source();
        let web = web_job(&workflow);

        assert!(web.contains("needs: verify"), "{web}");
        assert!(web.contains("\n    if: always()\n"), "{web}");
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
            "Workflow lint",
            "Published progress data",
            // Composes the degraded publication fallback with the real
            // assembler, so it is measured here rather than with the cheap
            // gates: it needs the `sitegen` the gate above leaves built.
            "History bound tests",
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
            verify.contains(r#"echo "RESULT=$RUNNER_TEMP/native" >> "$GITHUB_ENV""#),
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

    // -----------------------------------------------------------------------
    // Workflow semantic linting
    // -----------------------------------------------------------------------

    /// GitHub evaluates a job-level `env:` before a runner exists, so a
    /// `runner` expression there is not a value that resolves late — it is a
    /// workflow the API refuses to start. No gate in this repository can run
    /// if the run never begins, so the rule is asserted directly.
    #[test]
    fn no_job_level_env_names_the_runner_context() {
        let workflow = workflow_source();

        // The checked-in workflow declares no job-level `env:` at all, so a
        // loop over what the detector finds in it would pass by finding
        // nothing. The detector is therefore proved able to fail first, on the
        // exact shape this rule exists to refuse.
        let offending = concat!(
            "jobs:\n",
            "  verify:\n",
            "    runs-on: ubuntu-latest\n",
            "    env:\n",
            "      RESULT: ${{ runner.temp }}/native\n",
            "    steps:\n",
            "      - run: echo \"$RESULT\"\n",
        );
        assert_eq!(
            job_level_env_blocks(offending),
            vec!["      RESULT: ${{ runner.temp }}/native"],
            "the detector has to find a job-level env: before its verdict on \
             this workflow means anything"
        );

        for block in job_level_env_blocks(&workflow) {
            assert!(
                !block.contains("runner."),
                "a job-level env: may not name the runner context: {block}"
            );
        }
        assert!(
            !workflow.contains("RESULT: ${{ runner.temp }}"),
            "the result root must be resolved from $RUNNER_TEMP in a step"
        );
        assert!(
            !workflow.contains("GATES: ${{ runner.temp }}"),
            "the gate file must be resolved from $RUNNER_TEMP in a step"
        );

        for (job, root) in [
            (verify_job(&workflow), "native"),
            (web_job(&workflow), "web"),
        ] {
            let resolve = step(job, &format!("Resolve the {root} result root"));
            assert!(
                resolve.contains(&format!(r#"echo "RESULT=$RUNNER_TEMP/{root}""#)),
                "{resolve}"
            );
            assert!(
                resolve.contains(&format!(r#"echo "GATES=$RUNNER_TEMP/{root}/gates.jsonl""#)),
                "{resolve}"
            );
            assert!(resolve.contains(r#">> "$GITHUB_ENV""#), "{resolve}");
        }
    }

    /// The rule above is GitHub's, not this repository's, so it is proved with
    /// GitHub's own rules rather than with a string match: the pinned linter
    /// has to accept the workflow as it stands and reject the whole workflow
    /// in exactly the shape it really had.
    ///
    /// The invalid file is rebuilt from the current one rather than read out
    /// of Git history, because a shallow checkout — what CI does by default —
    /// has no history to read and a gate that quietly stops running is worse
    /// than one that never existed.
    #[test]
    fn the_pinned_linter_accepts_this_workflow_and_rejects_the_shape_it_replaced() {
        let workflow = workflow_source();
        let clean = run_actionlint(&[repository()
            .join(".github/workflows/pages.yml")
            .to_string_lossy()
            .into_owned()]);
        assert!(
            clean.status.success(),
            "the checked-in workflow should lint clean:\n{}{}",
            String::from_utf8_lossy(&clean.stdout),
            String::from_utf8_lossy(&clean.stderr)
        );

        let regressed = with_historical_job_level_env(&workflow);
        assert_eq!(
            job_level_env_blocks(&regressed).len(),
            2,
            "both jobs carried the defect, so both have to be rebuilt: {regressed}"
        );
        let directory = TempDirectory::new("actionlint-regression");
        let regression = directory.path().join("regression.yml");
        fs::write(&regression, &regressed).expect("the regression workflow should be writable");

        let dirty = run_actionlint(&[regression.to_string_lossy().into_owned()]);
        let report = format!(
            "{}{}",
            String::from_utf8_lossy(&dirty.stdout),
            String::from_utf8_lossy(&dirty.stderr)
        );
        assert!(
            !dirty.status.success(),
            "a job-level runner context must fail the gate: {report}"
        );
        assert_eq!(
            report
                .matches(r#"context "runner" is not allowed here"#)
                .count(),
            4,
            "every job-level runner expression the workflow really carried has \
             to be named: {report}"
        );
    }

    /// The reduced shape is kept beside the full one. It is written from
    /// scratch rather than rebuilt from the checked-in workflow, so it keeps
    /// proving that the pinned linter refuses a job-level runner context even
    /// if this repository's workflow is one day restructured so far that the
    /// historical shape can no longer be reconstructed from it.
    #[test]
    fn the_pinned_linter_rejects_a_minimal_job_level_runner_context() {
        let directory = TempDirectory::new("actionlint-minimal");
        let minimal = directory.path().join("minimal.yml");
        fs::write(
            &minimal,
            concat!(
                "name: Regression\n",
                "on:\n",
                "  push:\n",
                "    branches: [main]\n",
                "jobs:\n",
                "  verify:\n",
                "    runs-on: ubuntu-latest\n",
                "    env:\n",
                "      RESULT: ${{ runner.temp }}/native\n",
                "    steps:\n",
                "      - run: echo \"$RESULT\"\n",
            ),
        )
        .expect("the regression workflow should be writable");

        let dirty = run_actionlint(&[minimal.to_string_lossy().into_owned()]);
        let report = format!(
            "{}{}",
            String::from_utf8_lossy(&dirty.stdout),
            String::from_utf8_lossy(&dirty.stderr)
        );

        assert!(
            !dirty.status.success(),
            "a job-level runner context must fail the gate: {report}"
        );
        assert!(
            report.contains(r#"context "runner" is not allowed here"#),
            "{report}"
        );
    }

    /// The Pages workflow as it stands, with the job-level `env:` each gated
    /// job really carried before the fix put back exactly where it was: after
    /// the job's permissions and before its steps.
    fn with_historical_job_level_env(workflow: &str) -> String {
        let mut regressed = workflow.to_owned();
        for (job, root) in [("build-web", "web"), ("verify", "native")] {
            let job_start = regressed
                .find(&format!("\n  {job}:\n"))
                .unwrap_or_else(|| panic!("the workflow should declare the {job} job"));
            let steps = regressed[job_start..]
                .find("\n    steps:\n")
                .map(|offset| job_start + offset + 1)
                .unwrap_or_else(|| panic!("the {job} job should declare steps"));
            regressed.insert_str(
                steps,
                &format!(
                    "    env:\n      RESULT: ${{{{ runner.temp }}}}/{root}\n      \
                     GATES: ${{{{ runner.temp }}}}/{root}/gates.jsonl\n"
                ),
            );
        }
        regressed
    }

    /// A linter that changes its rules under the gate is a gate that changes
    /// its verdict, so the version is pinned, the cache lives inside this
    /// repository, and the binary is checked against the pin before it runs.
    #[test]
    fn the_workflow_linter_is_pinned_cached_and_verified_before_it_is_trusted() {
        let script = fs::read_to_string(repository().join("scripts/actionlint.sh"))
            .expect("the workflow linter should be checked in");

        assert!(
            script.contains(r#"ACTIONLINT_VERSION="1.7.7""#),
            "the linter version should be pinned: {script}"
        );
        assert!(
            script.contains(r#"actionlint@v$ACTIONLINT_VERSION"#),
            "the install should request exactly the pinned version: {script}"
        );
        assert!(
            script.contains(r#"^[0-9]+(\.[0-9]+)*$"#),
            "the version must be checked before it becomes a path: {script}"
        );
        assert!(
            script.contains(r#"cache="$tools/actionlint/$ACTIONLINT_VERSION""#),
            "the cache should be versioned: {script}"
        );
        assert!(
            script.contains(r#"tools="$repository/target/tools""#),
            "the cache should live inside this repository: {script}"
        );
        assert!(
            script.contains(r#"refusing a tools cache outside the repository"#),
            "a redirected cache should be refused: {script}"
        );
        assert!(
            script.contains(r#"this gate is pinned to v$ACTIONLINT_VERSION"#),
            "the binary should be verified before it is trusted: {script}"
        );
        assert!(
            script.contains("sed 's/^v//'"),
            "official release binaries omit the optional version prefix: {script}"
        );
        assert!(
            script.contains(r#"-ignore 'unknown permission scope "copilot-requests"'"#),
            "only the valid permission missing from actionlint may be ignored: {script}"
        );

        let check = fs::read_to_string(repository().join("scripts/check.sh"))
            .expect("the clean-push gate should be checked in");
        let lint = check
            .find("./scripts/actionlint.sh")
            .expect("the clean-push gate should lint the workflows");
        let clippy = check
            .find("cargo clippy")
            .expect("the clean-push gate should run Clippy");
        assert!(
            lint < clippy,
            "the workflow lint should run before the expensive gates: {check}"
        );
    }

    /// The lint is a named row on the published site like every other gate, so
    /// it is measured by the recording runner and reported even when a later
    /// gate fails.
    #[test]
    fn ci_measures_the_workflow_lint_as_its_first_named_gate() {
        let workflow = workflow_source();
        let gates = step(verify_job(&workflow), "Run the native gates");

        assert!(
            gates.contains(r#"run-gate.sh "$GATES" "Workflow lint" --"#),
            "{gates}"
        );
        let lint = gates
            .find(r#""Workflow lint" --"#)
            .expect("the lint should be a named gate");
        assert!(
            gates[lint..]
                .lines()
                .nth(1)
                .is_some_and(|command| command.contains("./scripts/actionlint.sh")),
            "{gates}"
        );
        assert!(
            gates[..lint].matches("run-gate.sh").count() == 1,
            "the workflow lint should be the first measured gate: {gates}"
        );
    }

    fn run_actionlint(targets: &[String]) -> std::process::Output {
        Command::new(bash_command())
            .arg(repository().join("scripts/actionlint.sh"))
            .args(targets)
            .current_dir(repository())
            .output()
            .expect("the workflow linter should be runnable")
    }

    /// Every `env:` mapping declared directly on a job, at the indent GitHub
    /// evaluates before a runner exists.
    fn job_level_env_blocks(workflow: &str) -> Vec<&str> {
        let mut blocks = Vec::new();
        let mut rest = workflow;
        while let Some(offset) = rest.find("\n    env:\n") {
            let body = &rest[offset + "\n    env:\n".len()..];
            let end = body
                .find("\n    ")
                .or_else(|| body.find("\n  "))
                .unwrap_or(body.len());
            blocks.push(&body[..end]);
            rest = &body[end..];
        }
        blocks
    }

    fn workflow_source() -> String {
        fs::read_to_string(repository().join(".github/workflows/pages.yml"))
            .expect("Pages workflow should be checked in")
            .replace("\r\n", "\n")
            .replace('\r', "\n")
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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

/// A degraded current tree is a status page the site generator did not write.
/// It is the shape `scripts/history_failure_site.py` publishes when Publish
/// cannot verify the history: one `index.html`, no game, no evidence.
const DEGRADED_STATUS_PAGE: [(&str, &str); 1] = [(
    "index.html",
    "<!doctype html><html><body><main><h1>Publication history unavailable</h1></main></body></html>",
)];

#[test]
fn a_green_run_publishing_a_degraded_page_retains_the_previous_game() {
    // The defect this guards: a status page passed off as a replacement.
    //
    // A green run whose history could not be verified never built a site, so
    // there is no new game to promote and nothing to replace the last verified
    // one with. Refusing the whole publication would take the run's own status
    // down with it; treating the page as a replacement would throw a verified
    // game away for a page that never had one. Retention is the only answer
    // left that is true.
    let previous = fixture_site(
        "previous-green-degraded",
        &[
            ("index.html", "PREVIOUS SOURCE: GREEN"),
            ("play/index.html", "old shell"),
            ("play/game_bg.wasm", "last-known-good-game"),
            ("last-green.json", r#"{"source_commit":"old"}"#),
        ],
    );
    let current = fixture_site("current-degraded-green", &DEGRADED_STATUS_PAGE);
    let output = TempDirectory::new("degraded-green-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        CurrentPublication::Degraded,
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::RetainLastGreen);
    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("last-green.json")).unwrap(),
        r#"{"source_commit":"old"}"#
    );
    let index = fs::read_to_string(output.path().join("index.html")).unwrap();
    assert!(index.contains("Publication history unavailable"));
    assert!(!index.contains("PREVIOUS SOURCE: GREEN"));
}

#[test]
fn a_degraded_page_retains_the_previous_game_whatever_the_run_did() {
    for (native, web) in [
        (GateStatus::Passed, GateStatus::Passed),
        (GateStatus::Passed, GateStatus::SkippedDependency),
        (GateStatus::SkippedDependency, GateStatus::SkippedDependency),
    ] {
        let previous = fixture_site(
            "previous-degraded-matrix",
            &[
                ("play/index.html", "old shell"),
                ("play/game_bg.wasm", "last-known-good-game"),
            ],
        );
        let current = fixture_site("current-degraded-matrix", &DEGRADED_STATUS_PAGE);
        let output = TempDirectory::new("degraded-matrix-output");

        let disposition = assemble_site(
            Some(previous.path()),
            current.path(),
            &workflow_summary(native, web),
            CurrentPublication::Degraded,
            output.path(),
        )
        .unwrap();

        assert_eq!(disposition, BuildDisposition::RetainLastGreen, "{native:?}");
        assert_eq!(
            fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
            "last-known-good-game"
        );
    }
}

#[test]
fn a_failed_run_publishing_a_degraded_page_still_retains_the_previous_game() {
    let previous = fixture_site(
        "previous-degraded-failed",
        &[
            ("play/index.html", "old shell"),
            ("play/game_bg.wasm", "last-known-good-game"),
        ],
    );
    let current = fixture_site("current-degraded-failed", &DEGRADED_STATUS_PAGE);
    let output = TempDirectory::new("degraded-failed-output");

    let disposition = assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Failed),
        CurrentPublication::Degraded,
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FailedRetainLastGreen);
    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
    );
}

#[test]
fn a_degraded_page_with_no_previous_publication_is_status_only() {
    let current = fixture_site("current-degraded-first-run", &DEGRADED_STATUS_PAGE);
    let output = TempDirectory::new("degraded-first-run-output");

    let disposition = assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        CurrentPublication::Degraded,
        output.path(),
    )
    .unwrap();

    assert_eq!(disposition, BuildDisposition::FirstRunStatusOnly);
    assert!(!output.path().join("play").exists());
    assert!(!output.path().join("last-green.json").exists());
}

#[test]
fn a_degraded_page_never_promotes_a_game_it_carries_by_accident() {
    // Degradation reduces what a tree may claim; it never grants it a game.
    // Anything under `play/` in a degraded current tree is protected out of
    // the copy exactly as it is for any other retaining disposition, so the
    // published game stays the one the previous publication earned.
    let previous = fixture_site(
        "previous-degraded-protected",
        &[
            ("play/index.html", "old shell"),
            ("play/game_bg.wasm", "last-known-good-game"),
        ],
    );
    let current = fixture_site(
        "current-degraded-smuggled",
        &[
            (
                "index.html",
                "<!doctype html><html><body><main><h1>Status</h1></main></body></html>",
            ),
            ("play/game_bg.wasm", "smuggled-game"),
        ],
    );
    let output = TempDirectory::new("degraded-protected-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        CurrentPublication::Degraded,
        output.path(),
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
    );
}

#[test]
fn a_degraded_publication_retains_previous_game_and_evidence_artifacts() {
    let previous = previous_with_evidence(
        "previous-degraded-evidence",
        &index_linking(&[OLD_HISTORY, CURRENT_FRAME]),
    );
    let previous_projection = GREEN_PROJECTION.replace("bbbbbbbb", "aaaaaaaa");
    fs::write(
        previous.path().join("verification.json"),
        &previous_projection,
    )
    .unwrap();
    let current = fixture_site(
        "current-degraded-evidence",
        &[
            (
                "index.html",
                "<!doctype html><html><body><main><h1>Status</h1></main></body></html>",
            ),
            ("play/game_bg.wasm", "smuggled-game"),
            ("last-green.json", r#"{"source_commit":"smuggled"}"#),
            ("gallery.json", TWO_POINT_GALLERY),
            ("verification.json", GREEN_PROJECTION),
            (CURRENT_FRAME, "smuggled current"),
            (OLD_HISTORY, "smuggled old history"),
            (NEW_HISTORY, "smuggled new history"),
        ],
    );
    let output = TempDirectory::new("degraded-evidence-output");

    assemble_site(
        Some(previous.path()),
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        CurrentPublication::Degraded,
        output.path(),
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(output.path().join("play/game_bg.wasm")).unwrap(),
        "last-known-good-game"
    );
    assert_eq!(
        fs::read_to_string(output.path().join("gallery.json")).unwrap(),
        PRIOR_GALLERY
    );
    assert_eq!(
        fs::read_to_string(output.path().join("verification.json")).unwrap(),
        previous_projection
    );
    assert_eq!(
        fs::read_to_string(output.path().join(CURRENT_FRAME)).unwrap(),
        "old current"
    );
    assert_eq!(
        fs::read_to_string(output.path().join(OLD_HISTORY)).unwrap(),
        "old history"
    );
    assert!(!output.path().join(NEW_HISTORY).exists());
    assert!(
        !fs::read_to_string(output.path().join("last-green.json"))
            .unwrap()
            .contains("smuggled")
    );
}

#[test]
fn a_first_degraded_publication_discards_game_and_evidence_artifacts() {
    let current = fixture_site(
        "current-first-degraded-evidence",
        &[
            (
                "index.html",
                "<!doctype html><html><body><main><h1>Status</h1></main></body></html>",
            ),
            ("play/game_bg.wasm", "smuggled-game"),
            ("last-green.json", r#"{"source_commit":"smuggled"}"#),
            ("gallery.json", r#"{"entries":[]}"#),
            ("verification.json", GREEN_PROJECTION),
            (CURRENT_FRAME, "smuggled current"),
        ],
    );
    let output = TempDirectory::new("first-degraded-evidence-output");

    assemble_site(
        None,
        current.path(),
        &workflow_summary(GateStatus::Passed, GateStatus::Passed),
        CurrentPublication::Degraded,
        output.path(),
    )
    .unwrap();

    assert!(output.path().join("index.html").is_file());
    assert!(!output.path().join("play").exists());
    assert!(!output.path().join("last-green.json").exists());
    assert!(!output.path().join("screenshots").exists());
    assert!(!output.path().join("gallery.json").exists());
    assert!(!output.path().join("verification.json").exists());
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
        CurrentPublication::Generated,
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
        ("verification.json", GREEN_PROJECTION),
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        ("verification.json", GREEN_PROJECTION),
        (
            "screenshots/current/01-healthy-center-ne.png",
            "new current",
        ),
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
        CurrentPublication::Generated,
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
const GREEN_PROJECTION: &str = r##"{
  "schema_version": 1,
  "succeeded": true,
  "failed_stage": null,
  "stages": [],
  "semantic_visual_hash": "bbbbbbbb",
  "camera": {
    "tonemapping": "TonyMcMapface",
    "deband_dither": "Enabled",
    "msaa_samples": 1,
    "clear_color": "#000000"
  },
  "hashes": {
    "assets": {},
    "asset_sources": {},
    "references": {},
    "sources": {}
  },
  "frames": [],
  "browser": null,
  "metrics": {},
  "metric_failures": [],
  "gates": []
}"##;

/// A failed projection: the current status, with no pixels behind it. It still
/// carries the run's semantic hash, exactly as `VerificationSummary` does.
const FAILED_PROJECTION: &str = r##"{
  "schema_version": 1,
  "succeeded": false,
  "failed_stage": "repair",
  "stages": [],
  "semantic_visual_hash": "cccccccc",
  "camera": {
    "tonemapping": "TonyMcMapface",
    "deband_dither": "Enabled",
    "msaa_samples": 1,
    "clear_color": "#000000"
  },
  "hashes": {
    "assets": {},
    "asset_sources": {},
    "references": {},
    "sources": {}
  },
  "frames": [],
  "browser": null,
  "metrics": {},
  "metric_failures": [],
  "gates": []
}"##;

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
        CurrentPublication::Generated,
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
            CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        CurrentPublication::Generated,
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
        Command::new(bash_command())
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

    /// Both jobs passed, but the browser gate's own canvas is unreadable. The
    /// native run proved everything it proved regardless, so publishing
    /// nothing at all would throw away fourteen verified frames because of one
    /// corrupt PNG from the other job.
    #[test]
    fn unprojectable_browser_evidence_never_costs_the_native_evidence_beside_it() {
        let run = Run::new("inputs-browser-unprojectable");
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
        fs::write(
            run.web_root().join("browser/canvas.png"),
            b"\x89PNG\r\n\x1a\nnot actually an image",
        )
        .unwrap();

        let (workflow, inputs) = run.execute("success", "success");

        assert!(
            inputs["verification"]["report"].is_string(),
            "the native evidence still projects on its own: {inputs}"
        );
        assert!(
            inputs["verification"]["browser"].is_null(),
            "the unusable browser evidence is dropped, not published: {inputs}"
        );
        assert_eq!(
            gate(&workflow, "Published browser evidence").status,
            GateStatus::Failed,
            "the gap is published rather than hidden"
        );
        assert!(
            !workflow
                .gates
                .iter()
                .any(|gate| gate.name == "Published verification evidence"),
            "the native evidence did not fail: {:?}",
            gate_names(&workflow)
        );
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

    /// `known_commits` exists to resolve the commits the published documents
    /// name. Enumerating the whole repository grows the published facts, and
    /// the work of collecting them, with every commit anybody ever pushes, so
    /// it is bounded by what is really published: the timeline, the head, and
    /// the commits the progress document actually references.
    #[test]
    fn the_published_repository_facts_are_bounded_by_what_they_have_to_resolve() {
        let run = Run::new("inputs-bounded-facts");
        run.native(
            GateStatus::Passed,
            &[("Clippy lints", GateStatus::Passed)],
            true,
        );
        run.execute("success", "skipped");

        let repo = read_value(&run.output().join("repo.json"));
        let known = repo["known_commits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        let timeline = repo["commits"].as_array().unwrap().len();
        let referenced = referenced_commits();
        let history = git(&["rev-list", "--all"]);
        let history = history.lines().collect::<Vec<_>>();

        assert!(
            known.len() <= timeline + referenced.len() + 1,
            "the published facts enumerate {} commits for a timeline of {timeline} and \
             {} referenced commits",
            known.len(),
            referenced.len()
        );
        assert!(
            known.contains(repo["head_sha"].as_str().unwrap()),
            "the head always resolves: {known:?}"
        );
        for commit in &referenced {
            assert!(
                known.contains(commit),
                "the progress document references {commit}, which must still resolve"
            );
        }

        // Whatever else this repository's history holds is not published.
        let recent = git(&["log", "--max-count", "20", "--format=%H"]);
        if let Some(old) = history.iter().find(|sha| {
            !recent.lines().any(|recent| recent == **sha) && !referenced.contains(**sha)
        }) {
            assert!(
                !known.contains(*old),
                "{old} is neither on the timeline nor referenced, so it must not be published"
            );
        }
    }

    /// Every commit `docs/progress.json` names, as a full SHA.
    fn referenced_commits() -> std::collections::BTreeSet<String> {
        let document =
            read_value(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/progress.json"));
        let tasks = document["tasks"].as_array().unwrap().iter();
        let challenges = document["challenges"].as_array().unwrap().iter();
        tasks
            .filter_map(|task| task["completed_commit"].as_str())
            .chain(challenges.filter_map(|challenge| challenge["resolved_commit"].as_str()))
            .filter(|commit| {
                commit.len() == 40 && commit.chars().all(|value| value.is_ascii_hexdigit())
            })
            .map(str::to_owned)
            .collect()
    }

    fn git(args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(env!("CARGO_MANIFEST_DIR"))
            .args(args)
            .output()
            .expect("git should run in the checkout");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).expect("git output should be UTF-8")
    }

    // -------------------------------------------------------------------
    // One `sitegen inputs` run and the artifacts it was handed
    // -------------------------------------------------------------------

    /// `inputs` publishes `docs/progress.json` and resolves exactly the
    /// commits that document names. A document that cannot be read, or that
    /// does not match the schema, is therefore not a run with no references:
    /// it is a run that cannot publish. Treating the two the same silently
    /// drops every reference the real document names and leaves the generator
    /// to fail later — or, worse, to publish a timeline that resolves nothing.
    #[test]
    fn an_unpublishable_progress_document_stops_the_run_that_would_publish_it() {
        for (case, document) in [
            ("missing", None),
            ("unreadable", Some("{ this is not JSON")),
            (
                "schema-invalid",
                Some(r#"{"schema_version":1,"project":"Cell Shift"}"#),
            ),
        ] {
            let repository = TempDirectory::new(&format!("inputs-progress-{case}"));
            initialize_checkout(repository.path());
            fs::create_dir_all(repository.path().join("docs")).unwrap();
            if let Some(document) = document {
                fs::write(repository.path().join("docs/progress.json"), document).unwrap();
            }
            let run = Run::new(&format!("inputs-progress-run-{case}"));

            let finished = run.launch(
                repository.path().to_str().unwrap(),
                repository.path(),
                "skipped",
                "skipped",
            );
            let stderr = String::from_utf8_lossy(&finished.stderr).into_owned();

            assert_eq!(
                finished.status.code(),
                Some(1),
                "a {case} progress document must stop the run: {stderr}"
            );
            assert!(
                stderr.contains("progress.json"),
                "the failure must name the document it could not publish: {stderr}"
            );
            assert!(
                !run.output().join("inputs.json").exists(),
                "nothing may be handed to the generator from a run that could not read \
                 the document it was going to publish"
            );
        }
    }

    /// A checkout with one commit, so that the only thing wrong with a
    /// repository under test is the thing the test put there.
    fn initialize_checkout(root: &Path) {
        for arguments in [
            vec!["init", "--quiet"],
            vec!["commit", "--quiet", "--allow-empty", "-m", "root"],
        ] {
            let finished = Command::new("git")
                .arg("-C")
                .arg(root)
                .args([
                    "-c",
                    "user.name=midcreek-tests",
                    "-c",
                    "user.email=tests@midcreek.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(&arguments)
                .output()
                .expect("git should run");
            assert!(
                finished.status.success(),
                "git {arguments:?}: {}",
                String::from_utf8_lossy(&finished.stderr)
            );
        }
    }

    /// `build` resolves the relative paths of an inputs document against the
    /// directory that document lives in — the output directory, which is not
    /// where a relative `--repository` was ever measured from. The repository
    /// is therefore resolved once, at inputs time, against the working
    /// directory it really meant. Publishing the whole real checkout through
    /// it proves the resolved document works on the documents this repository
    /// actually carries, not only on fixtures.
    #[test]
    fn a_relative_repository_publishes_the_real_documents_of_this_checkout() {
        let run = Run::new("inputs-relative-repository");
        run.native(
            GateStatus::Passed,
            &[("Clippy lints", GateStatus::Passed)],
            true,
        );

        let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let finished = run.launch(".", &checkout, "success", "skipped");
        assert_eq!(
            finished.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&finished.stderr)
        );

        let inputs = read_value(&run.output().join("inputs.json"));
        let canonical = fs::canonicalize(&checkout).unwrap();
        assert_eq!(
            Path::new(inputs["repository"].as_str().unwrap()),
            canonical,
            "the declared repository must not stay relative: {inputs}"
        );
        for document in ["progress", "plan", "reference_manifest"] {
            let path = Path::new(inputs[document].as_str().unwrap());
            assert!(
                path.starts_with(&canonical) && path.is_file(),
                "{document} must resolve from anywhere: {}",
                path.display()
            );
        }

        // The published page renders the real plan, the real progress
        // document, and real commit subjects, none of which any fixture
        // carries. `build` validates its own output, so a page that leaked a
        // path of this machine, or lost a link, fails right here.
        let site = run.root.path().join("site");
        let built = Command::new(env!("CARGO_BIN_EXE_sitegen"))
            .current_dir(run.root.path())
            .args([
                "build",
                "--inputs",
                run.output().join("inputs.json").to_str().unwrap(),
                "--output",
                site.to_str().unwrap(),
            ])
            .output()
            .expect("sitegen should launch");
        assert_eq!(
            built.status.code(),
            Some(0),
            "the real documents must publish: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        assert!(site.join("index.html").is_file());
        assert_eq!(
            String::from_utf8_lossy(&built.stdout).trim(),
            COMMIT,
            "the published source commit is the one the run declared"
        );

        // The page really rendered those documents rather than an empty
        // stand-in for them: a plan heading the real plan declares, and the
        // head commit of the real checkout.
        let page = fs::read_to_string(site.join("index.html")).unwrap();
        let head = git(&["rev-parse", "HEAD"]);
        assert!(page.contains(r#"id="plan-ci-baseline""#), "{page}");
        assert!(page.contains(&head[..8]), "{page}");
    }

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
            let finished = self.launch(
                env!("CARGO_MANIFEST_DIR"),
                Path::new(env!("CARGO_MANIFEST_DIR")),
                native_outcome,
                web_outcome,
            );
            assert_eq!(
                finished.status.code(),
                Some(0),
                "sitegen inputs should always publish: {}",
                String::from_utf8_lossy(&finished.stderr)
            );

            let output = self.output();
            let workflow = serde_json::from_str::<WorkflowSummary>(
                &fs::read_to_string(output.join("workflow.json")).unwrap(),
            )
            .expect("the merged result should match the published schema");
            (workflow, read_value(&output.join("inputs.json")))
        }

        /// One `sitegen inputs` run against a named repository argument, from
        /// a named working directory, whatever it exits with.
        fn launch(
            &self,
            repository: &str,
            working_directory: &Path,
            native_outcome: &str,
            web_outcome: &str,
        ) -> std::process::Output {
            let output = self.output();
            let mut command = Command::new(env!("CARGO_BIN_EXE_sitegen"));
            command.current_dir(working_directory).args([
                "inputs",
                "--repository",
                repository,
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
            command.output().expect("sitegen should launch")
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
        let passed = manifest(
            "verify",
            Some("verification"),
            &[("Rendered image contracts", None)],
        );
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

    #[test]
    fn a_passed_manifest_cannot_contain_a_failed_gate() {
        let mut result = manifest(
            "verify",
            Some("verification"),
            &[("Rendered image contracts", None)],
        );
        result.gates[0].status = GateStatus::Failed;
        result.gates[0].passed = 0;
        result.gates[0].failed = 1;

        assert!(
            validate_job_result(&result).is_err(),
            "a passed manifest must not contain a failed gate: {result:?}"
        );
    }

    /// A job that fell over before its first gate still uploads a manifest,
    /// and that manifest measured nothing. Reading it as a result would let a
    /// failed run publish a matrix with no row saying the job failed at all.
    /// It has to be read exactly like a manifest that never arrived.
    #[test]
    fn a_native_failure_before_its_first_gate_publishes_the_job_and_its_manifest() {
        let native = JobReport {
            outcome: JobOutcome::Failure,
            result: Some(JobResult {
                status: GateStatus::Failed,
                ..manifest("verify", None, &[])
            }),
        };
        let web = JobReport::absent(JobOutcome::Skipped);

        let merged = merge_job_results(
            "1111111111111111111111111111111111111111",
            "https://github.com/ridermw/midcreek-cs-1/actions/runs/1",
            &native,
            &web,
        )
        .expect("an empty manifest should publish, not crash");

        assert_eq!(
            merged
                .gates
                .iter()
                .map(|gate| (gate.name.as_str(), gate.status))
                .collect::<Vec<_>>(),
            vec![
                ("Native verification", GateStatus::Failed),
                ("Native verification result manifest", GateStatus::Failed),
                (
                    "Web package and browser gate",
                    GateStatus::SkippedDependency
                ),
            ],
        );
        assert_eq!(native.status(), GateStatus::Failed);
        assert_eq!(merged.native, GateStatus::Failed);
    }

    /// The web job resolves the preinstalled Chrome before its first gate, so
    /// a runner without Chrome fails exactly there: the job ran, uploaded an
    /// empty manifest, and measured nothing. The site still has to say so.
    #[test]
    fn a_web_failure_resolving_chrome_publishes_the_job_and_its_manifest() {
        let native = JobReport {
            outcome: JobOutcome::Success,
            result: Some(manifest(
                "verify",
                Some("verification"),
                &[("Clippy lints", None)],
            )),
        };
        let web = JobReport {
            outcome: JobOutcome::Failure,
            result: Some(JobResult {
                status: GateStatus::Failed,
                ..manifest("build-web", None, &[])
            }),
        };

        let merged = merge_job_results(
            "1111111111111111111111111111111111111111",
            "https://github.com/ridermw/midcreek-cs-1/actions/runs/1",
            &native,
            &web,
        )
        .expect("an empty web manifest should publish, not crash");

        assert_eq!(
            merged
                .gates
                .iter()
                .map(|gate| (gate.name.as_str(), gate.status))
                .collect::<Vec<_>>(),
            vec![
                ("Clippy lints", GateStatus::Passed),
                ("Web package and browser gate", GateStatus::Failed),
                (
                    "Web package and browser gate result manifest",
                    GateStatus::Failed
                ),
            ],
        );
        assert_eq!(merged.native, GateStatus::Passed);
        assert_eq!(merged.web, GateStatus::Failed);
    }

    /// An empty manifest is incomplete whatever it declares about itself, so
    /// a forged one cannot publish a green job or project its evidence.
    #[test]
    fn an_empty_manifest_can_neither_pass_a_job_nor_project_its_evidence() {
        let forged = JobReport {
            outcome: JobOutcome::Success,
            result: Some(manifest("verify", Some("verification"), &[])),
        };

        assert_eq!(forged.publishable_result(), None);
        assert_eq!(forged.status(), GateStatus::Failed);
        assert_eq!(forged.evidence(), None);

        let merged = merge_job_results(
            "1111111111111111111111111111111111111111",
            "https://github.com/ridermw/midcreek-cs-1/actions/runs/1",
            &forged,
            &JobReport::absent(JobOutcome::Skipped),
        )
        .expect("a forged empty manifest should publish as a failure");

        assert!(
            merged
                .gates
                .iter()
                .any(|gate| gate.name == "Native verification result manifest"
                    && gate.status == GateStatus::Failed),
            "{merged:?}"
        );
    }

    /// A job whose manifest really measured its gates is still published from
    /// that manifest, so the incomplete-manifest rule cannot swallow a real
    /// matrix.
    #[test]
    fn a_manifest_that_measured_gates_is_still_published_from_its_own_rows() {
        let native = JobReport {
            outcome: JobOutcome::Success,
            result: Some(manifest(
                "verify",
                Some("verification"),
                &[("Rust formatting", None), ("Clippy lints", None)],
            )),
        };

        let merged = merge_job_results(
            "1111111111111111111111111111111111111111",
            "https://github.com/ridermw/midcreek-cs-1/actions/runs/1",
            &native,
            &JobReport::absent(JobOutcome::Skipped),
        )
        .expect("a measured manifest should publish");

        assert_eq!(
            merged
                .gates
                .iter()
                .map(|gate| gate.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Rust formatting",
                "Clippy lints",
                "Web package and browser gate",
            ],
        );
        assert_eq!(native.evidence(), Some("verification"));
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
