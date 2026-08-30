//! Autonomous asset pipeline for the Cell Shift data center POC.
//!
//! Every shipped mesh is generated from repository-owned declarative RON in
//! `assets/source` by the deterministic tessellator and glTF writer in this
//! module. There is no external content tool, no third-party art, and no manual
//! export step anywhere in the chain.
//!
//! Determinism rules enforced here:
//!
//! * every float written to a buffer or to JSON is quantized to a 1e-6 grid and
//!   normalized so `-0.0` can never appear;
//! * every collection is ordered by declaration order or by the fixed
//!   [`PaletteRole::ALL`] order, never by hash iteration;
//! * the asset generator string is a constant, and no timestamp, host name,
//!   user name, or filesystem path is ever embedded in an output.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use bevy::color::LinearRgba;
use gltf_json as json;
use json::validation::{Checked, USize64};
use serde::{Deserialize, Serialize};

use crate::design::PaletteRole;

/// Directory holding the declarative, repository-owned asset sources.
pub const SOURCE_DIR: &str = "assets/source";
/// Directory holding the committed generated binary glTF assets.
pub const GENERATED_DIR: &str = "assets/generated";
/// Fixed generator string. It deliberately carries no version or timestamp.
pub const GENERATOR_NAME: &str = "midcreek-cs-1 assetgen";

/// Every generated asset, in stable alphabetical order.
pub const ASSET_NAMES: [&str; 5] = [
    "cooling-unit",
    "infrastructure",
    "rack",
    "technician",
    "utility-props",
];

/// The scene/module names each asset must expose, in declaration order.
pub const ASSET_MODULES: [(&str, &[&str]); 5] = [
    ("cooling-unit", &["cooling-unit"]),
    (
        "infrastructure",
        &["overhead-tray", "hose-drop", "floor-grid"],
    ),
    ("rack", &["rack-row"]),
    ("technician", &["technician"]),
    ("utility-props", &["utility-cart", "step-stool"]),
];

/// The technician rig, in skin joint order. `bone-hips` is the skeleton root.
pub const TECHNICIAN_BONES: [&str; 11] = [
    "bone-hips",
    "bone-spine",
    "bone-chest",
    "bone-head",
    "bone-arm-upper-left",
    "bone-arm-lower-left",
    "bone-arm-upper-right",
    "bone-arm-lower-right",
    "bone-tool",
    "bone-leg-left",
    "bone-leg-right",
];

/// The technician animation clips, in declaration order.
pub const TECHNICIAN_CLIPS: [&str; 3] = ["Idle", "Walk", "Repair"];

/// Merge budget: one primitive per palette role per module, and no more.
pub const MAX_PRIMITIVES_PER_MESH: usize = 12;
/// Triangle budget for a single generated asset file.
pub const MAX_TRIANGLES_PER_ASSET: usize = 24_000;
/// Vertex budget for a single merged primitive; keeps 16-bit indices valid.
pub const MAX_VERTICES_PER_PRIMITIVE: usize = 60_000;

/// `JOINTS_0` is written as unsigned bytes, so a rig may declare at most 256
/// bones. Exceeding this is rejected at validation rather than truncated.
pub const MAX_BONES_PER_RIG: usize = 256;

const MAX_INSTANCES_PER_SHAPE: usize = 4_096;
const MAX_REPEAT_COUNT: u32 = 1_024;
const QUANTIZATION_SCALE: f64 = 1.0e6;
const MIN_CYLINDER_SEGMENTS: u32 = 3;
const MAX_CYLINDER_SEGMENTS: u32 = 64;
const RIGID_WEIGHTS: [f32; 4] = [1.0, 0.0, 0.0, 0.0];
/// Joint slot written for unrigged modules, whose `JOINTS_0` accessor is never
/// emitted. It is a structural placeholder, not a fallback for a missing bone.
const UNSKINNED_JOINT: u16 = 0;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every failure the pipeline can report. Each variant names the file it
/// concerns so a failed generation is never silent or ambiguous.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetGenError {
    /// A source or output file could not be read or written.
    Io {
        /// Offending file.
        path: String,
        /// Underlying message.
        message: String,
    },
    /// A source file is not valid RON for the asset schema.
    Parse {
        /// Offending file.
        path: String,
        /// Underlying message, including the offending value.
        message: String,
    },
    /// A source file parsed but violates a pipeline invariant.
    Invalid {
        /// Offending file.
        path: String,
        /// Dotted field path inside the source document.
        field: String,
        /// Human readable explanation.
        message: String,
    },
    /// A committed asset does not match a fresh generation.
    Stale {
        /// Offending committed file.
        path: String,
        /// Byte length of the freshly generated asset.
        generated_bytes: usize,
        /// Byte length of the committed asset.
        committed_bytes: usize,
    },
    /// Two independent generations of the same source disagreed.
    Nondeterministic {
        /// Offending asset name.
        path: String,
    },
}

impl fmt::Display for AssetGenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => write!(formatter, "{path}: io error: {message}"),
            Self::Parse { path, message } => write!(formatter, "{path}: parse error: {message}"),
            Self::Invalid {
                path,
                field,
                message,
            } => write!(formatter, "{path}: {field}: {message}"),
            Self::Stale {
                path,
                generated_bytes,
                committed_bytes,
            } => write!(
                formatter,
                "{path}: committed asset is stale ({committed_bytes} bytes) and does not match \
                 generation ({generated_bytes} bytes); run `assetgen --write`"
            ),
            Self::Nondeterministic { path } => write!(
                formatter,
                "{path}: two independent generations produced different bytes"
            ),
        }
    }
}

impl std::error::Error for AssetGenError {}

fn io_error(path: &Path, error: &io::Error) -> AssetGenError {
    AssetGenError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Declarative source schema
// ---------------------------------------------------------------------------

/// A three component authored value in metres or degrees.
pub type Triple = [f64; 3];

/// One repository-owned asset source document.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSource {
    /// Asset name; must match the source file stem.
    pub asset: String,
    /// Modules, each of which becomes one named glTF scene.
    pub modules: Vec<ModuleSource>,
}

/// One module: a merged mesh plus an optional rig.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleSource {
    /// Scene and root node name.
    pub name: String,
    /// Optional skeleton, skin binding, and animation clips.
    #[serde(default)]
    pub rig: Option<RigSource>,
    /// Authored primitives, merged by palette role.
    pub shapes: Vec<ShapeSource>,
}

/// One authored primitive, optionally repeated on a lattice.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeSource {
    /// Author-facing name; used for diagnostics only because shapes are merged.
    pub name: String,
    /// Palette role that selects the merged primitive and material.
    pub role: PaletteRole,
    /// Rigid skin binding; required when the module declares a rig.
    #[serde(default)]
    pub bone: Option<String>,
    /// Nested repeat lattice; the instance count is the product of the counts.
    #[serde(default)]
    pub repeat: Vec<RepeatSource>,
    /// When set, also emit an inverted, expanded ink hull for a cel outline.
    #[serde(default)]
    pub outline: Option<f64>,
    /// The primitive itself.
    pub primitive: PrimitiveSource,
}

/// One level of a repeat lattice.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepeatSource {
    /// Instances at this level, including the original.
    pub count: u32,
    /// Translation applied per step at this level.
    pub step: Triple,
}

