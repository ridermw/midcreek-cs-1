//! The measurement engine.
//!
//! Everything that turns pixels into numbers, and the numbers a frame must
//! satisfy. This is deliberately separate from `verification`, which drives the
//! game through its journey and decides what to do with the verdict: the engine
//! here reads an image and nothing else, so it can be pointed at approved
//! reference art just as easily as at a captured frame.
//!
//! ```text
//!   PNG ──▶ load_frame ──▶ RgbImage
//!                             │
//!                             ▼
//!                    FrameMetrics::compute ──▶ one traversal, per pixel:
//!                             │                  · linear luminance
//!                             │                  · nearest palette role
//!                             │                  · within-tolerance roles
//!                             │                  · Sobel edge magnitude
//!                             │                  · edge orientation band
//!                             ▼
//!                        FrameMetrics ──▶ whole frame + named regions
//! ```
//!
//! This module holds no gate policy. Every numeric bound a fidelity gate is
//! derived from lives in `docs/reference/fidelity.json` and reaches the code
//! through `crate::reference`, so a recalibration is a reviewed edit to a
//! frozen document rather than a constant sitting inside the engine that
//! measures the thing being judged. What remains here is measurement
//! mechanics: the palette table, the matching tolerance the nearest-role
//! classification is defined by, the Sobel threshold, and the stable region
//! names.

use std::{collections::BTreeMap, sync::OnceLock};

use image::RgbImage;
use serde::Serialize;
use std::path::Path;

use crate::design::{KEY_ART_REFERENCE_PATH, PaletteRole};

// ---------------------------------------------------------------------------
// Frame analysis
// ---------------------------------------------------------------------------

/// A pixel rectangle inside one frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelRect {
    /// Left edge, in pixels.
    pub x: u32,
    /// Top edge, in pixels.
    pub y: u32,
    /// Width, in pixels.
    pub width: u32,
    /// Height, in pixels.
    pub height: u32,
}

impl PixelRect {
    /// Whether one pixel is inside.
    pub const fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    /// How many pixels the rectangle covers.
    pub const fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

/// Everything one named region of a frame measured.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RegionMetrics {
    /// How many pixels the region covered.
    pub pixels: u64,
    /// Fraction of the region within [`PALETTE_TOLERANCE`] of each role.
    pub near_role_ratio: BTreeMap<PaletteRole, f64>,
    /// Mean linear luminance inside the region.
    pub mean_linear_luminance: f64,
    /// Fraction of the region holding the magenta sentinel.
    pub sentinel_ratio: f64,
}

impl RegionMetrics {
    /// Fraction of the region within tolerance of one role.
    pub fn near(&self, role: PaletteRole) -> f64 {
        self.near_role_ratio.get(&role).copied().unwrap_or(0.0)
    }
}

/// Euclidean RGB distance at which a pixel still counts as one palette role.
pub const PALETTE_TOLERANCE: f64 = 24.0;

/// Gradient magnitude, in 0..1 grey units, above which an edge counts.
pub const STRONG_EDGE: f64 = 0.10;

/// Everything one frame measured, computed in a single traversal.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FrameMetrics {
    /// Frame width, in pixels.
    pub width: u32,
    /// Frame height, in pixels.
    pub height: u32,
    /// Total pixels.
    pub pixels: u64,
    /// Mean linear luminance over the whole frame.
    pub mean_linear_luminance: f64,
    /// Fraction of the frame holding the magenta sentinel.
    pub sentinel_ratio: f64,
    /// Fraction of the frame within [`PALETTE_TOLERANCE`] of any approved role.
    pub palette_ratio: f64,
    /// Nearest-role histogram, normalized; sums to one.
    pub nearest_role_ratio: BTreeMap<PaletteRole, f64>,
    /// Fraction of the frame within tolerance of each role.
    pub near_role_ratio: BTreeMap<PaletteRole, f64>,
    /// Fraction of pixels carrying a strong edge.
    pub edge_density: f64,
    /// Share of strong-edge mass whose edge direction lies in 30..50 degrees.
    pub diagonal_band_low: f64,
    /// Share of strong-edge mass whose edge direction lies in 130..150 degrees.
    pub diagonal_band_high: f64,
    /// Per-region measurements, by stable region name.
    pub regions: BTreeMap<String, RegionMetrics>,
}

