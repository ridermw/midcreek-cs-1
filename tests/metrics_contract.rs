//! The metrics contract.
//!
//! These tests pin the measurement engine itself, independently of the
//! rendered contract. They read committed PNGs rather than launching the game,
//! so they are deterministic on every platform — unlike `render_contract`,
//! which needs a real renderer and only produces bit-identical frames under
//! software rasterisation.
//!
//! The golden fixture exists to prove that moving `FrameMetrics` out of
//! `verification.rs` and into `metrics.rs` does not change one measured
//! number. Capture it before the move; assert it after.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use midcreek_cs_1::{
    design::KEY_ART_REFERENCE_PATH,
    metrics::{
        FrameMetrics, MeasureSource, dominant_row_angle, elevation_from_row_angle, load_frame,
        measure,
    },
    verification::parse_verification_args,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn golden_path() -> PathBuf {
    repo_root().join("tests/fixtures/metrics/key-art.json")
}

/// The approved key art, measured.
fn key_art_metrics() -> FrameMetrics {
    let path = repo_root().join(KEY_ART_REFERENCE_PATH);
    let image = load_frame(&path).expect("the approved key art is vendored in this repository");
    FrameMetrics::compute(&image, &BTreeMap::new())
}

#[test]
fn key_art_metrics_match_the_golden_fixture() {
    let measured =
        serde_json::to_string_pretty(&key_art_metrics()).expect("frame metrics serialize");

    // Capturing the golden is deliberately explicit and deliberately rare. It
    // is done once, before the metrics engine moves out of `verification.rs`,
    // and again only if the approved reference art itself is replaced. A test
    // that silently rewrote its own expectation would prove nothing.
    if std::env::var_os("BLESS_GOLDEN").is_some() {
        fs::create_dir_all(
            golden_path()
                .parent()
                .expect("fixture directory has a parent"),
        )
        .expect("fixture directory is creatable");
        fs::write(golden_path(), format!("{measured}\n")).expect("golden is writable");
    }

    let golden = fs::read_to_string(golden_path()).unwrap_or_else(|error| {
        panic!(
            "{} is readable: {error}. Capture it before moving the metrics engine.",
            golden_path().display()
        )
    });

    assert_eq!(
        measured.trim(),
        golden.trim(),
        "measuring the approved key art must produce the same numbers it did \
         before the metrics engine moved"
    );
}

#[test]
fn the_golden_fixture_pins_the_numbers_that_gates_are_derived_from() {
    let metrics = key_art_metrics();
    let golden: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(golden_path()).expect("golden is readable"))
            .expect("golden is valid json");

    for field in [
        "mean_linear_luminance",
        "edge_density",
        "palette_ratio",
        "diagonal_band_low",
        "diagonal_band_high",
    ] {
        assert!(
            golden
                .get(field)
                .and_then(serde_json::Value::as_f64)
                .is_some(),
            "the golden must pin {field}, because a gate is derived from it"
        );
    }

    assert_eq!(metrics.width, 1536, "the approved key art is 1536 wide");
    assert_eq!(metrics.height, 1024, "the approved key art is 1024 tall");
}

// ---------------------------------------------------------------------------
// Row angle and implied elevation
// ---------------------------------------------------------------------------

/// A frame of one flat colour, which has no edges at all.
fn flat_frame(width: u32, height: u32) -> image::RgbImage {
    image::RgbImage::from_pixel(width, height, image::Rgb([222, 230, 235]))
}

#[test]
fn elevation_follows_from_the_row_angle() {
    // The one pair this can be checked against independently: the POC's camera
    // basis puts its ground axes on screen at 40 degrees, and its elevation is
    // 57. Any formula that does not reproduce that pair is wrong.
    let elevation = elevation_from_row_angle(40.0).expect("40 degrees implies an elevation");
    assert!(
        (elevation - 57.0).abs() < 0.05,
        "a 40 degree row angle must imply 57 degrees of elevation, got {elevation}"
    );
}

#[test]
fn a_row_angle_of_45_degrees_or_more_implies_no_elevation() {
    // sin(elevation) = tan(row angle), so a row angle at or beyond 45 degrees
    // asks for a sine above one. It means the measurement is wrong, not that
    // the camera is steep.
    assert_eq!(elevation_from_row_angle(45.0), None);
    assert_eq!(elevation_from_row_angle(60.0), None);
}