/// Axis of revolution for a cylinder.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Axis {
    /// Cylinder revolves around the X axis.
    X,
    /// Cylinder revolves around the Y axis.
    Y,
    /// Cylinder revolves around the Z axis.
    Z,
}

/// The authored primitive shapes the tessellator understands.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum PrimitiveSource {
    /// Axis aligned flat-shaded box.
    Box {
        /// Box centre.
        center: Triple,
        /// Positive half extents.
        half_extents: Triple,
    },
    /// Flat-shaded faceted cylinder with capped ends.
    Cylinder {
        /// Cylinder centre.
        center: Triple,
        /// Positive radius.
        radius: f64,
        /// Positive half height along `axis`.
        half_height: f64,
        /// Axis of revolution.
        axis: Axis,
        /// Facet count.
        segments: u32,
    },
}

/// A skeleton, its skin node name, and its animation clips.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigSource {
    /// Node name carrying the skinned mesh.
    pub skin_node: String,
    /// Bones in joint order; a parent must precede its children.
    pub bones: Vec<BoneSource>,
    /// Animation clips in declaration order.
    pub clips: Vec<ClipSource>,
}

/// One bone of the rest skeleton. Rest rotations are identity by construction.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BoneSource {
    /// Bone name; also the glTF node name.
    pub name: String,
    /// Parent bone name, or `None` for the skeleton root.
    pub parent: Option<String>,
    /// Rest translation relative to the parent.
    pub translation: Triple,
}

/// One animation clip.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClipSource {
    /// Clip name as exposed to the runtime.
    pub name: String,
    /// Clip duration in seconds; the last keyframe must land on it.
    pub duration: f64,
    /// Animated tracks in declaration order.
    pub tracks: Vec<TrackSource>,
}

/// One animated channel of one bone.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackSource {
    /// Target bone name.
    pub bone: String,
    /// Animated property.
    pub channel: ChannelSource,
    /// Keyframes, strictly ascending in time and starting at zero.
    pub keys: Vec<KeySource>,
}

/// Animated property of a bone.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ChannelSource {
    /// Local translation offset from the rest translation, in metres.
    Translation,
    /// Extrinsic XYZ Euler rotation in degrees.
    Rotation,
}

/// One keyframe.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeySource {
    /// Keyframe time in seconds.
    pub time: f64,
    /// Channel dependent value.
    pub value: Triple,
}

// ---------------------------------------------------------------------------
// Public pipeline API
// ---------------------------------------------------------------------------

/// One generated asset held in memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedAsset {
    /// Asset name, matching the source stem.
    pub name: String,
    /// Output file name.
    pub file_name: String,
    /// Binary glTF bytes.
    pub bytes: Vec<u8>,
}

/// Result of a successful `--check`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckReport {
    /// Asset names that were regenerated and byte compared.
    pub checked: Vec<String>,
}

/// Path of the declarative source for `name` under `root`.
pub fn source_path(root: &Path, name: &str) -> PathBuf {
    root.join(SOURCE_DIR).join(format!("{name}.ron"))
}

/// Path of the generated binary glTF for `name` under `root`.
pub fn generated_path(root: &Path, name: &str) -> PathBuf {
    root.join(GENERATED_DIR).join(format!("{name}.glb"))
}