impl FrameMetrics {
    /// Fraction of the frame within tolerance of one role.
    pub fn near(&self, role: PaletteRole) -> f64 {
        self.near_role_ratio.get(&role).copied().unwrap_or(0.0)
    }

    /// Fraction of the frame classified nearest to one role.
    pub fn nearest(&self, role: PaletteRole) -> f64 {
        self.nearest_role_ratio.get(&role).copied().unwrap_or(0.0)
    }

    /// Combined fraction of several roles, by nearest classification.
    pub fn nearest_of(&self, roles: &[PaletteRole]) -> f64 {
        roles.iter().map(|role| self.nearest(*role)).sum()
    }

    /// L1 distance between two nearest-role histograms.
    pub fn histogram_distance(&self, other: &Self) -> f64 {
        PaletteRole::ALL
            .iter()
            .map(|role| (self.nearest(*role) - other.nearest(*role)).abs())
            .sum()
    }

    /// One measured region.
    pub fn region(&self, name: &str) -> Option<&RegionMetrics> {
        self.regions.get(name)
    }

    /// Measures one decoded frame and every supplied region in one traversal.
    ///
    /// Colour statistics, the sentinel ratio, the nearest-role histogram, the
    /// per-region accumulators, and the Sobel edge orientation histogram are
    /// all produced by the same walk over the image; nothing is re-read.
    pub fn compute(image: &RgbImage, regions: &BTreeMap<String, PixelRect>) -> Self {
        let width = image.width();
        let height = image.height();
        let pixels = u64::from(width) * u64::from(height);
        let palette = palette_table();
        let luminance = linear_luminance_table();

        let mut luminance_sum = 0.0;
        let mut sentinel = 0u64;
        let mut palette_hits = 0u64;
        let mut nearest = [0u64; PaletteRole::ALL.len()];
        let mut near = [0u64; PaletteRole::ALL.len()];
        let mut edge_pixels = 0u64;
        let mut edge_mass = 0.0;
        let mut band_low = 0.0;
        let mut band_high = 0.0;
        // The diagonal windows are centred on the authority's projected
        // ground-axis angle rather than on a literal, so a camera that matches
        // the reference lands its rows mid-window instead of at the edge.
        let (low_window, high_window) = crate::reference::diagonal_band_windows();

        let names = regions.keys().cloned().collect::<Vec<_>>();
        let rects = names.iter().map(|name| regions[name]).collect::<Vec<_>>();
        let mut region_luminance = vec![0.0f64; rects.len()];
        let mut region_sentinel = vec![0u64; rects.len()];
        let mut region_near = vec![[0u64; PaletteRole::ALL.len()]; rects.len()];
        let mut region_pixels = vec![0u64; rects.len()];

        let raw = image.as_raw();
        let stride = width as usize * 3;
        let mut active = Vec::with_capacity(rects.len());
        for y in 0..height {
            let row = y as usize * stride;
            // Regions are resolved per row rather than per pixel: a frame
            // carries one crop, a handful of HUD panels and badges, and one
            // rectangle per projected equipment segment, and testing every one
            // of them against every pixel would dominate the walk.
            active.clear();
            active.extend(
                rects
                    .iter()
                    .enumerate()
                    .filter(|(_, rect)| y >= rect.y && y < rect.y + rect.height)
                    .map(|(slot, rect)| (slot, *rect)),
            );
            for x in 0..width {
                let index = row + x as usize * 3;
                let red = raw[index];
                let green = raw[index + 1];
                let blue = raw[index + 2];

                let pixel_luminance = 0.2126 * luminance[red as usize]
                    + 0.7152 * luminance[green as usize]
                    + 0.0722 * luminance[blue as usize];
                luminance_sum += pixel_luminance;

                let is_sentinel = red >= 240 && green <= 24 && blue >= 240;
                if is_sentinel {
                    sentinel += 1;
                }

                // The palette is scanned once per pixel. The tolerance hits
                // are kept as a bit mask so every region this pixel falls in
                // reuses that one scan instead of repeating it.
                let mut best = f64::INFINITY;
                let mut best_role = 0usize;
                let mut near_mask = 0u32;
                for (role, colour) in palette.iter().enumerate() {
                    let distance = squared_distance(red, green, blue, *colour);
                    if distance < best {
                        best = distance;
                        best_role = role;
                    }
                    if distance <= PALETTE_TOLERANCE * PALETTE_TOLERANCE {
                        near[role] += 1;
                        near_mask |= 1 << role;
                    }
                }
                nearest[best_role] += 1;
                if best <= PALETTE_TOLERANCE * PALETTE_TOLERANCE {
                    palette_hits += 1;
                }

                for (slot, rect) in &active {
                    if x < rect.x || x >= rect.x + rect.width {
                        continue;
                    }
                    let slot = *slot;
                    region_pixels[slot] += 1;
                    region_luminance[slot] += pixel_luminance;
                    if is_sentinel {
                        region_sentinel[slot] += 1;
                    }
                    let mut remaining = near_mask;
                    while remaining != 0 {
                        let role = remaining.trailing_zeros() as usize;
                        remaining &= remaining - 1;
                        region_near[slot][role] += 1;
                    }
                }

                if x == 0 || y == 0 || x + 1 >= width || y + 1 >= height {
                    continue;
                }
                let grey = |dx: i32, dy: i32| -> f64 {
                    let sx = (x as i32 + dx) as usize;
                    let sy = (y as i32 + dy) as usize;
                    let at = sy * stride + sx * 3;
                    (0.299 * f64::from(raw[at])
                        + 0.587 * f64::from(raw[at + 1])
                        + 0.114 * f64::from(raw[at + 2]))
                        / 255.0
                };
                let gradient_x = (grey(1, -1) + 2.0 * grey(1, 0) + grey(1, 1))
                    - (grey(-1, -1) + 2.0 * grey(-1, 0) + grey(-1, 1));
                let gradient_y = (grey(-1, 1) + 2.0 * grey(0, 1) + grey(1, 1))
                    - (grey(-1, -1) + 2.0 * grey(0, -1) + grey(1, -1));
                let magnitude = (gradient_x * gradient_x + gradient_y * gradient_y).sqrt() / 4.0;
                if magnitude < STRONG_EDGE {
                    continue;
                }
                edge_pixels += 1;
                edge_mass += magnitude;
                // Screen space runs y downwards; the edge itself is the
                // gradient turned a quarter turn, and only its axis matters.
                let direction =
                    (gradient_x.atan2(-gradient_y).to_degrees() + 180.0).rem_euclid(180.0);
                if low_window.contains(&direction) {
                    band_low += magnitude;
                } else if high_window.contains(&direction) {
                    band_high += magnitude;
                }
            }
        }

        let total = pixels.max(1) as f64;
        let ratios = |counts: &[u64; PaletteRole::ALL.len()], divisor: f64| {
            PaletteRole::ALL
                .iter()
                .enumerate()
                .map(|(index, role)| (*role, counts[index] as f64 / divisor))
                .collect::<BTreeMap<_, _>>()
        };

        let mut measured = BTreeMap::new();
        for (slot, name) in names.into_iter().enumerate() {
            let count = region_pixels[slot].max(1) as f64;
            measured.insert(
                name,
                RegionMetrics {
                    pixels: region_pixels[slot],
                    near_role_ratio: ratios(&region_near[slot], count),
                    mean_linear_luminance: region_luminance[slot] / count,
                    sentinel_ratio: region_sentinel[slot] as f64 / count,
                },
            );
        }

        let mass = if edge_mass > 0.0 { edge_mass } else { 1.0 };
        Self {
            width,
            height,
            pixels,
            mean_linear_luminance: luminance_sum / total,
            sentinel_ratio: sentinel as f64 / total,
            palette_ratio: palette_hits as f64 / total,
            nearest_role_ratio: ratios(&nearest, total),
            near_role_ratio: ratios(&near, total),
            edge_density: edge_pixels as f64 / total,
            diagonal_band_low: band_low / mass,
            diagonal_band_high: band_high / mass,
            regions: measured,
        }
    }
}

