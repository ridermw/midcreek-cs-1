//! The reference-policy boundary.
//!
//! Every numeric bound a fidelity gate is derived from lives in
//! `docs/reference/fidelity.json` and reaches the code through this module.
//! `src/metrics.rs` measures images and holds no policy; `src/verification.rs`
//! decides whether a measurement passes, using bounds it reads from here.
//!
//! # Why the contract is embedded rather than read from disk
//!
//! The contract is pulled in with [`include_str!`] and parsed once behind a
//! [`OnceLock`]. That choice settles the build question U1 booked, and it is
//! deliberate on three counts. A verification run must not be able to disagree
//! with the repository it is measuring, and an embedded copy cannot drift from
//! the committed file. The browser build has no filesystem, and `metrics`
//! compiles for `wasm32-unknown-unknown`, so runtime file IO is not available
//! to every consumer. And a `build.rs` generating constants would add a
//! codegen step whose output would itself need byte-stability gating, for no
//! capability this does not already provide.
//!
//! The frozen G0 contract hash is therefore a test-time property of the
//! committed file, not a build-time one.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// The committed contract, embedded at compile time.
const FIDELITY_CONTRACT: &str = include_str!("../docs/reference/fidelity.json");

/// The committed reference manifest, embedded at compile time.
const REFERENCE_MANIFEST: &str = include_str!("../docs/reference/manifest.json");

/// The repository-relative path of the manifest this module embeds.
pub const REFERENCE_MANIFEST_PATH: &str = "docs/reference/manifest.json";

/// One approved reference image, as the manifest declares it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceAsset {
    pub name: String,
    pub source_path: String,
    pub public_path: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

/// Every approved reference image this repository vendors and publishes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceManifest {
    pub assets: Vec<ReferenceAsset>,
}

/// The approved references, parsed once for the process.
///
/// This is the single *runtime* source of the approved hashes. They used to be
/// repeated as constants in `design`, which meant three copies of every hash:
/// the constant, the manifest, and the image. The manifest now declares, and
/// the image must match it. The literal pin in `sitegen_contract` remains on
/// purpose as an independent approval record, so changing the approved art
/// still requires a reviewer-visible edit rather than a coordinated swap of
/// the manifest and the PNG together.
#[must_use]
pub fn approved_references() -> &'static ReferenceManifest {
    static MANIFEST: OnceLock<ReferenceManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(REFERENCE_MANIFEST).unwrap_or_else(|error| {
            panic!("{REFERENCE_MANIFEST_PATH} must be a valid reference manifest: {error}")
        })
    })
}

/// The repository-relative path of the contract this module embeds.
pub const FIDELITY_CONTRACT_PATH: &str = "docs/reference/fidelity.json";

/// One numeric bound a gate is derived from.
///
/// A bound carries whichever of `value`, `min` and `max` its gate uses. All
/// are optional because the contract holds one-sided floors, one-sided
/// ceilings, two-sided windows, and calibrated measurements that carry both a
/// value and the tolerance around it. Spelling them with one shape keeps the
/// document readable and the parse total.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Bound {
    /// The calibrated measurement, for a bound taken from the authority image
    /// rather than chosen as an engineering contract.
    #[serde(default)]
    pub value: Option<f64>,
    /// The inclusive floor, when the gate has one.
    #[serde(default)]
    pub min: Option<f64>,
    /// The inclusive ceiling, when the gate has one.
    #[serde(default)]
    pub max: Option<f64>,
}

impl Bound {
    /// The calibrated measurement, for a bound its gate derives from one.
    #[must_use]
    pub fn value(&self) -> f64 {
        self.value
            .expect("the contract must carry a calibrated value for this bound")
    }

    /// The floor, for a bound its gate requires to have one.
    ///
    /// A missing floor is a malformed contract rather than a runtime
    /// condition: the document is committed, embedded, and covered by a test,
    /// so a caller asking for a bound the contract does not carry is a bug in
    /// the pairing of gate to bound.
    #[must_use]
    pub fn min(&self) -> f64 {
        self.min
            .expect("the contract must carry a floor for this bound")
    }

    /// The ceiling, for a bound its gate requires to have one.
    #[must_use]
    pub fn max(&self) -> f64 {
        self.max
            .expect("the contract must carry a ceiling for this bound")
    }

    /// The two-sided window, for a bound its gate requires to have both.
    #[must_use]
    pub fn range(&self) -> (f64, f64) {
        (self.min(), self.max())
    }
}