/// Parse and validate a source document. `path` is used for diagnostics only
/// and never reaches an output file.
pub fn parse_source(text: &str, path: &str) -> Result<AssetSource, AssetGenError> {
    let source: AssetSource = ron::from_str(text).map_err(|error| AssetGenError::Parse {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    validate_source(&source, path)?;
    Ok(source)
}

/// Read, parse, and validate the source for `name` under `root`.
pub fn load_source(root: &Path, name: &str) -> Result<AssetSource, AssetGenError> {
    let path = source_path(root, name);
    let text = fs::read_to_string(&path).map_err(|error| io_error(&path, &error))?;
    let display = relative_display(root, &path);
    let source = parse_source(&text, &display)?;
    if source.asset != name {
        return Err(AssetGenError::Invalid {
            path: display,
            field: "asset".to_owned(),
            message: format!(
                "asset name {:?} does not match the file stem {name:?}",
                source.asset
            ),
        });
    }
    Ok(source)
}

/// Build one binary glTF document from a parsed source.
///
/// `path` is used for diagnostics only and never reaches an output file. This
/// is the same code path [`generate_assets`] uses, exposed so budget and rig
/// invariants can be exercised against a single document.
pub fn generate_glb(source: &AssetSource, path: &str) -> Result<Vec<u8>, AssetGenError> {
    build_glb(source, path)
}

/// Generate every asset in memory, in [`ASSET_NAMES`] order.
pub fn generate_assets(root: &Path) -> Result<Vec<GeneratedAsset>, AssetGenError> {
    let mut generated = Vec::with_capacity(ASSET_NAMES.len());
    for name in ASSET_NAMES {
        let source = load_source(root, name)?;
        let display = relative_display(root, &source_path(root, name));
        let bytes = build_glb(&source, &display)?;
        generated.push(GeneratedAsset {
            name: name.to_owned(),
            file_name: format!("{name}.glb"),
            bytes,
        });
    }
    Ok(generated)
}

/// Generate every asset and write it into `output_directory`.
///
/// Only the five named `.glb` files are written; nothing is ever deleted.
pub fn write_assets(root: &Path, output_directory: &Path) -> Result<Vec<PathBuf>, AssetGenError> {
    let generated = generate_assets(root)?;
    fs::create_dir_all(output_directory).map_err(|error| io_error(output_directory, &error))?;

    let mut written = Vec::with_capacity(generated.len());
    for asset in generated {
        let path = output_directory.join(&asset.file_name);
        fs::write(&path, &asset.bytes).map_err(|error| io_error(&path, &error))?;
        written.push(path);
    }
    Ok(written)
}

/// Regenerate every asset twice into two separate temporary roots, require the
/// two generations to be byte identical, and require the committed assets to
/// match them exactly.
pub fn check_assets(root: &Path) -> Result<CheckReport, AssetGenError> {
    let first = TempRoot::create("assetgen-check-first")?;
    let second = TempRoot::create("assetgen-check-second")?;

    write_assets(root, first.path())?;
    write_assets(root, second.path())?;

    let mut checked = Vec::with_capacity(ASSET_NAMES.len());
    for name in ASSET_NAMES {
        let file_name = format!("{name}.glb");
        let first_path = first.path().join(&file_name);
        let second_path = second.path().join(&file_name);
        let first_bytes = fs::read(&first_path).map_err(|error| io_error(&first_path, &error))?;
        let second_bytes =
            fs::read(&second_path).map_err(|error| io_error(&second_path, &error))?;
        if first_bytes != second_bytes {
            return Err(AssetGenError::Nondeterministic {
                path: file_name.clone(),
            });
        }

        let committed_path = generated_path(root, name);
        let committed =
            fs::read(&committed_path).map_err(|error| io_error(&committed_path, &error))?;
        if committed != first_bytes {
            return Err(AssetGenError::Stale {
                path: relative_display(root, &committed_path),
                generated_bytes: first_bytes.len(),
                committed_bytes: committed.len(),
            });
        }
        checked.push(name.to_owned());
    }

    Ok(CheckReport { checked })
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// ---------------------------------------------------------------------------
// Temporary roots
// ---------------------------------------------------------------------------

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn create(label: &str) -> Result<Self, AssetGenError> {
        let path = std::env::temp_dir().join(format!(
            "midcreek-{label}-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        if path.exists() {
            fs::remove_dir_all(&path).map_err(|error| io_error(&path, &error))?;
        }
        fs::create_dir_all(&path).map_err(|error| io_error(&path, &error))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        if self.0.starts_with(std::env::temp_dir()) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

struct Validator<'a> {
    path: &'a str,
}

impl Validator<'_> {
    fn fail<T>(&self, field: String, message: impl Into<String>) -> Result<T, AssetGenError> {
        Err(AssetGenError::Invalid {
            path: self.path.to_owned(),
            field,
            message: message.into(),
        })
    }

    fn require(
        &self,
        condition: bool,
        field: String,
        message: impl Into<String>,
    ) -> Result<(), AssetGenError> {
        if condition {
            Ok(())
        } else {
            self.fail(field, message)
        }
    }

    fn finite(&self, values: &[f64], field: String) -> Result<(), AssetGenError> {
        self.require(
            values.iter().all(|value| value.is_finite()),
            field,
            "values must be finite",
        )
    }
}

fn validate_source(source: &AssetSource, path: &str) -> Result<(), AssetGenError> {
    let validator = Validator { path };
    validator.require(
        !source.asset.trim().is_empty(),
        "asset".to_owned(),
        "asset name must not be empty",
    )?;
    validator.require(
        !source.modules.is_empty(),
        "modules".to_owned(),
        "at least one module is required",
    )?;

    let mut module_names = BTreeSet::new();
    // glTF animations live on the document, not on a skin, so clip names must be
    // unique across every rigged module in one source.
    let mut animation_names = BTreeSet::new();
    for (module_index, module) in source.modules.iter().enumerate() {
        let module_field = format!("modules[{module_index}]");
        validator.require(
            !module.name.trim().is_empty(),
            format!("{module_field}.name"),
            "module name must not be empty",
        )?;
        validator.require(
            module_names.insert(module.name.clone()),
            format!("{module_field}.name"),
            format!("duplicate module name {}", module.name),
        )?;
        validator.require(
            !module.shapes.is_empty(),
            format!("{module_field}.shapes"),
            "at least one shape is required",
        )?;

        let bone_names = match &module.rig {
            Some(rig) => validate_rig(&validator, rig, &module_field, &mut animation_names)?,
            None => BTreeSet::new(),
        };

        let mut shape_names = BTreeSet::new();
        let mut roles = BTreeSet::new();
        for (shape_index, shape) in module.shapes.iter().enumerate() {
            let field = format!("{module_field}.shapes[{shape_index}]");
            validator.require(
                !shape.name.trim().is_empty(),
                format!("{field}.name"),
                "shape name must not be empty",
            )?;
            validator.require(
                shape_names.insert(shape.name.clone()),
                format!("{field}.name"),
                format!("duplicate shape name {}", shape.name),
            )?;
            roles.insert(role_index(shape.role));

            match (&module.rig, &shape.bone) {
                (Some(_), None) => validator.fail(
                    format!("{field}.bone"),
                    "a rigged module requires every shape to declare a bone",
                )?,
                (None, Some(bone)) => validator.fail(
                    format!("{field}.bone"),
                    format!("shape declares bone {bone} but the module has no rig"),
                )?,
                (Some(_), Some(bone)) => validator.require(
                    bone_names.contains(bone),
                    format!("{field}.bone"),
                    format!("unknown bone {bone}"),
                )?,
                (None, None) => {}
            }

            if let Some(expansion) = shape.outline {
                validator.require(
                    expansion.is_finite() && expansion > 0.0,
                    format!("{field}.outline"),
                    "outline expansion must be finite and positive",
                )?;
                roles.insert(role_index(PaletteRole::Ink));
            }

            let mut instances = 1usize;
            for (level_index, level) in shape.repeat.iter().enumerate() {
                let level_field = format!("{field}.repeat[{level_index}]");
                validator.require(
                    (1..=MAX_REPEAT_COUNT).contains(&level.count),
                    format!("{level_field}.count"),
                    format!("repeat count must be between 1 and {MAX_REPEAT_COUNT}"),
                )?;
                validator.finite(&level.step, format!("{level_field}.step"))?;
                instances = instances.saturating_mul(level.count as usize);
            }
            validator.require(
                instances <= MAX_INSTANCES_PER_SHAPE,
                format!("{field}.repeat"),
                format!("{instances} instances exceed the {MAX_INSTANCES_PER_SHAPE} budget"),
            )?;

            validate_primitive(&validator, &shape.primitive, &field)?;
        }

        validator.require(
            roles.len() <= MAX_PRIMITIVES_PER_MESH,
            format!("{module_field}.shapes"),
            format!(
                "{} palette roles exceed the {MAX_PRIMITIVES_PER_MESH} merged primitive budget",
                roles.len()
            ),
        )?;
    }

    Ok(())
}

fn validate_primitive(
    validator: &Validator<'_>,
    primitive: &PrimitiveSource,
    field: &str,
) -> Result<(), AssetGenError> {
    match primitive {
        PrimitiveSource::Box {
            center,
            half_extents,
        } => {
            validator.finite(center, format!("{field}.primitive.center"))?;
            validator.finite(half_extents, format!("{field}.primitive.half_extents"))?;
            validator.require(
                half_extents.iter().all(|value| *value > 0.0),
                format!("{field}.primitive.half_extents"),
                "every half extent must be positive",
            )
        }
        PrimitiveSource::Cylinder {
            center,
            radius,
            half_height,
            axis: _,
            segments,
        } => {
            validator.finite(center, format!("{field}.primitive.center"))?;
            validator.require(
                radius.is_finite() && *radius > 0.0,
                format!("{field}.primitive.radius"),
                "radius must be finite and positive",
            )?;
            validator.require(
                half_height.is_finite() && *half_height > 0.0,
                format!("{field}.primitive.half_height"),
                "half height must be finite and positive",
            )?;
            validator.require(
                (MIN_CYLINDER_SEGMENTS..=MAX_CYLINDER_SEGMENTS).contains(segments),
                format!("{field}.primitive.segments"),
                format!(
                    "segments must be between {MIN_CYLINDER_SEGMENTS} and {MAX_CYLINDER_SEGMENTS}"
                ),
            )
        }
    }
}

fn validate_rig(
    validator: &Validator<'_>,
    rig: &RigSource,
    module_field: &str,
    animation_names: &mut BTreeSet<String>,
) -> Result<BTreeSet<String>, AssetGenError> {
    let rig_field = format!("{module_field}.rig");
    validator.require(
        !rig.skin_node.trim().is_empty(),
        format!("{rig_field}.skin_node"),
        "skin node name must not be empty",
    )?;
    validator.require(
        !rig.bones.is_empty(),
        format!("{rig_field}.bones"),
        "at least one bone is required",
    )?;
    validator.require(
        rig.bones.len() <= MAX_BONES_PER_RIG,
        format!("{rig_field}.bones"),
        format!(
            "{} bones exceed the {MAX_BONES_PER_RIG} bone budget of the unsigned byte JOINTS_0 \
             encoding",
            rig.bones.len()
        ),
    )?;

    let mut names = BTreeSet::new();
    let mut roots = 0usize;
    for (bone_index, bone) in rig.bones.iter().enumerate() {
        let field = format!("{rig_field}.bones[{bone_index}]");
        validator.require(
            !bone.name.trim().is_empty(),
            format!("{field}.name"),
            "bone name must not be empty",
        )?;
        validator.finite(&bone.translation, format!("{field}.translation"))?;
        match &bone.parent {
            None => roots += 1,
            Some(parent) => validator.require(
                names.contains(parent),
                format!("{field}.parent"),
                format!("unknown or forward referenced parent bone {parent}"),
            )?,
        }
        validator.require(
            names.insert(bone.name.clone()),
            format!("{field}.name"),
            format!("duplicate bone name {}", bone.name),
        )?;
    }
    validator.require(
        roots == 1,
        format!("{rig_field}.bones"),
        format!("exactly one root bone is required, found {roots}"),
    )?;

    let mut clip_names = BTreeSet::new();
    for (clip_index, clip) in rig.clips.iter().enumerate() {
        let clip_field = format!("{rig_field}.clips[{clip_index}]");
        validator.require(
            !clip.name.trim().is_empty(),
            format!("{clip_field}.name"),
            "clip name must not be empty",
        )?;
        validator.require(
            clip_names.insert(clip.name.clone()),
            format!("{clip_field}.name"),
            format!("duplicate clip name {}", clip.name),
        )?;
        validator.require(
            animation_names.insert(clip.name.clone()),
            format!("{clip_field}.name"),
            format!(
                "duplicate animation name {} across rigged modules; glTF animation names are \
                 document scoped",
                clip.name
            ),
        )?;
        validator.require(
            clip.duration.is_finite() && clip.duration > 0.0,
            format!("{clip_field}.duration"),
            "clip duration must be finite and positive",
        )?;
        validator.require(
            !clip.tracks.is_empty(),
            format!("{clip_field}.tracks"),
            "at least one track is required",
        )?;

        let mut targets = BTreeSet::new();
        for (track_index, track) in clip.tracks.iter().enumerate() {
            let field = format!("{clip_field}.tracks[{track_index}]");
            validator.require(
                names.contains(&track.bone),
                format!("{field}.bone"),
                format!("unknown bone {}", track.bone),
            )?;
            validator.require(
                targets.insert((track.bone.clone(), track.channel)),
                format!("{field}.channel"),
                format!("duplicate channel for bone {}", track.bone),
            )?;
            validator.require(
                track.keys.len() >= 2,
                format!("{field}.keys"),
                "at least two keyframes are required",
            )?;

            for (key_index, key) in track.keys.iter().enumerate() {
                let key_field = format!("{field}.keys[{key_index}]");
                validator.require(
                    key.time.is_finite(),
                    format!("{key_field}.time"),
                    "keyframe time must be finite",
                )?;
                validator.finite(&key.value, format!("{key_field}.value"))?;
                if key_index > 0 {
                    validator.require(
                        key.time > track.keys[key_index - 1].time,
                        format!("{field}.keys[{key_index}].time"),
                        "keyframe times must be strictly ascending",
                    )?;
                }
                if track.channel == ChannelSource::Rotation {
                    validator.require(
                        key.value.iter().all(|value| value.abs() <= 180.0),
                        format!("{key_field}.value"),
                        "Euler rotations must stay within +/-180 degrees",
                    )?;
                }
            }

            let first = track.keys[0].time;
            let last = track.keys[track.keys.len() - 1].time;
            validator.require(
                first == 0.0,
                format!("{field}.keys[0].time"),
                "the first keyframe must land on zero",
            )?;
            validator.require(
                (last - clip.duration).abs() <= 1.0e-9,
                format!("{field}.keys"),
                format!(
                    "the last keyframe must land on the clip duration {}",
                    clip.duration
                ),
            )?;
        }
    }

    Ok(names)
}

// ---------------------------------------------------------------------------
// Deterministic numerics
// ---------------------------------------------------------------------------

fn quantize(value: f64) -> f32 {
    let scaled = (value * QUANTIZATION_SCALE).round() / QUANTIZATION_SCALE;
    let normalized = if scaled == 0.0 { 0.0 } else { scaled };
    normalized as f32
}

fn quantize_triple(value: [f64; 3]) -> [f32; 3] {
    [quantize(value[0]), quantize(value[1]), quantize(value[2])]
}

fn normalize(value: [f64; 3]) -> [f64; 3] {
    let length = (value[0] * value[0] + value[1] * value[1] + value[2] * value[2]).sqrt();
    if length == 0.0 {
        return [0.0, 1.0, 0.0];
    }
    [value[0] / length, value[1] / length, value[2] / length]
}

fn quaternion_from_euler_degrees(euler: [f64; 3]) -> [f32; 4] {
    let (sin_x, cos_x) = (euler[0].to_radians() * 0.5).sin_cos();
    let (sin_y, cos_y) = (euler[1].to_radians() * 0.5).sin_cos();
    let (sin_z, cos_z) = (euler[2].to_radians() * 0.5).sin_cos();

    let raw = [
        sin_x * cos_y * cos_z + cos_x * sin_y * sin_z,
        cos_x * sin_y * cos_z - sin_x * cos_y * sin_z,
        cos_x * cos_y * sin_z + sin_x * sin_y * cos_z,
        cos_x * cos_y * cos_z - sin_x * sin_y * sin_z,
    ];
    let length = raw.iter().map(|value| value * value).sum::<f64>().sqrt();
    let length = if length == 0.0 { 1.0 } else { length };
    [
        quantize(raw[0] / length),
        quantize(raw[1] / length),
        quantize(raw[2] / length),
        quantize(raw[3] / length),
    ]
}

fn role_index(role: PaletteRole) -> usize {
    PaletteRole::ALL
        .iter()
        .position(|candidate| *candidate == role)
        .expect("every palette role appears in PaletteRole::ALL")
}

fn role_name(role: PaletteRole) -> String {
    format!("{role:?}")
}

// ---------------------------------------------------------------------------
// Tessellation and merging
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RoleGeometry {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    joints: Vec<u16>,
    indices: Vec<u16>,
}

impl RoleGeometry {
    fn push_face(
        &mut self,
        corners: [[f64; 3]; 4],
        normal: [f64; 3],
        joint: u16,
        inverted: bool,
    ) -> Result<(), &'static str> {
        let winding: [u16; 6] = if inverted {
            [0, 2, 1, 0, 3, 2]
        } else {
            [0, 1, 2, 0, 2, 3]
        };
        self.push_polygon(&corners, orient(normal, inverted), joint, &winding)
    }

    fn push_triangle(
        &mut self,
        corners: [[f64; 3]; 3],
        normal: [f64; 3],
        joint: u16,
        inverted: bool,
    ) -> Result<(), &'static str> {
        let winding: [u16; 3] = if inverted { [0, 2, 1] } else { [0, 1, 2] };
        self.push_polygon(&corners, orient(normal, inverted), joint, &winding)
    }

    fn push_polygon(
        &mut self,
        corners: &[[f64; 3]],
        normal: [f64; 3],
        joint: u16,
        winding: &[u16],
    ) -> Result<(), &'static str> {
        let base = self.positions.len();
        if base + corners.len() > MAX_VERTICES_PER_PRIMITIVE {
            return Err("merged primitive exceeds the vertex budget");
        }
        let normal = quantize_triple(normalize(normal));
        for corner in corners {
            self.positions.push(quantize_triple(*corner));
            self.normals.push(normal);
            self.joints.push(joint);
        }
        let base = base as u16;
        for offset in winding {
            self.indices.push(base + offset);
        }
        Ok(())
    }
}