pub(crate) fn squared_distance(red: u8, green: u8, blue: u8, colour: [f64; 3]) -> f64 {
    let dr = f64::from(red) - colour[0];
    let dg = f64::from(green) - colour[1];
    let db = f64::from(blue) - colour[2];
    dr * dr + dg * dg + db * db
}

pub(crate) fn palette_table() -> &'static [[f64; 3]; PaletteRole::ALL.len()] {
    static TABLE: OnceLock<[[f64; 3]; PaletteRole::ALL.len()]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [[0.0; 3]; PaletteRole::ALL.len()];
        for (slot, role) in PaletteRole::ALL.into_iter().enumerate() {
            let colour = role.color();
            table[slot] = [
                f64::from(colour.red) * 255.0,
                f64::from(colour.green) * 255.0,
                f64::from(colour.blue) * 255.0,
            ];
        }
        table
    })
}

fn linear_luminance_table() -> &'static [f64; 256] {
    static TABLE: OnceLock<[f64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0.0; 256];
        for (value, slot) in table.iter_mut().enumerate() {
            let channel = value as f64 / 255.0;
            *slot = if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            };
        }
        table
    })
}

/// Loads one PNG frame.
pub fn load_frame(path: &Path) -> Result<RgbImage, String> {
    let decoded = image::open(path)
        .map_err(|error| format!("{} could not be decoded: {error}", path.display()))?;
    Ok(decoded.to_rgb8())
}

