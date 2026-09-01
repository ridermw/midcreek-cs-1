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

use serde::Deserialize;

/// The committed contract, embedded at compile time.
const FIDELITY_CONTRACT: &str = include_str!("../docs/reference/fidelity.json");

/// The repository-relative path of the contract this module embeds.
pub const FIDELITY_CONTRACT_PATH: &str = "docs/reference/fidelity.json";

/// One numeric bound a gate is derived from.
///
/// A bound carries whichever of `min` and `max` its gate uses. Both are
/// optional because the contract holds one-sided floors, one-sided ceilings,
/// and two-sided windows, and spelling them with one shape keeps the document
/// readable and the parse total.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Bound {
    /// The inclusive floor, when the gate has one.
    #[serde(default)]
    pub min: Option<f64>,
    /// The inclusive ceiling, when the gate has one.
    #[serde(default)]
    pub max: Option<f64>,
}

impl Bound {
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
    fn named(&self) -> [(&'static str, Bound); 16] {
        [
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
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

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
            if bound.min.is_none() && bound.max.is_none() {
                return Err(format!("{name} carries neither a floor nor a ceiling"));
            }
            if let (Some(min), Some(max)) = (bound.min, bound.max)
                && min > max
            {
                return Err(format!("{name} has a floor {min} above its ceiling {max}"));
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