/// Flips a face normal so an inverted hull shades and culls as a cel outline.
fn orient(normal: [f64; 3], inverted: bool) -> [f64; 3] {
    if inverted {
        [-normal[0], -normal[1], -normal[2]]
    } else {
        normal
    }
}

/// Face frames as `(normal, u, v)` where `u` cross `v` equals `normal`.
const BOX_FACES: [([f64; 3], [f64; 3], [f64; 3]); 6] = [
    ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
    ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
    ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
    ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
];

fn extent_along(half_extents: [f64; 3], axis: [f64; 3]) -> f64 {
    half_extents[0] * axis[0].abs()
        + half_extents[1] * axis[1].abs()
        + half_extents[2] * axis[2].abs()
}

fn combine(base: [f64; 3], axis: [f64; 3], scale: f64) -> [f64; 3] {
    [
        base[0] + axis[0] * scale,
        base[1] + axis[1] * scale,
        base[2] + axis[2] * scale,
    ]
}

fn tessellate_box(
    geometry: &mut RoleGeometry,
    center: [f64; 3],
    half_extents: [f64; 3],
    joint: u16,
    inverted: bool,
) -> Result<(), &'static str> {
    for (normal, u_axis, v_axis) in BOX_FACES {
        let face_center = combine(center, normal, extent_along(half_extents, normal));
        let half_u = extent_along(half_extents, u_axis);
        let half_v = extent_along(half_extents, v_axis);
        let mut corners = [[0.0; 3]; 4];
        for (corner, (u_sign, v_sign)) in
            corners
                .iter_mut()
                .zip([(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)])
        {
            let point = combine(face_center, u_axis, u_sign * half_u);
            *corner = combine(point, v_axis, v_sign * half_v);
        }
        geometry.push_face(corners, normal, joint, inverted)?;
    }
    Ok(())
}