/// The approved key art's metrics, measured once and cached for the process.
pub fn reference_metrics() -> &'static FrameMetrics {
    static REFERENCE: OnceLock<FrameMetrics> = OnceLock::new();
    REFERENCE.get_or_init(|| {
        let image = load_frame(Path::new(KEY_ART_REFERENCE_PATH))
            .expect("the approved key art is vendored in this repository");
        FrameMetrics::compute(&image, &BTreeMap::new())
    })
}

// ---------------------------------------------------------------------------
// Mandatory frame contracts
// ---------------------------------------------------------------------------

/// The stable region name of the projected worker crop.
pub const WORKER_REGION: &str = "worker";

// ---------------------------------------------------------------------------
// Row angle and implied elevation
// ---------------------------------------------------------------------------

/// Smallest share of strong-edge mass the two diagonal families must hold
/// together before a measured row angle means anything.
///
/// Without this, a frame with no diagonals at all still produces a peak: the
/// largest bin of an empty histogram is the first one, and the tool would
/// report a confident zero degrees. Refusing to answer is the only honest
/// result for an image that does not contain the thing being measured.
pub const ROW_ANGLE_MIN_MASS: f64 = 0.15;

/// Smallest share of strong-edge mass either diagonal family must hold.
pub const ROW_ANGLE_BAND_MIN_MASS: f64 = 0.04;

/// Largest disagreement between the mirrored diagonal families.
pub const ROW_ANGLE_MAX_SPREAD_DEGREES: f64 = 2.5;

/// Half-width, in one-degree bins, of the window averaged around each peak.
///
/// Argmax alone quantises to whole degrees, which is coarse enough to move the
/// implied elevation by more than a degree. Taking the magnitude-weighted
/// centroid of a small window around the peak recovers the sub-degree
/// precision the projection relation deserves.
const ROW_ANGLE_WINDOW: usize = 3;

/// The two diagonal edge families a diamond ground grid draws on screen.
///
/// ```text
///        low family                high family
///          ~30 deg                   ~150 deg
///             \                         /
///              \                       /
///   -------------\-------------------/-----------  screen
///                 \                 /
///
///   A 45-degree azimuth camera puts both ground axes on screen
///   symmetrically about the vertical, so the two peaks should mirror
///   each other. How far they miss is the measurement's own error bar.
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RowAngle {
    /// Peak of the shallow family, in degrees from the horizontal.
    pub low_degrees: f64,
    /// Peak of the steep family, in degrees.
    pub high_degrees: f64,
    /// Share of all strong-edge mass the two families hold together.
    pub mass: f64,
}

impl RowAngle {
    /// The single row angle the two families agree on.
    ///
    /// The high family is folded onto the low one before averaging, so a
    /// perfectly symmetric pair returns exactly its own angle.
    pub fn row_degrees(&self) -> f64 {
        (self.low_degrees + (180.0 - self.high_degrees)) / 2.0
    }

    /// How far the two families disagree, in degrees.
    ///
    /// This is an error bar, not a defect: a real render never places the two
    /// families perfectly. A large spread means the measurement should not be
    /// trusted to a fraction of a degree.
    pub fn spread_degrees(&self) -> f64 {
        (self.low_degrees - (180.0 - self.high_degrees)).abs()
    }

    /// Elevation implied by the measured angle, when the public fields still
    /// describe a physically derivable row angle.
    ///
    /// [`dominant_row_angle`] enforces that invariant, but callers can also
    /// construct this public type directly.
    pub fn elevation_degrees(&self) -> Option<f64> {
        elevation_from_row_angle(self.row_degrees())
    }
}

