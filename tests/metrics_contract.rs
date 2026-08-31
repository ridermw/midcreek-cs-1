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

use std::{collections::BTreeMap, fs, path::PathBuf};

use midcreek_cs_1::{
    design::KEY_ART_REFERENCE_PATH,
    metrics::{FrameMetrics, dominant_row_angle, elevation_from_row_angle, load_frame},
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
    // that silently rewrote its own expectation would proved nothing.
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
        angle.elevation_degrees() < 45.0,
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
        (34.0..38.0).contains(&angle.elevation_degrees()),
        "the approved art measures a shallow isometric camera, got {:.2}",
        angle.elevation_degrees()
    );
}