fn cylinder_frame(axis: Axis) -> ([f64; 3], [f64; 3], [f64; 3]) {
    match axis {
        Axis::X => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        Axis::Y => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        Axis::Z => ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    }
}

struct CylinderSpec {
    center: [f64; 3],
    radius: f64,
    half_height: f64,
    axis: Axis,
    segments: u32,
}

fn tessellate_cylinder(
    geometry: &mut RoleGeometry,
    spec: &CylinderSpec,
    joint: u16,
    inverted: bool,
) -> Result<(), &'static str> {
    let (axis, u_axis, v_axis) = cylinder_frame(spec.axis);
    let segments = spec.segments as usize;
    let angle = |index: usize| std::f64::consts::TAU * (index % segments) as f64 / segments as f64;
    let ring = |index: usize, side: f64| {
        let theta = angle(index);
        let base = combine(spec.center, axis, side * spec.half_height);
        let point = combine(base, u_axis, theta.cos() * spec.radius);
        combine(point, v_axis, theta.sin() * spec.radius)
    };

    for index in 0..segments {
        let mid = (angle(index)
            + angle(index + 1)
            + if index + 1 == segments {
                std::f64::consts::TAU
            } else {
                0.0
            })
            * 0.5;
        let normal = combine(combine([0.0; 3], u_axis, mid.cos()), v_axis, mid.sin());
        let corners = [
            ring(index, -1.0),
            ring(index + 1, -1.0),
            ring(index + 1, 1.0),
            ring(index, 1.0),
        ];
        geometry.push_face(corners, normal, joint, inverted)?;
    }

    for (side, normal) in [(1.0, axis), (-1.0, [-axis[0], -axis[1], -axis[2]])] {
        let cap_center = combine(spec.center, axis, side * spec.half_height);
        for index in 0..segments {
            let (first, second) = if side > 0.0 {
                (ring(index, side), ring(index + 1, side))
            } else {
                (ring(index + 1, side), ring(index, side))
            };
            geometry.push_triangle([cap_center, first, second], normal, joint, inverted)?;
        }
    }

    Ok(())
}

fn instance_offsets(repeat: &[RepeatSource]) -> Vec<[f64; 3]> {
    let mut offsets = vec![[0.0, 0.0, 0.0]];
    for level in repeat {
        let mut expanded = Vec::with_capacity(offsets.len() * level.count as usize);
        for base in &offsets {
            for step_index in 0..level.count {
                let scale = f64::from(step_index);
                expanded.push([
                    base[0] + level.step[0] * scale,
                    base[1] + level.step[1] * scale,
                    base[2] + level.step[2] * scale,
                ]);
            }
        }
        offsets = expanded;
    }
    offsets
}

struct ModuleGeometry {
    roles: BTreeMap<usize, RoleGeometry>,
}

fn build_module_geometry(
    module: &ModuleSource,
    layout: Option<&RigLayout>,
    path: &str,
    module_field: &str,
) -> Result<ModuleGeometry, AssetGenError> {
    let mut roles: BTreeMap<usize, RoleGeometry> = BTreeMap::new();

    for (shape_index, shape) in module.shapes.iter().enumerate() {
        let field = format!("{module_field}.shapes[{shape_index}]");
        let invalid = |message: String| AssetGenError::Invalid {
            path: path.to_owned(),
            field: format!("{field}.bone"),
            message,
        };
        // Authored positions stay in model space: glTF skinning multiplies each
        // vertex by `global joint transform * inverse bind matrix`, which is the
        // identity in the rest pose. Rebasing here would collapse the figure.
        let joint = match (layout, &shape.bone) {
            (Some(layout), Some(bone)) => layout
                .order
                .get(bone)
                .copied()
                .ok_or_else(|| invalid(format!("unknown bone {bone}")))?,
            (None, None) => UNSKINNED_JOINT,
            (Some(_), None) => {
                return Err(invalid(
                    "a rigged module requires every shape to declare a bone".to_owned(),
                ));
            }
            (None, Some(bone)) => {
                return Err(invalid(format!(
                    "shape declares bone {bone} but the module has no rig"
                )));
            }
        };

        for translation in instance_offsets(&shape.repeat) {
            emit_shape(&mut roles, shape, translation, joint, 0.0, shape.role).map_err(
                |message| AssetGenError::Invalid {
                    path: path.to_owned(),
                    field: field.clone(),
                    message: message.to_owned(),
                },
            )?;

            if let Some(expansion) = shape.outline {
                emit_shape(
                    &mut roles,
                    shape,
                    translation,
                    joint,
                    expansion,
                    PaletteRole::Ink,
                )
                .map_err(|message| AssetGenError::Invalid {
                    path: path.to_owned(),
                    field: format!("{field}.outline"),
                    message: message.to_owned(),
                })?;
            }
        }
    }

    Ok(ModuleGeometry { roles })
}