/// Elevation implied by a row angle, under orthographic projection at a
/// 45-degree azimuth.
///
/// A ground axis lands on screen at `arctan(sin(elevation))`, so inverting
/// gives `elevation = arcsin(tan(row angle))`. The relation is checkable: the
/// POC's camera basis is 57 degrees and puts its axes at 40.
///
/// Returns nothing at or beyond 45 degrees, where the relation asks for a sine
/// above one. That is a measurement error rather than a very steep camera.
pub fn elevation_from_row_angle(row_angle_degrees: f64) -> Option<f64> {
    if !row_angle_degrees.is_finite() || row_angle_degrees <= 0.0 || row_angle_degrees >= 45.0 {
        return None;
    }
    let sine = row_angle_degrees.to_radians().tan();
    if !(0.0..1.0).contains(&sine) {
        return None;
    }
    Some(sine.asin().to_degrees())
}

/// Measures the dominant diagonal edge families of one frame.
///
/// Valid on reference art. **Not** valid on captured frames: the game renders
/// without multisampling, so aliased near-diagonals produce local gradients
/// biased toward 45 degrees, and the measured angle drifts away from the one
/// the camera basis actually uses. Ask the camera, not the pixels.
pub fn dominant_row_angle(image: &RgbImage) -> Option<RowAngle> {
    let (width, height) = image.dimensions();
    if width < 3 || height < 3 {
        return None;
    }
    let raw = image.as_raw();
    let stride = width as usize * 3;

    let mut histogram = [0.0f64; 180];
    let mut total = 0.0f64;

    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let grey = |dx: i32, dy: i32| -> f64 {
                let sx = (x as i32 + dx) as usize;
                let sy = (y as i32 + dy) as usize;
                let at = sy * stride + sx * 3;
                (0.299 * f64::from(raw[at])
                    + 0.587 * f64::from(raw[at + 1])
                    + 0.114 * f64::from(raw[at + 2]))
                    / 255.0
            };
            let gradient_x = (grey(1, -1) + 2.0 * grey(1, 0) + grey(1, 1))
                - (grey(-1, -1) + 2.0 * grey(-1, 0) + grey(-1, 1));
            let gradient_y = (grey(-1, 1) + 2.0 * grey(0, 1) + grey(1, 1))
                - (grey(-1, -1) + 2.0 * grey(0, -1) + grey(1, -1));
            let magnitude = (gradient_x * gradient_x + gradient_y * gradient_y).sqrt() / 4.0;
            if magnitude < STRONG_EDGE {
                continue;
            }
            let direction = (gradient_x.atan2(-gradient_y).to_degrees() + 180.0).rem_euclid(180.0);
            let bin = (direction as usize).min(179);
            histogram[bin] += magnitude;
            total += magnitude;
        }
    }

    if total <= 0.0 {
        return None;
    }

    let low_mass = histogram[15..75].iter().sum::<f64>() / total;
    let high_mass = histogram[105..165].iter().sum::<f64>() / total;
    if low_mass < ROW_ANGLE_BAND_MIN_MASS || high_mass < ROW_ANGLE_BAND_MIN_MASS {
        return None;
    }

    let low = refine_peak(&histogram, 15, 75)?;
    let high = refine_peak(&histogram, 105, 165)?;
    let mass = low_mass + high_mass;
    if mass < ROW_ANGLE_MIN_MASS {
        return None;
    }

    let angle = RowAngle {
        low_degrees: low,
        high_degrees: high,
        mass,
    };
    if angle.spread_degrees() > ROW_ANGLE_MAX_SPREAD_DEGREES {
        return None;
    }
    elevation_from_row_angle(angle.row_degrees())?;
    Some(angle)
}

/// The magnitude-weighted centre of the tallest peak within one band.
fn refine_peak(histogram: &[f64; 180], from: usize, to: usize) -> Option<f64> {
    let peak = (from..to)
        .max_by(|left, right| histogram[*left].total_cmp(&histogram[*right]))
        .filter(|bin| histogram[*bin] > 0.0)?;
    let first = peak.saturating_sub(ROW_ANGLE_WINDOW);
    let last = (peak + ROW_ANGLE_WINDOW).min(179);

    let mut weight = 0.0;
    let mut moment = 0.0;
    for (bin, mass) in histogram.iter().enumerate().take(last + 1).skip(first) {
        weight += mass;
        moment += mass * (bin as f64 + 0.5);
    }
    (weight > 0.0).then(|| moment / weight)
}