#[test]
fn a_flat_frame_has_no_measurable_row_angle() {
    assert!(
        dominant_row_angle(&flat_frame(256, 256)).is_none(),
        "a frame with no edges must report no angle rather than the first bin"
    );
}

#[test]
fn the_approved_key_art_measures_a_shallow_isometric_row_angle() {
    let path = repo_root().join(KEY_ART_REFERENCE_PATH);
    let image = load_frame(&path).expect("the approved key art is vendored");
    let angle = dominant_row_angle(&image).expect("the key art is full of rack rows");

    assert!(
        (25.0..35.0).contains(&angle.low_degrees),
        "the key art's shallow diagonal family should sit near 30 degrees, got {}",
        angle.low_degrees
    );
    assert!(
        (145.0..155.0).contains(&angle.high_degrees),
        "and its mirror near 150, got {}",
        angle.high_degrees
    );
    assert!(
        angle.elevation_degrees().expect("angle implies elevation") < 45.0,
        "the implied elevation must be shallower than the 57 degrees the POC ships"
    );
}

#[test]
fn the_two_diagonal_families_of_the_key_art_mirror_each_other() {
    let image = load_frame(&repo_root().join(KEY_ART_REFERENCE_PATH)).expect("key art");
    let angle = dominant_row_angle(&image).expect("rack rows");

    // A 45-degree azimuth orthographic camera places the two ground axes
    // symmetrically about the vertical. That the measured families mirror each
    // other this closely is the evidence that a real projection is being
    // recovered rather than incidental texture.
    assert!(
        angle.spread_degrees() < 0.5,
        "the families should mirror to well under half a degree, got {:.3}",
        angle.spread_degrees()
    );
    assert!(
        angle.mass > 0.4,
        "rack rows and floor markings should dominate the edge mass, got {:.3}",
        angle.mass
    );
    assert!(
        (34.0..38.0).contains(&angle.elevation_degrees().expect("angle implies elevation")),
        "the approved art measures a shallow isometric camera, got {:.2}",
        angle.elevation_degrees().expect("angle implies elevation")
    );
}

#[test]
fn row_angle_public_construction_reports_invalid_elevation_as_absent() {
    let angle = midcreek_cs_1::metrics::RowAngle {
        low_degrees: 50.0,
        high_degrees: 120.0,
        mass: 1.0,
    };

    assert_eq!(angle.elevation_degrees(), None);
}

// ---------------------------------------------------------------------------
// The --measure subcommand
// ---------------------------------------------------------------------------

#[test]
fn measuring_reference_art_reports_the_camera_it_was_drawn_at() {
    let report = measure(
        &repo_root().join(KEY_ART_REFERENCE_PATH),
        MeasureSource::Reference,
    )
    .expect("the approved key art measures");

    let angle = report
        .row_angle
        .expect("reference art reports its row angle");
    assert!((30.0..31.0).contains(&angle.low_degrees));
    let elevation = report
        .implied_elevation_degrees
        .expect("reference art reports an implied elevation");
    assert!(
        (34.0..38.0).contains(&elevation),
        "the approved art is a shallow isometric camera, got {elevation:.2}"
    );
}

#[test]
fn measuring_a_captured_frame_withholds_the_camera() {
    // The game renders without multisampling, so aliased near-diagonals bias
    // the measured angle. Reporting it anyway would be a confident wrong
    // answer, which is worse than no answer.
    let report = measure(
        &repo_root().join(KEY_ART_REFERENCE_PATH),
        MeasureSource::Capture,
    )
    .expect("any png measures");

    assert!(
        report.row_angle.is_none(),
        "a capture must not report a row angle"
    );
    assert!(report.implied_elevation_degrees.is_none());
    assert!(
        report.note.is_some(),
        "withholding must say why, or it reads as a measurement failure"
    );
}

#[test]
fn every_measurement_reports_the_mass_ratio_the_gates_care_about() {
    let report = measure(
        &repo_root().join(KEY_ART_REFERENCE_PATH),
        MeasureSource::Capture,
    )
    .expect("key art measures");

    assert!(
        (1.4..1.6).contains(&report.rack_to_floor.expect("floor mass gives a ratio")),
        "the approved art holds roughly three parts rack to two parts floor, got {:.3}",
        report.rack_to_floor.expect("floor mass gives a ratio")
    );
    assert!(report.rack_mass > 0.0 && report.floor_mass > 0.0);
    assert!(
        report
            .nearest_role_ratio
            .contains_key(&midcreek_cs_1::design::PaletteRole::SignatureYellow),
        "measurements must expose the per-role nearest histogram"
    );
    assert!(
        report
            .near_role_ratio
            .contains_key(&midcreek_cs_1::design::PaletteRole::Ink),
        "measurements must expose the per-role tolerance histogram"
    );
}