fn emit_shape(
    roles: &mut BTreeMap<usize, RoleGeometry>,
    shape: &ShapeSource,
    translation: [f64; 3],
    joint: u16,
    expansion: f64,
    role: PaletteRole,
) -> Result<(), &'static str> {
    let inverted = expansion > 0.0;
    let geometry = roles.entry(role_index(role)).or_default();
    match shape.primitive {
        PrimitiveSource::Box {
            center,
            half_extents,
        } => tessellate_box(
            geometry,
            [
                center[0] + translation[0],
                center[1] + translation[1],
                center[2] + translation[2],
            ],
            [
                half_extents[0] + expansion,
                half_extents[1] + expansion,
                half_extents[2] + expansion,
            ],
            joint,
            inverted,
        ),
        PrimitiveSource::Cylinder {
            center,
            radius,
            half_height,
            axis,
            segments,
        } => tessellate_cylinder(
            geometry,
            &CylinderSpec {
                center: [
                    center[0] + translation[0],
                    center[1] + translation[1],
                    center[2] + translation[2],
                ],
                radius: radius + expansion,
                half_height: half_height + expansion,
                axis,
                segments,
            },
            joint,
            inverted,
        ),
    }
}

// ---------------------------------------------------------------------------
// Deterministic glTF assembly
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Builder {
    root: json::Root,
    bin: Vec<u8>,
}

impl Builder {
    fn push_view(
        &mut self,
        bytes: &[u8],
        target: Option<json::buffer::Target>,
    ) -> json::Index<json::buffer::View> {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        json::Index::push(
            &mut self.root.buffer_views,
            json::buffer::View {
                buffer: json::Index::new(0),
                byte_length: USize64(bytes.len() as u64),
                byte_offset: Some(USize64(offset as u64)),
                byte_stride: None,
                name: None,
                target: target.map(Checked::Valid),
                extensions: None,
                extras: json::Extras::default(),
            },
        )
    }

    fn push_accessor(&mut self, request: AccessorRequest<'_>) -> json::Index<json::Accessor> {
        let view = self.push_view(request.bytes, request.target);
        json::Index::push(
            &mut self.root.accessors,
            json::Accessor {
                buffer_view: Some(view),
                byte_offset: Some(USize64(0)),
                count: USize64(request.count as u64),
                component_type: Checked::Valid(json::accessor::GenericComponentType(
                    request.component_type,
                )),
                extensions: None,
                extras: json::Extras::default(),
                type_: Checked::Valid(request.data_type),
                min: request.min,
                max: request.max,
                name: None,
                normalized: false,
                sparse: None,
            },
        )
    }
}

struct AccessorRequest<'a> {
    bytes: &'a [u8],
    target: Option<json::buffer::Target>,
    count: usize,
    component_type: json::accessor::ComponentType,
    data_type: json::accessor::Type,
    min: Option<serde_json::Value>,
    max: Option<serde_json::Value>,
}

fn vec3_bytes(values: &[[f32; 3]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 12);
    for value in values {
        for component in value {
            bytes.extend_from_slice(&component.to_le_bytes());
        }
    }
    bytes
}

fn vec4_bytes(values: &[[f32; 4]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 16);
    for value in values {
        for component in value {
            bytes.extend_from_slice(&component.to_le_bytes());
        }
    }
    bytes
}

fn scalar_bytes(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn index_bytes(values: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 2);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn joint_bytes(values: &[u16]) -> Result<Vec<u8>, u16> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        let slot = u8::try_from(*value).map_err(|_| *value)?;
        bytes.push(slot);
        bytes.extend_from_slice(&[0, 0, 0]);
    }
    Ok(bytes)
}

fn matrix_bytes(values: &[[f32; 16]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 64);
    for value in values {
        for component in value {
            bytes.extend_from_slice(&component.to_le_bytes());
        }
    }
    bytes
}

fn bounds(values: &[[f32; 3]]) -> (serde_json::Value, serde_json::Value) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for value in values {
        for axis in 0..3 {
            min[axis] = min[axis].min(value[axis]);
            max[axis] = max[axis].max(value[axis]);
        }
    }
    (
        serde_json::Value::from(min.to_vec()),
        serde_json::Value::from(max.to_vec()),
    )
}

fn scalar_bounds(values: &[f32]) -> (serde_json::Value, serde_json::Value) {
    let min = values.iter().copied().fold(f32::INFINITY, f32::min);
    let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    (
        serde_json::Value::from(vec![min]),
        serde_json::Value::from(vec![max]),
    )
}

