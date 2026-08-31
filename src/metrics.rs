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
//!                             │
//!                             ▼
//!                    the gate constants below
//! ```
//!
//! The gate constants come in two kinds and the distinction matters. Some are
//! measured from the approved reference art; some are engineering contracts
//! chosen for this hall. Mixing them is how a window ends up tuned to whatever
//! the engine already produced, which is a gate that can only ever agree with
//! itself.

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
                if (30.0..50.0).contains(&direction) {
                    band_low += magnitude;
                } else if (130.0..150.0).contains(&direction) {
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

/// Largest share of magenta sentinel a frame may hold.
///
/// The sentinel is the clear colour, so any of it on screen means the camera's
/// ground quadrilateral left the 72 m rendered apron. This is the
/// rendered-coverage gate.
pub const SENTINEL_MAX: f64 = 0.001;

/// Absolute mean linear luminance window.
pub const LUMINANCE_RANGE: (f64, f64) = (0.48, 0.88);

/// How far a frame's mean linear luminance may sit from the approved key art.
pub const LUMINANCE_REFERENCE_TOLERANCE: f64 = 0.18;

/// Smallest share of pixels within [`PALETTE_TOLERANCE`] of the typed palette.
pub const PALETTE_MIN: f64 = 0.60;

/// Smallest share of floor tones.
pub const FLOOR_MIN: f64 = 0.20;

/// Smallest share of rack base and rack shadow tones.
pub const RACK_MIN: f64 = 0.06;

/// Smallest share of signature yellow.
pub const YELLOW_MIN: f64 = 0.005;

/// Allowed share of ink and hose charcoal.
pub const INK_RANGE: (f64, f64) = (0.03, 0.35);

/// Smallest share of strong-edge mass each diagonal band must hold at a
/// settled heading.
pub const DIAGONAL_BAND_MIN: f64 = 0.08;

/// Largest nearest-palette histogram L1 distance from the key art.
pub const HISTOGRAM_MAX: f64 = 0.90;

/// Allowed edge density, as a multiple of the key art's edge density.
pub const EDGE_DENSITY_RANGE: (f64, f64) = (0.35, 2.5);

/// Smallest share of the projected worker crop each worker identity colour
/// must cover.
pub const WORKER_ROLE_MIN: f64 = 0.002;

/// Smallest share of a drawn badge rectangle its own colour must cover.
pub const BADGE_ROLE_MIN: f64 = 0.10;

/// Smallest share of the queue panel each live state colour must cover.
pub const HUD_STATE_MIN: f64 = 0.002;

/// Allowed difference between two worker crops playing different clips.
pub const CLIP_DIFFERENCE_RANGE: (f64, f64) = (0.02, 0.60);

/// Largest share of a frame outside the worker crop that may change between
/// two captures taken from the same position.
pub const OUTSIDE_CROP_MAX: f64 = 0.01;

/// The stable region name of the projected worker crop.
pub const WORKER_REGION: &str = "worker";