#[test]
fn measuring_a_missing_file_is_an_error_not_a_panic() {
    let missing = repo_root().join("docs/reference/there-is-no-such-plate.png");
    assert!(measure(&missing, MeasureSource::Reference).is_err());
}

#[test]
fn a_floorless_frame_reports_no_ratio_rather_than_infinity() {
    let flat = repo_root().join("tests/fixtures/metrics/no-floor.png");
    image::RgbImage::from_pixel(64, 64, image::Rgb([251, 252, 253]))
        .save(&flat)
        .expect("fixture is writable");
    let report = measure(&flat, MeasureSource::Capture).expect("measures");
    std::fs::remove_file(&flat).ok();

    assert!(
        report.rack_to_floor.is_none(),
        "a frame with no floor has no defined rack-to-floor ratio"
    );
}

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn measure_parses_a_path_in_either_form() {
    let split = parse_verification_args(args(&["--measure", "a.png"])).expect("parses");
    assert_eq!(split.measure.as_deref(), Some(Path::new("a.png")));
    let joined = parse_verification_args(args(&["--measure=a.png"])).expect("parses");
    assert_eq!(joined.measure, split.measure);
}

#[test]
fn measure_defaults_to_treating_an_image_as_a_capture() {
    // The safe default: withhold the camera unless the operator says the image
    // is drawn art. Getting it wrong this way loses a number; the other way
    // publishes a wrong one.
    let request = parse_verification_args(args(&["--measure", "a.png"])).expect("parses");
    assert_eq!(request.measure_source, MeasureSource::Capture);
}

#[test]
fn measure_reference_opts_in_to_reporting_the_camera() {
    let request =
        parse_verification_args(args(&["--measure", "a.png", "--reference"])).expect("parses");
    assert_eq!(request.measure_source, MeasureSource::Reference);
}

#[test]
fn measure_without_a_path_is_a_usage_error() {
    assert!(parse_verification_args(args(&["--measure"])).is_err());
}

#[test]
fn measure_does_not_consume_a_known_flag_as_its_path() {
    let error = parse_verification_args(args(&["--measure", "--reference"]))
        .expect_err("a known flag is not a measurement path");

    assert!(
        error.contains("--measure requires an image path"),
        "the missing path should be reported before source mode, got {error}"
    );
}

#[test]
fn measure_given_twice_is_a_usage_error() {
    assert!(parse_verification_args(args(&["--measure", "a.png", "--measure", "b.png"])).is_err());
}

#[test]
fn reference_without_measure_is_a_usage_error() {
    // Silently ignoring a flag that changes what gets reported would let a
    // mistyped command look like it worked.
    assert!(parse_verification_args(args(&["--reference"])).is_err());
}

#[test]
fn measure_binary_prints_reference_json() {
    let output = Command::new(env!("CARGO_BIN_EXE_midcreek-cs-1"))
        .current_dir(repo_root())
        .args(["--measure", KEY_ART_REFERENCE_PATH, "--reference"])
        .output()
        .expect("measurement binary launches");

    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "a successful measurement must not write stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("measurement stdout is json");
    assert_eq!(json["source"], "reference");
    assert!(json["row_angle"].is_object());
    assert!(json["implied_elevation_degrees"].as_f64().is_some());
    assert!(
        json["nearest_role_ratio"]["SignatureYellow"]
            .as_f64()
            .is_some()
    );
    assert!(json["near_role_ratio"]["Ink"].as_f64().is_some());
}

#[test]
fn measure_binary_reports_decode_errors_on_stderr_only() {
    let output = Command::new(env!("CARGO_BIN_EXE_midcreek-cs-1"))
        .current_dir(repo_root())
        .args(["--measure", "docs/reference/missing.png", "--reference"])
        .output()
        .expect("measurement binary launches");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "failed measurements must leave stdout empty"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("could not be decoded"),
        "stderr must explain the decode failure, got {stderr}"
    );
}