fn build_glb(source: &AssetSource, path: &str) -> Result<Vec<u8>, AssetGenError> {
    let mut builder = Builder::default();
    builder.root.asset = json::Asset {
        copyright: None,
        extensions: None,
        extras: json::Extras::default(),
        generator: Some(GENERATOR_NAME.to_owned()),
        min_version: None,
        version: "2.0".to_owned(),
    };
    builder.root.extensions_used = vec!["KHR_materials_unlit".to_owned()];

    let mut materials: BTreeMap<usize, json::Index<json::Material>> = BTreeMap::new();
    let mut total_triangles = 0usize;

    for (module_index, module) in source.modules.iter().enumerate() {
        let module_field = format!("modules[{module_index}]");
        let layout = match &module.rig {
            Some(rig) => Some(rig_layout(rig, path, &module_field)?),
            None => None,
        };
        let geometry = build_module_geometry(module, layout.as_ref(), path, &module_field)?;
        total_triangles += geometry
            .roles
            .values()
            .map(|role| role.indices.len() / 3)
            .sum::<usize>();
        if total_triangles > MAX_TRIANGLES_PER_ASSET {
            return Err(AssetGenError::Invalid {
                path: path.to_owned(),
                field: format!("{module_field}.shapes"),
                message: format!(
                    "{total_triangles} triangles exceed the {MAX_TRIANGLES_PER_ASSET} triangle \
                     budget for one asset"
                ),
            });
        }
        let skinned = module.rig.is_some();
        let mesh = push_mesh(
            &mut builder,
            &mut materials,
            &geometry,
            &format!("{}-mesh", module.name),
            skinned,
            path,
            &module_field,
        )?;

        let root_index = builder.root.nodes.len();
        builder.root.nodes.push(json::Node::default());
        let mut children = Vec::new();

        if let (Some(rig), Some(layout)) = (&module.rig, layout.as_ref()) {
            let skin_index = json::Index::push(
                &mut builder.root.nodes,
                json::Node {
                    mesh: Some(mesh),
                    name: Some(rig.skin_node.clone()),
                    ..json::Node::default()
                },
            );
            children.push(skin_index);

            let bone_base = builder.root.nodes.len();
            for bone in &rig.bones {
                builder.root.nodes.push(json::Node {
                    name: Some(bone.name.clone()),
                    translation: Some(quantize_triple(bone.translation)),
                    ..json::Node::default()
                });
            }
            for (bone_index, bone) in rig.bones.iter().enumerate() {
                let Some(parent) = &bone.parent else {
                    continue;
                };
                let parent_index =
                    layout
                        .order
                        .get(parent)
                        .copied()
                        .ok_or_else(|| AssetGenError::Invalid {
                            path: path.to_owned(),
                            field: format!("{module_field}.rig.bones"),
                            message: format!(
                                "bone {} references unresolved parent {parent}",
                                bone.name
                            ),
                        })? as usize;
                let child = json::Index::new((bone_base + bone_index) as u32);
                builder.root.nodes[bone_base + parent_index]
                    .children
                    .get_or_insert_with(Vec::new)
                    .push(child);
            }
            let root_bone = rig
                .bones
                .iter()
                .position(|bone| bone.parent.is_none())
                .ok_or_else(|| AssetGenError::Invalid {
                    path: path.to_owned(),
                    field: format!("{module_field}.rig.bones"),
                    message: "rig has no root bone".to_owned(),
                })?;
            children.push(json::Index::new((bone_base + root_bone) as u32));

            let matrices = rig
                .bones
                .iter()
                .map(|bone| {
                    layout
                        .origins
                        .get(&bone.name)
                        .copied()
                        .map(inverse_bind_matrix)
                        .ok_or_else(|| AssetGenError::Invalid {
                            path: path.to_owned(),
                            field: format!("{module_field}.rig.bones"),
                            message: format!("bone {} has no resolved bind origin", bone.name),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let inverse_bind = builder.push_accessor(AccessorRequest {
                bytes: &matrix_bytes(&matrices),
                target: None,
                count: matrices.len(),
                component_type: json::accessor::ComponentType::F32,
                data_type: json::accessor::Type::Mat4,
                min: None,
                max: None,
            });
            let skin = json::Index::push(
                &mut builder.root.skins,
                json::Skin {
                    extensions: None,
                    extras: json::Extras::default(),
                    inverse_bind_matrices: Some(inverse_bind),
                    joints: (0..rig.bones.len())
                        .map(|offset| json::Index::new((bone_base + offset) as u32))
                        .collect(),
                    name: Some(rig.skin_node.clone()),
                    skeleton: Some(json::Index::new((bone_base + root_bone) as u32)),
                },
            );
            builder.root.nodes[skin_index.value()].skin = Some(skin);

            push_animations(&mut builder, rig, bone_base, path, &module_field)?;
        }

        builder.root.nodes[root_index] = json::Node {
            children: (!children.is_empty()).then_some(children),
            mesh: (!skinned).then_some(mesh),
            name: Some(module.name.clone()),
            ..json::Node::default()
        };
        json::Index::push(
            &mut builder.root.scenes,
            json::Scene {
                extensions: None,
                extras: json::Extras::default(),
                name: Some(module.name.clone()),
                nodes: vec![json::Index::new(root_index as u32)],
            },
        );
    }

    builder.root.scene = Some(json::Index::new(0));
    builder.root.buffers = vec![json::Buffer {
        byte_length: USize64(builder.bin.len() as u64),
        extensions: None,
        extras: json::Extras::default(),
        name: None,
        uri: None,
    }];

    let json_bytes = serde_json::to_vec(&builder.root).map_err(|error| AssetGenError::Invalid {
        path: path.to_owned(),
        field: "modules".to_owned(),
        message: format!("glTF document could not be serialized: {error}"),
    })?;

    Ok(glb_container(&json_bytes, &builder.bin))
}

/// Resolved rest-pose data for one rig: each bone's global bind origin and its
/// joint index.
struct RigLayout {
    origins: BTreeMap<String, [f64; 3]>,
    order: BTreeMap<String, u16>,
}

fn rig_layout(rig: &RigSource, path: &str, module_field: &str) -> Result<RigLayout, AssetGenError> {
    let rig_field = format!("{module_field}.rig");
    let mut origins = BTreeMap::new();
    let mut order = BTreeMap::new();
    for (index, bone) in rig.bones.iter().enumerate() {
        let parent_origin = match &bone.parent {
            None => [0.0, 0.0, 0.0],
            Some(parent) => origins
                .get(parent)
                .copied()
                .ok_or_else(|| AssetGenError::Invalid {
                    path: path.to_owned(),
                    field: format!("{rig_field}.bones[{index}].parent"),
                    message: format!("unknown or forward referenced parent bone {parent}"),
                })?,
        };
        origins.insert(
            bone.name.clone(),
            [
                parent_origin[0] + bone.translation[0],
                parent_origin[1] + bone.translation[1],
                parent_origin[2] + bone.translation[2],
            ],
        );
        let slot = u16::try_from(index).map_err(|_| AssetGenError::Invalid {
            path: path.to_owned(),
            field: format!("{rig_field}.bones"),
            message: format!(
                "{} bones exceed the {MAX_BONES_PER_RIG} bone budget",
                rig.bones.len()
            ),
        })?;
        order.insert(bone.name.clone(), slot);
    }
    Ok(RigLayout { origins, order })
}

fn inverse_bind_matrix(origin: [f64; 3]) -> [f32; 16] {
    [
        1.0,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        quantize(-origin[0]),
        quantize(-origin[1]),
        quantize(-origin[2]),
        1.0,
    ]
}

fn push_mesh(
    builder: &mut Builder,
    materials: &mut BTreeMap<usize, json::Index<json::Material>>,
    geometry: &ModuleGeometry,
    mesh_name: &str,
    skinned: bool,
    path: &str,
    module_field: &str,
) -> Result<json::Index<json::Mesh>, AssetGenError> {
    let mut primitives = Vec::with_capacity(geometry.roles.len());
    for (role_slot, role_geometry) in &geometry.roles {
        let role = PaletteRole::ALL[*role_slot];
        let material = *materials.entry(*role_slot).or_insert_with(|| {
            // glTF stores `baseColorFactor` in linear space, so the typed sRGB
            // palette has to be converted here. Writing the sRGB channels
            // straight through would make every generated surface render far
            // brighter than the authored role, which is exactly what the ink,
            // shadow, and luminance frame contracts refuse.
            let color = LinearRgba::from(role.color());
            json::Index::push(
                &mut builder.root.materials,
                json::Material {
                    alpha_cutoff: None,
                    alpha_mode: Checked::Valid(json::material::AlphaMode::Opaque),
                    double_sided: false,
                    name: Some(role_name(role)),
                    pbr_metallic_roughness: json::material::PbrMetallicRoughness {
                        base_color_factor: json::material::PbrBaseColorFactor([
                            color.red,
                            color.green,
                            color.blue,
                            color.alpha,
                        ]),
                        base_color_texture: None,
                        metallic_factor: json::material::StrengthFactor(0.0),
                        roughness_factor: json::material::StrengthFactor(1.0),
                        metallic_roughness_texture: None,
                        extensions: None,
                        extras: json::Extras::default(),
                    },
                    normal_texture: None,
                    occlusion_texture: None,
                    emissive_texture: None,
                    emissive_factor: json::material::EmissiveFactor([0.0, 0.0, 0.0]),
                    extensions: Some(json::extensions::material::Material {
                        unlit: Some(json::extensions::material::Unlit {}),
                        ..Default::default()
                    }),
                    extras: json::Extras::default(),
                },
            )
        });

        let (min, max) = bounds(&role_geometry.positions);
        let position = builder.push_accessor(AccessorRequest {
            bytes: &vec3_bytes(&role_geometry.positions),
            target: Some(json::buffer::Target::ArrayBuffer),
            count: role_geometry.positions.len(),
            component_type: json::accessor::ComponentType::F32,
            data_type: json::accessor::Type::Vec3,
            min: Some(min),
            max: Some(max),
        });
        let normal = builder.push_accessor(AccessorRequest {
            bytes: &vec3_bytes(&role_geometry.normals),
            target: Some(json::buffer::Target::ArrayBuffer),
            count: role_geometry.normals.len(),
            component_type: json::accessor::ComponentType::F32,
            data_type: json::accessor::Type::Vec3,
            min: None,
            max: None,
        });
        let indices = builder.push_accessor(AccessorRequest {
            bytes: &index_bytes(&role_geometry.indices),
            target: Some(json::buffer::Target::ElementArrayBuffer),
            count: role_geometry.indices.len(),
            component_type: json::accessor::ComponentType::U16,
            data_type: json::accessor::Type::Scalar,
            min: None,
            max: None,
        });

        let mut attributes = BTreeMap::new();
        attributes.insert(Checked::Valid(json::mesh::Semantic::Positions), position);
        attributes.insert(Checked::Valid(json::mesh::Semantic::Normals), normal);

        if skinned {
            let joint_data =
                joint_bytes(&role_geometry.joints).map_err(|joint| AssetGenError::Invalid {
                    path: path.to_owned(),
                    field: format!("{module_field}.rig.bones"),
                    message: format!(
                        "joint index {joint} does not fit the unsigned byte JOINTS_0 encoding; a \
                         rig may declare at most {MAX_BONES_PER_RIG} bones"
                    ),
                })?;
            let joints = builder.push_accessor(AccessorRequest {
                bytes: &joint_data,
                target: Some(json::buffer::Target::ArrayBuffer),
                count: role_geometry.joints.len(),
                component_type: json::accessor::ComponentType::U8,
                data_type: json::accessor::Type::Vec4,
                min: None,
                max: None,
            });
            let weights_data = vec![RIGID_WEIGHTS; role_geometry.joints.len()];
            let weights = builder.push_accessor(AccessorRequest {
                bytes: &vec4_bytes(&weights_data),
                target: Some(json::buffer::Target::ArrayBuffer),
                count: weights_data.len(),
                component_type: json::accessor::ComponentType::F32,
                data_type: json::accessor::Type::Vec4,
                min: None,
                max: None,
            });
            attributes.insert(Checked::Valid(json::mesh::Semantic::Joints(0)), joints);
            attributes.insert(Checked::Valid(json::mesh::Semantic::Weights(0)), weights);
        }

        primitives.push(json::mesh::Primitive {
            attributes,
            extensions: None,
            extras: json::Extras::default(),
            indices: Some(indices),
            material: Some(material),
            mode: Checked::Valid(json::mesh::Mode::Triangles),
            targets: None,
        });
    }

    Ok(json::Index::push(
        &mut builder.root.meshes,
        json::Mesh {
            extensions: None,
            extras: json::Extras::default(),
            name: Some(mesh_name.to_owned()),
            primitives,
            weights: None,
        },
    ))
}

fn push_animations(
    builder: &mut Builder,
    rig: &RigSource,
    bone_base: usize,
    path: &str,
    module_field: &str,
) -> Result<(), AssetGenError> {
    for (clip_index, clip) in rig.clips.iter().enumerate() {
        let mut channels = Vec::with_capacity(clip.tracks.len());
        let mut samplers = Vec::with_capacity(clip.tracks.len());

        for (track_index, track) in clip.tracks.iter().enumerate() {
            let bone_index = rig
                .bones
                .iter()
                .position(|bone| bone.name == track.bone)
                .ok_or_else(|| AssetGenError::Invalid {
                    path: path.to_owned(),
                    field: format!(
                        "{module_field}.rig.clips[{clip_index}].tracks[{track_index}].bone"
                    ),
                    message: format!("unknown animation target bone {}", track.bone),
                })?;
            let rest = rig.bones[bone_index].translation;

            let times = track
                .keys
                .iter()
                .map(|key| quantize(key.time))
                .collect::<Vec<_>>();
            let (min, max) = scalar_bounds(&times);
            let input = builder.push_accessor(AccessorRequest {
                bytes: &scalar_bytes(&times),
                target: None,
                count: times.len(),
                component_type: json::accessor::ComponentType::F32,
                data_type: json::accessor::Type::Scalar,
                min: Some(min),
                max: Some(max),
            });

            let (output, property) = match track.channel {
                ChannelSource::Translation => {
                    let values = track
                        .keys
                        .iter()
                        .map(|key| {
                            quantize_triple([
                                rest[0] + key.value[0],
                                rest[1] + key.value[1],
                                rest[2] + key.value[2],
                            ])
                        })
                        .collect::<Vec<_>>();
                    let accessor = builder.push_accessor(AccessorRequest {
                        bytes: &vec3_bytes(&values),
                        target: None,
                        count: values.len(),
                        component_type: json::accessor::ComponentType::F32,
                        data_type: json::accessor::Type::Vec3,
                        min: None,
                        max: None,
                    });
                    (accessor, json::animation::Property::Translation)
                }
                ChannelSource::Rotation => {
                    let values = track
                        .keys
                        .iter()
                        .map(|key| quaternion_from_euler_degrees(key.value))
                        .collect::<Vec<_>>();
                    let accessor = builder.push_accessor(AccessorRequest {
                        bytes: &vec4_bytes(&values),
                        target: None,
                        count: values.len(),
                        component_type: json::accessor::ComponentType::F32,
                        data_type: json::accessor::Type::Vec4,
                        min: None,
                        max: None,
                    });
                    (accessor, json::animation::Property::Rotation)
                }
            };

            let sampler_index = samplers.len() as u32;
            samplers.push(json::animation::Sampler {
                extensions: None,
                extras: json::Extras::default(),
                input,
                interpolation: Checked::Valid(json::animation::Interpolation::Linear),
                output,
            });
            channels.push(json::animation::Channel {
                sampler: json::Index::new(sampler_index),
                target: json::animation::Target {
                    extensions: None,
                    extras: json::Extras::default(),
                    node: json::Index::new((bone_base + bone_index) as u32),
                    path: Checked::Valid(property),
                },
                extensions: None,
                extras: json::Extras::default(),
            });
        }

        json::Index::push(
            &mut builder.root.animations,
            json::Animation {
                extensions: None,
                extras: json::Extras::default(),
                channels,
                name: Some(clip.name.clone()),
                samplers,
            },
        );
    }
    Ok(())
}

fn glb_container(json_chunk: &[u8], bin_chunk: &[u8]) -> Vec<u8> {
    let json_padding = (4 - json_chunk.len() % 4) % 4;
    let bin_padding = (4 - bin_chunk.len() % 4) % 4;
    let json_length = json_chunk.len() + json_padding;
    let bin_length = bin_chunk.len() + bin_padding;
    let total = 12 + 8 + json_length + 8 + bin_length;

    let mut output = Vec::with_capacity(total);
    output.extend_from_slice(b"glTF");
    output.extend_from_slice(&2u32.to_le_bytes());
    output.extend_from_slice(&(total as u32).to_le_bytes());

    output.extend_from_slice(&(json_length as u32).to_le_bytes());
    output.extend_from_slice(b"JSON");
    output.extend_from_slice(json_chunk);
    output.extend(std::iter::repeat_n(b' ', json_padding));

    output.extend_from_slice(&(bin_length as u32).to_le_bytes());
    output.extend_from_slice(b"BIN\0");
    output.extend_from_slice(bin_chunk);
    output.extend(std::iter::repeat_n(0u8, bin_padding));

    output
}