/// Every bound the fidelity gates read.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    /// The projected ground-axis angle measured from the authority image, and
    /// the reviewed tolerance around it.
    ///
    /// This is the one bound the camera itself is derived from rather than
    /// judged against: [`crate::design::CAMERA_ELEVATION_DEGREES`] follows from
    /// it. The tolerance is the band
    /// `the_approved_key_art_measures_a_shallow_isometric_row_angle` already
    /// asserted before the value was frozen here, so it is a reviewed number
    /// rather than one chosen to fit.
    pub projected_row_angle: Bound,
    /// Half-width of the two diagonal edge windows, in degrees.
    ///
    /// The windows are centred on [`Bounds::projected_row_angle`] and its
    /// mirror, because that is where a correctly projected ground axis lands.
    /// They used to be hard-coded as 30 to 50 and 130 to 150, which is 40
    /// degrees plus or minus this half-width: 40 is where the POC's 57-degree
    /// camera put its axes, so the windows silently encoded the camera the
    /// authority image disagrees with.
    pub diagonal_band_half_width: Bound,
    pub sentinel: Bound,
    pub luminance: Bound,
    pub luminance_reference_tolerance: Bound,
    pub palette: Bound,
    pub floor: Bound,
    pub rack: Bound,
    pub yellow: Bound,
    pub ink: Bound,
    pub diagonal_band: Bound,
    pub histogram: Bound,
    pub edge_density: Bound,
    pub worker_role: Bound,
    pub badge_role: Bound,
    pub hud_state: Bound,
    pub clip_difference: Bound,
    pub outside_crop: Bound,
}

impl Bounds {
    /// Every bound beside the name the contract spells it with.
    fn named(&self) -> [(&'static str, Bound); 18] {
        [
            ("projected_row_angle", self.projected_row_angle),
            ("diagonal_band_half_width", self.diagonal_band_half_width),
            ("sentinel", self.sentinel),
            ("luminance", self.luminance),
            (
                "luminance_reference_tolerance",
                self.luminance_reference_tolerance,
            ),
            ("palette", self.palette),
            ("floor", self.floor),
            ("rack", self.rack),
            ("yellow", self.yellow),
            ("ink", self.ink),
            ("diagonal_band", self.diagonal_band),
            ("histogram", self.histogram),
            ("edge_density", self.edge_density),
            ("worker_role", self.worker_role),
            ("badge_role", self.badge_role),
            ("hud_state", self.hud_state),
            ("clip_difference", self.clip_difference),
            ("outside_crop", self.outside_crop),
        ]
    }
}

/// The machine-readable fidelity contract.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FidelityContract {
    /// The contract version, frozen at G0.
    pub schema_version: u32,
    /// What the document is and how it is consumed.
    pub description: String,
    /// Every bound a gate is derived from.
    pub bounds: Bounds,
}

/// The contract version this module knows how to read.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 2;

/// The committed contract, parsed once for the process.
///
/// The contract is repository data rather than input, so a document that does
/// not parse, or that carries a version or a bound this code cannot honour, is
/// a broken build and not a runtime condition to report.
#[must_use]
pub fn contract() -> &'static FidelityContract {
    static CONTRACT: OnceLock<FidelityContract> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        let contract: FidelityContract =
            serde_json::from_str(FIDELITY_CONTRACT).unwrap_or_else(|error| {
                panic!("{FIDELITY_CONTRACT_PATH} must be a valid fidelity contract: {error}")
            });
        if let Err(reason) = contract.validate() {
            panic!("{FIDELITY_CONTRACT_PATH} is not a usable fidelity contract: {reason}");
        }
        contract
    })
}

impl FidelityContract {
    /// Every structural invariant a gate assumes when it reads a bound.
    ///
    /// Parsing proves the document has the right shape. It does not prove the
    /// numbers are usable: a window whose floor sits above its ceiling rejects
    /// every measurement and would read as a permanently failing gate rather
    /// than as a malformed contract, and a version this code does not know may
    /// mean a bound it does not read at all.
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(format!(
                "schema version {} is not the supported {SUPPORTED_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        for (name, bound) in self.bounds.named() {
            if bound.value.is_none() && bound.min.is_none() && bound.max.is_none() {
                return Err(format!("{name} carries no value, floor, or ceiling"));
            }
            if let (Some(min), Some(max)) = (bound.min, bound.max)
                && min > max
            {
                return Err(format!("{name} has a floor {min} above its ceiling {max}"));
            }
            // A calibrated measurement outside its own tolerance is the one
            // shape that looks fine field by field and is nonsense together.
            if let Some(value) = bound.value
                && (bound.min.is_some_and(|min| value < min)
                    || bound.max.is_some_and(|max| value > max))
            {
                return Err(format!(
                    "{name} has a calibrated value {value} outside its own tolerance"
                ));
            }
        }
        Ok(())
    }
}

/// Every bound the fidelity gates read.
#[must_use]
pub fn bounds() -> &'static Bounds {
    &contract().bounds
}