// ---------------------------------------------------------------------------
// Measuring one image
// ---------------------------------------------------------------------------

/// What kind of image is being measured.
///
/// This is an assertion by the operator, not something the tool can detect,
/// and it exists because one measurement is only meaningful for one of them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeasureSource {
    /// Approved concept art. Drawn, not rendered, so its edges are clean and
    /// the row angle recovers the camera it was drawn at.
    Reference,
    /// A frame captured from the running game. Rendered without multisampling,
    /// so aliased near-diagonals bias the measured angle toward 45 degrees.
    ///
    /// The default, deliberately. Mistaking art for a capture loses a number;
    /// mistaking a capture for art publishes a wrong one.
    #[default]
    Capture,
}

/// Why a capture's camera is withheld.
const CAPTURE_NOTE: &str = "row angle withheld: captures render without multisampling, so \
                            aliased diagonals bias the measurement. Derive the camera from \
                            CAMERA_ELEVATION_DEGREES instead.";

/// Everything one image reports.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MeasureReport {
    /// The file measured.
    pub path: String,
    /// Frame width, in pixels.
    pub width: u32,
    /// Frame height, in pixels.
    pub height: u32,
    /// What the operator declared this image to be.
    pub source: MeasureSource,
    /// Share of the frame nearest a rack tone.
    pub rack_mass: f64,
    /// Share of the frame nearest a floor tone.
    pub floor_mass: f64,
    /// Rack mass over floor mass. Absent when no floor mass was measured.
    pub rack_to_floor: Option<f64>,
    /// Mean linear luminance over the whole frame.
    pub mean_linear_luminance: f64,
    /// Fraction of pixels carrying a strong edge.
    pub edge_density: f64,
    /// Fraction of the frame within tolerance of the approved palette.
    pub palette_ratio: f64,
    /// Nearest-role histogram, normalized; sums to one.
    pub nearest_role_ratio: BTreeMap<PaletteRole, f64>,
    /// Fraction of the frame within tolerance of each role.
    pub near_role_ratio: BTreeMap<PaletteRole, f64>,
    /// The measured diagonal families, for reference art only.
    pub row_angle: Option<RowAngle>,
    /// Elevation implied by that angle, for reference art only.
    pub implied_elevation_degrees: Option<f64>,
    /// Why something was withheld, when it was.
    pub note: Option<String>,
}

/// Measures one image.
///
/// The rack-to-floor ratio is the headline: the approved key art holds roughly
/// 1.51 parts rack to one part floor, and a frame that inverts that reads as a
/// floor with some equipment on it however correct its palette.
pub fn measure(path: &Path, source: MeasureSource) -> Result<MeasureReport, String> {
    let image = load_frame(path)?;
    let metrics = FrameMetrics::compute(&image, &BTreeMap::new());

    let rack_mass = metrics.nearest_of(&[PaletteRole::RackWhite, PaletteRole::RackShadow]);
    let floor_mass = metrics.nearest_of(&[PaletteRole::FloorLight, PaletteRole::FloorShadow]);
    let rack_to_floor = if floor_mass > 0.0 {
        Some(rack_mass / floor_mass)
    } else {
        None
    };

    let angle = match source {
        MeasureSource::Reference => dominant_row_angle(&image),
        MeasureSource::Capture => None,
    };

    Ok(MeasureReport {
        path: path.display().to_string(),
        width: metrics.width,
        height: metrics.height,
        source,
        rack_mass,
        floor_mass,
        rack_to_floor,
        mean_linear_luminance: metrics.mean_linear_luminance,
        edge_density: metrics.edge_density,
        palette_ratio: metrics.palette_ratio,
        nearest_role_ratio: metrics.nearest_role_ratio,
        near_role_ratio: metrics.near_role_ratio,
        row_angle: angle,
        implied_elevation_degrees: angle.and_then(|angle| angle.elevation_degrees()),
        note: match source {
            MeasureSource::Capture => Some(CAPTURE_NOTE.to_owned()),
            MeasureSource::Reference if angle.is_none() => {
                Some("row angle withheld: the diagonal edge families were not strong, symmetric, and physically derivable".to_owned())
            }
            MeasureSource::Reference => None,
        },
    })
}
