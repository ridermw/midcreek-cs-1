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
    verification::{FrameMetrics, load_frame},
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