/// The two diagonal edge windows, in screen degrees.
///
/// Both are centred on the authority's projected ground-axis angle rather than
/// on a literal, so a camera that matches the reference puts its rows in the
/// middle of the window instead of at its edge.
#[must_use]
pub fn diagonal_band_windows() -> (std::ops::Range<f64>, std::ops::Range<f64>) {
    let bounds = bounds();
    let centre = bounds.projected_row_angle.value();
    let half = bounds.diagonal_band_half_width.value();
    (
        (centre - half)..(centre + half),
        (180.0 - centre - half)..(180.0 - centre + half),
    )
}

/// The approved hash of one vendored reference image.
///
/// Panics on an unknown path: the manifest is committed repository data, so a
/// caller naming a reference this repository does not vendor is a bug rather
/// than a runtime condition.
#[must_use]
pub fn approved_reference_sha256(public_path: &str) -> &'static str {
    approved_references()
        .assets
        .iter()
        .find(|asset| asset.public_path == public_path)
        .map(|asset| asset.sha256.as_str())
        .unwrap_or_else(|| panic!("{public_path} is not an approved reference"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract carries the calibrated number every gate is judged
    /// against.
    ///
    /// These are the values the gates were calibrated with before U1 moved
    /// them out of `metrics`, written literally on purpose. The contract is
    /// data, and data with no independent copy can be edited without any test
    /// noticing. An edit to `fidelity.json` is a recalibration, and after G0 it
    /// needs authority-only justification and a new near-boundary fixture, so
    /// it has to fail here first rather than quietly move a gate.
    #[test]
    fn the_contract_pins_every_calibrated_bound() {
        let bounds = bounds();

        assert_eq!(bounds.projected_row_angle.value(), 30.505_530_591_671_54);
        assert_eq!(bounds.projected_row_angle.range(), (25.0, 35.0));
        assert_eq!(bounds.diagonal_band_half_width.value(), 10.0);
        assert_eq!(bounds.sentinel.max(), 0.001);
        assert_eq!(bounds.luminance.range(), (0.48, 0.88));
        assert_eq!(bounds.luminance_reference_tolerance.max(), 0.18);
        assert_eq!(bounds.palette.min(), 0.60);
        assert_eq!(bounds.floor.min(), 0.20);
        assert_eq!(bounds.rack.min(), 0.06);
        assert_eq!(bounds.yellow.min(), 0.005);
        assert_eq!(bounds.ink.range(), (0.03, 0.35));
        assert_eq!(bounds.diagonal_band.min(), 0.08);
        assert_eq!(bounds.histogram.max(), 0.90);
        assert_eq!(bounds.edge_density.range(), (0.35, 2.5));
        assert_eq!(bounds.worker_role.min(), 0.002);
        assert_eq!(bounds.badge_role.min(), 0.10);
        assert_eq!(bounds.hud_state.min(), 0.002);
        assert_eq!(bounds.clip_difference.range(), (0.02, 0.60));
        assert_eq!(bounds.outside_crop.max(), 0.01);
    }

    /// The embedded contract is the document on disk, not a stale copy.
    ///
    /// This proves what [`include_str!`] can prove and no more: the bytes the
    /// binary carries are the bytes in the working tree it was built from. It
    /// is deliberately not a claim about committed state, and it is not the
    /// frozen G0 hash gate, which does not exist yet.
    #[test]
    fn the_embedded_contract_matches_the_document_on_disk() {
        let on_disk = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIDELITY_CONTRACT_PATH),
        )
        .expect("the fidelity contract must be present");
        let parsed: FidelityContract =
            serde_json::from_str(&on_disk).expect("the document on disk must parse");
        assert_eq!(
            &parsed,
            contract(),
            "the embedded contract has drifted from the document on disk"
        );
    }

    /// A contract that cannot be honoured is a broken build, not a failing
    /// gate.
    #[test]
    fn validation_refuses_an_unusable_contract() {
        let mut inverted = contract().clone();
        inverted.bounds.luminance = Bound {
            value: None,
            min: Some(0.9),
            max: Some(0.1),
        };
        assert!(
            inverted.validate().is_err(),
            "a floor above its ceiling rejects every measurement and must be refused"
        );

        let mut empty = contract().clone();
        empty.bounds.palette = Bound {
            value: None,
            min: None,
            max: None,
        };
        assert!(
            empty.validate().is_err(),
            "a bound with no value, floor, or ceiling is a gate with nothing to judge against"
        );

        let mut outside = contract().clone();
        outside.bounds.projected_row_angle = Bound {
            value: Some(99.0),
            min: Some(25.0),
            max: Some(35.0),
        };
        assert!(
            outside.validate().is_err(),
            "a calibrated value outside its own tolerance is nonsense the fields hide"
        );

        let mut future = contract().clone();
        future.schema_version = SUPPORTED_SCHEMA_VERSION + 1;
        assert!(
            future.validate().is_err(),
            "a version this code cannot read may carry a bound it never applies"
        );
    }
}
