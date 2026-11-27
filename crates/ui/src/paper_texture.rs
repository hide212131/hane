//! Paper / wall texture of the editor body (Issue #788, phase P1).
//!
//! A procedural, seamlessly tiling lightness field is generated once per
//! (theme, kind, strength, scale factor) and drawn behind the text as a grid of
//! one cached image. Nothing here touches the document, the layout or the
//! caret, and nothing asks for animation frames: scrolling only moves where the
//! tiles are placed.
//!
//! The field is a change of OKLab lightness applied to the editor background,
//! quantized to 1/1024 so that a few dozen colors cover the whole tile. Its
//! amplitude stays inside a fixed budget (`LIGHT_BUDGET` / `DARK_BUDGET`) so
//! that every foreground token keeps its WCAG contrast.

use crate::color::shift_lightness;
use gpui::{
    App, Bounds, Corners, Element, ElementId, GlobalElementId, IntoElement, LayoutId, Pixels,
    Position, RenderImage, Style, Window, point, px, relative,
};
use hane_session::PaperTextureStrength;
use std::sync::Arc;

/// Logical edge length of one tile.
pub(crate) const TILE_LOGICAL_PX: u32 = 384;
/// The lightness field is stored in steps of 1/1024.
const QUANT_STEPS: f32 = 1024.0;
/// Cycles of the two low-frequency layers per tile (feature size ~128 / 64 px).
const LOW_CYCLES: [u32; 2] = [3, 6];
const SEED_LIGHT: u64 = 0x4a1d_77c3_9be1_02f5;
const SEED_DARK: u64 = 0x91c4_2e68_d0a7_5b13;
/// Largest push an element may need beyond the exact device-pixel position.
const EDGE_EPSILON: f32 = 0.002;

/// Allowed change of lightness against the plain background (3.1.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Budget {
    pub(crate) min: f32,
    pub(crate) max: f32,
    pub(crate) rms: f32,
    /// RMS of the part with a spatial period of 2px or less.
    pub(crate) hf: f32,
}

pub(crate) const LIGHT_BUDGET: Budget = Budget {
    min: -0.020,
    max: 0.006,
    rms: 0.006,
    hf: 0.004,
};

pub(crate) const DARK_BUDGET: Budget = Budget {
    min: -0.018,
    max: 0.008,
    rms: 0.006,
    hf: 0.003,
};

/// Target amplitude of each layer at the standard strength.
struct Recipe {
    /// RMS of the combined low-frequency mottling.
    mottling_rms: f32,
    /// High-frequency RMS of the fibers; zero means no fibers.
    fiber_hf: f32,
    /// High-frequency RMS of the fine grain.
    grain_hf: f32,
}

const LIGHT_RECIPE: Recipe = Recipe {
    mottling_rms: 0.0014,
    fiber_hf: 0.0013,
    grain_hf: 0.0010,
};

const DARK_RECIPE: Recipe = Recipe {
    mottling_rms: 0.0025,
    fiber_hf: 0.0,
    grain_hf: 0.0012,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextureKind {
    /// Fine paper in light themes, plaster wall in dark themes.
    Paper,
}

/// What one generated image depends on. The image is regenerated only when the
/// key changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TextureKey {
    pub(crate) dark: bool,
    pub(crate) kind: TextureKind,
    pub(crate) strength: PaperTextureStrength,
    /// The scale factor in hundredths.
    pub(crate) scale_100: u32,
}

impl TextureKey {
    pub(crate) fn new(dark: bool, strength: PaperTextureStrength, scale_factor: f32) -> Self {
        let scale_100 = (scale_factor * 100.0).round().clamp(50.0, 800.0) as u32;
        Self {
            dark,
            kind: TextureKind::Paper,
            strength,
            scale_100,
        }
    }

    /// Edge length of the tile in device pixels.
    pub(crate) fn tile_px(&self) -> usize {
        (f64::from(TILE_LOGICAL_PX) * f64::from(self.scale_100) / 100.0).round() as usize
    }

    fn scale(&self) -> f32 {
        self.scale_100 as f32 / 100.0
    }

    pub(crate) fn budget(&self) -> Budget {
        if self.dark { DARK_BUDGET } else { LIGHT_BUDGET }
    }

    fn seed(&self) -> u64 {
        match self.kind {
            TextureKind::Paper if self.dark => SEED_DARK,
            TextureKind::Paper => SEED_LIGHT,
        }
    }

    fn recipe(&self) -> &'static Recipe {
        if self.dark { &DARK_RECIPE } else { &LIGHT_RECIPE }
    }
}

// --- Deterministic noise -------------------------------------------------

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// `[0, 1)` from the top 24 bits.
fn unit(bits: u64) -> f32 {
    (bits >> 40) as f32 / 16_777_216.0
}

/// `[-1, 1)`.
fn signed_unit(bits: u64) -> f32 {
    unit(bits) * 2.0 - 1.0
}

struct Rng(u64);

impl Rng {
    fn next_bits(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(GOLDEN);
        mix(self.0)
    }

    fn unit(&mut self) -> f32 {
        unit(self.next_bits())
    }
}

/// Value noise whose lattice wraps after `cycles` cells, so the field repeats
/// exactly every `n` pixels in both directions.
fn periodic_value_noise(n: usize, cycles: u32, seed: u64) -> Vec<f32> {
    let c = cycles as usize;
    let lattice: Vec<f32> = (0..c * c)
        .map(|i| signed_unit(mix(seed.wrapping_add((i as u64 + 1).wrapping_mul(GOLDEN)))))
        .collect();
    let cell = cycles as f32 / n as f32;
    let axis: Vec<(usize, usize, f32)> = (0..n)
        .map(|p| {
            let u = p as f32 * cell;
            let base = u.floor();
            let t = u - base;
            let t = t * t * (3.0 - 2.0 * t);
            let i0 = base as usize % c;
            (i0, (i0 + 1) % c, t)
        })
        .collect();
    let mut out = Vec::with_capacity(n * n);
    for &(y0, y1, ty) in &axis {
        for &(x0, x1, tx) in &axis {
            let a = lattice[y0 * c + x0];
            let b = lattice[y0 * c + x1];
            let d = lattice[y1 * c + x0];
            let e = lattice[y1 * c + x1];
            let top = a + (b - a) * tx;
            let bottom = d + (e - d) * tx;
            out.push(top + (bottom - top) * ty);
        }
    }
    out
}

/// Fine grain with a period of one to two pixels. Dark themes keep the bright
/// side small: a bright dot on a dark wall reads as dust.
fn grain(n: usize, seed: u64, dark: bool) -> Vec<f32> {
    let white: Vec<f32> = (0..n * n)
        .map(|i| signed_unit(mix(seed.wrapping_add((i as u64 + 1).wrapping_mul(GOLDEN)))))
        .collect();
    let mut out = Vec::with_capacity(n * n);
    for y in 0..n {
        let below = (y + 1) % n;
        for x in 0..n {
            let right = (x + 1) % n;
            let v = (2.0 * white[y * n + x] + white[y * n + right] + white[below * n + x]) / 4.0;
            out.push(if dark && v > 0.0 { v * 0.5 } else { v });
        }
    }
    out
}

/// Short, slightly bent dark fibers (150-200 per tile). They wrap around the
/// tile edges, so the tile still repeats seamlessly. Values are `<= 0`; a
/// fiber of depth 1 reaches `-1`.
fn fibers(n: usize, scale: f32, seed: u64) -> Vec<f32> {
    const STEP: f32 = 0.5;
    let mut rng = Rng(seed);
    let mut field = vec![0.0_f32; n * n];
    let count = 150 + (rng.next_bits() % 51) as usize;
    let extent = n as f32;
    let wrap = n as i64;
    for _ in 0..count {
        let mut x = rng.unit() * extent;
        let mut y = rng.unit() * extent;
        let mut angle = rng.unit() * std::f32::consts::TAU;
        let length = (20.0 + rng.unit() * 60.0) * scale;
        let width = 1.0 + rng.unit() * 0.5;
        let depth = 0.5 + rng.unit() * 0.5;
        let bend = (rng.unit() - 0.5) * 1.2;
        let steps = (length / STEP).ceil().max(1.0) as usize;
        let turn = bend / steps as f32;
        let reach = width / 2.0 + 0.5;
        for _ in 0..steps {
            let (base_x, base_y) = (x.floor() as i64, y.floor() as i64);
            for dy in -2..=2_i64 {
                for dx in -2..=2_i64 {
                    let (ix, iy) = (base_x + dx, base_y + dy);
                    let off_x = ix as f32 + 0.5 - x;
                    let off_y = iy as f32 + 0.5 - y;
                    let distance = (off_x * off_x + off_y * off_y).sqrt();
                    let coverage = (reach - distance).clamp(0.0, 1.0);
                    if coverage > 0.0 {
                        let index = iy.rem_euclid(wrap) as usize * n + ix.rem_euclid(wrap) as usize;
                        let value = -depth * coverage;
                        if value < field[index] {
                            field[index] = value;
                        }
                    }
                }
            }
            x += STEP * angle.cos();
            y += STEP * angle.sin();
            angle += turn;
        }
    }
    field
}

// --- Measurement ---------------------------------------------------------

/// Root-mean-square measures of a field on an `n` x `n` torus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FieldStats {
    /// RMS of the values.
    pub(crate) rms: f32,
    /// RMS of each value minus the mean of its 3x3 neighborhood: the part with
    /// a spatial period of about 2px or less.
    pub(crate) hf: f32,
}

pub(crate) fn field_stats(values: &[f32], n: usize) -> FieldStats {
    let mut square_sum = 0.0_f64;
    let mut hf_square_sum = 0.0_f64;
    for y in 0..n {
        let rows = [(y + n - 1) % n, y, (y + 1) % n];
        for x in 0..n {
            let cols = [(x + n - 1) % n, x, (x + 1) % n];
            let mut around = 0.0_f32;
            for row in rows {
                for col in cols {
                    around += values[row * n + col];
                }
            }
            let value = values[y * n + x];
            let high = value - around / 9.0;
            square_sum += f64::from(value) * f64::from(value);
            hf_square_sum += f64::from(high) * f64::from(high);
        }
    }
    let count = (n * n) as f64;
    FieldStats {
        rms: (square_sum / count).sqrt() as f32,
        hf: (hf_square_sum / count).sqrt() as f32,
    }
}

pub(crate) fn dequantize(delta: &[i16]) -> Vec<f32> {
    delta
        .iter()
        .map(|&step| f32::from(step) / QUANT_STEPS)
        .collect()
}

fn quantize(values: &[f32], gain: f32, budget: &Budget) -> Vec<i16> {
    let low = (budget.min * QUANT_STEPS).ceil();
    let high = (budget.max * QUANT_STEPS).floor();
    values
        .iter()
        .map(|&value| (value * gain * QUANT_STEPS).round().clamp(low, high) as i16)
        .collect()
}

/// The unit gain that brings a layer to `target` of `measure`, or zero when
/// the layer is flat.
fn gain_for(target: f32, measured: f32) -> f32 {
    if measured > 1e-9 { target / measured } else { 0.0 }
}

// --- Generation ----------------------------------------------------------

/// The quantized lightness field (in 1/1024 steps) for `key`. Pure and
/// deterministic: the same key always gives the same field.
pub(crate) fn generate_delta(key: &TextureKey) -> Vec<i16> {
    let n = key.tile_px();
    let seed = key.seed();
    let recipe = key.recipe();
    let budget = key.budget();

    let low_a = periodic_value_noise(n, LOW_CYCLES[0], seed ^ 0x1111);
    let low_b = periodic_value_noise(n, LOW_CYCLES[1], seed ^ 0x2222);
    let mottling: Vec<f32> = low_a
        .iter()
        .zip(&low_b)
        .map(|(a, b)| 0.6 * a + 0.4 * b)
        .collect();
    let mottling_gain = gain_for(recipe.mottling_rms, field_stats(&mottling, n).rms);

    let fine = grain(n, seed ^ 0x3333, key.dark);
    let grain_gain = gain_for(recipe.grain_hf, field_stats(&fine, n).hf);

    let (fiber_layer, fiber_gain) = if recipe.fiber_hf > 0.0 {
        let layer = fibers(n, key.scale(), seed ^ 0x4444);
        let gain = gain_for(recipe.fiber_hf, field_stats(&layer, n).hf);
        (layer, gain)
    } else {
        (Vec::new(), 0.0)
    };

    let combined: Vec<f32> = (0..n * n)
        .map(|i| {
            let fiber = fiber_layer.get(i).copied().unwrap_or(0.0);
            mottling_gain * mottling[i] + fiber_gain * fiber + grain_gain * fine[i]
        })
        .collect();

    // The strength scales every layer. A strength that would leave the budget
    // is capped by shrinking the whole field until it fits.
    let mut gain = key.strength.amplitude_factor();
    let mut delta = quantize(&combined, gain, &budget);
    for _ in 0..16 {
        let stats = field_stats(&dequantize(&delta), n);
        let ratio = (budget.rms / stats.rms.max(1e-9)).min(budget.hf / stats.hf.max(1e-9));
        if ratio >= 1.0 {
            break;
        }
        gain *= ratio * 0.97;
        delta = quantize(&combined, gain, &budget);
    }
    delta
}

/// An opaque square tile in BGRA byte order, as the renderer's atlas expects.
pub(crate) struct TileData {
    pub(crate) size: usize,
    pub(crate) bgra: Vec<u8>,
}

/// Generates the tile for `key` on top of `background` (`0xRRGGBB`).
pub(crate) fn generate_tile(key: &TextureKey, background: u32) -> TileData {
    let budget = key.budget();
    let delta = generate_delta(key);
    let lowest = (budget.min * QUANT_STEPS).ceil() as i16;
    let highest = (budget.max * QUANT_STEPS).floor() as i16;
    // Only a few dozen distinct lightness steps exist, so each color is
    // converted once.
    let table: Vec<[u8; 4]> = (lowest..=highest)
        .map(|step| {
            let rgb = shift_lightness(background, f64::from(step) / f64::from(QUANT_STEPS));
            [
                (rgb & 0xff) as u8,
                ((rgb >> 8) & 0xff) as u8,
                ((rgb >> 16) & 0xff) as u8,
                0xff,
            ]
        })
        .collect();
    let mut bgra = Vec::with_capacity(delta.len() * 4);
    for step in delta {
        bgra.extend_from_slice(&table[(step - lowest) as usize]);
    }
    TileData {
        size: key.tile_px(),
        bgra,
    }
}

impl TileData {
    pub(crate) fn into_render_image(self) -> Arc<RenderImage> {
        let side = self.size as u32;
        let buffer = image::RgbaImage::from_raw(side, side, self.bgra)
            .expect("a square tile has exactly four bytes per pixel");
        Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
    }
}

// --- Placement -----------------------------------------------------------

/// Device-pixel origins of the tiles that cover a `width` x `height` area at
/// (`left`, `top`). Rows are anchored to the document: the grid is shifted by
/// `scroll` (device pixels) modulo the tile size, so the paper moves with the
/// text. Columns are anchored to the left edge.
pub(crate) fn tile_origins(
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    tile: i32,
    scroll: i32,
) -> Vec<(i32, i32)> {
    if tile <= 0 || width <= 0 || height <= 0 {
        return Vec::new();
    }
    let mut origins = Vec::new();
    let mut y = top - scroll.rem_euclid(tile);
    while y < top + height {
        let mut x = left;
        while x < left + width {
            origins.push((x, y));
            x += tile;
        }
        y += tile;
    }
    origins
}

/// The texture layer: absolute, covering its parent, drawn as the parent's
/// first child so everything else paints over it.
pub(crate) struct PaperTexture {
    image: Arc<RenderImage>,
    scroll_y: f32,
}

impl PaperTexture {
    pub(crate) fn new(image: Arc<RenderImage>, scroll_y: f32) -> Self {
        Self { image, scroll_y }
    }
}

impl IntoElement for PaperTexture {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PaperTexture {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style {
            position: Position::Absolute,
            ..Style::default()
        };
        style.inset.top = px(0.0).into();
        style.inset.left = px(0.0).into();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) {
        let scale = window.scale_factor();
        let tile = self.image.size(0).width.0;
        let to_device = |value: Pixels| (f32::from(value) * scale).round() as i32;
        let origins = tile_origins(
            to_device(bounds.origin.x),
            to_device(bounds.origin.y),
            to_device(bounds.size.width),
            to_device(bounds.size.height),
            tile,
            (self.scroll_y * scale).round() as i32,
        );
        // The renderer floors an origin and ceils a size in device pixels, so
        // each tile is nudged to land exactly on its texel grid.
        let edge = px((tile as f32 - EDGE_EPSILON) / scale);
        for (x, y) in origins {
            let origin = point(
                px((x as f32 + EDGE_EPSILON) / scale),
                px((y as f32 + EDGE_EPSILON) / scale),
            );
            let tile_bounds = Bounds::new(origin, gpui::size(edge, edge));
            let _ = window.paint_image(
                tile_bounds,
                tile_bounds,
                Corners::default(),
                self.image.clone(),
                0,
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::syntax_tone_color;
    use crate::theme::{DARK_THEME, DEFAULT_THEME, Theme};
    use hane_presentation::SyntaxTone;

    const STRENGTHS: [PaperTextureStrength; 3] = [
        PaperTextureStrength::Weak,
        PaperTextureStrength::Standard,
        PaperTextureStrength::Strong,
    ];

    fn themes() -> [(bool, Theme); 2] {
        [(false, DEFAULT_THEME), (true, DARK_THEME)]
    }

    fn key(dark: bool, strength: PaperTextureStrength) -> TextureKey {
        TextureKey::new(dark, strength, 1.0)
    }

    fn luminance(rgb: u32) -> f64 {
        let channel = |shift: u32| {
            let value = f64::from((rgb >> shift) & 0xff) / 255.0;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    #[test]
    fn tile_edges_are_continuous_across_the_seam() {
        for (dark, _) in themes() {
            let key = key(dark, PaperTextureStrength::Standard);
            let n = key.tile_px();
            let delta = generate_delta(&key);
            let at = |x: usize, y: usize| i32::from(delta[y * n + x]);
            let mut interior = Vec::with_capacity(2 * n * n);
            for y in 0..n - 1 {
                for x in 0..n - 1 {
                    interior.push((at(x + 1, y) - at(x, y)).unsigned_abs());
                    interior.push((at(x, y + 1) - at(x, y)).unsigned_abs());
                }
            }
            interior.sort_unstable();
            let p99 = interior[interior.len() * 99 / 100] as f32;
            let seam_across_x = (0..n)
                .map(|y| (at(0, y) - at(n - 1, y)).unsigned_abs() as f32)
                .sum::<f32>()
                / n as f32;
            let seam_across_y = (0..n)
                .map(|x| (at(x, 0) - at(x, n - 1)).unsigned_abs() as f32)
                .sum::<f32>()
                / n as f32;
            assert!(seam_across_x <= p99, "dark={dark}: {seam_across_x} > {p99}");
            assert!(seam_across_y <= p99, "dark={dark}: {seam_across_y} > {p99}");
        }
    }

    #[test]
    fn low_frequency_layers_wrap_exactly() {
        for cycles in LOW_CYCLES {
            let n = 96;
            let field = periodic_value_noise(n, cycles, 7);
            let step = |a: f32, b: f32| (a - b).abs();
            let mut interior_max = 0.0_f32;
            for y in 0..n {
                for x in 0..n - 1 {
                    interior_max = interior_max.max(step(field[y * n + x], field[y * n + x + 1]));
                }
            }
            for y in 0..n {
                assert!(step(field[y * n + n - 1], field[y * n]) <= interior_max);
            }
            for x in 0..n {
                assert!(step(field[(n - 1) * n + x], field[x]) <= interior_max);
            }
        }
    }

    #[test]
    fn every_foreground_keeps_contrast_on_the_worst_pixel() {
        for (dark, theme) in themes() {
            let mut tokens = vec![theme.foreground];
            for tone in [
                SyntaxTone::Quote,
                SyntaxTone::Heading,
                SyntaxTone::HeadingMinor,
                SyntaxTone::Link,
                SyntaxTone::Code,
                SyntaxTone::Punctuation,
                SyntaxTone::Structure,
            ] {
                tokens.push(syntax_tone_color(&theme, tone));
            }
            for strength in STRENGTHS {
                let tile = generate_tile(&key(dark, strength), theme.editor_background);
                // The worst background is the darkest pixel in a light theme
                // and the brightest in a dark one.
                let mut worst = if dark { f64::MIN } else { f64::MAX };
                let mut worst_rgb = theme.editor_background;
                for pixel in tile.bgra.as_chunks::<4>().0 {
                    let rgb = (u32::from(pixel[2]) << 16)
                        | (u32::from(pixel[1]) << 8)
                        | u32::from(pixel[0]);
                    let y = luminance(rgb);
                    if (dark && y > worst) || (!dark && y < worst) {
                        worst = y;
                        worst_rgb = rgb;
                    }
                }
                for token in &tokens {
                    let ratio = contrast(*token, worst_rgb);
                    assert!(
                        ratio >= 4.5,
                        "dark={dark} {strength:?} token {token:06x} on {worst_rgb:06x}: {ratio:.2}"
                    );
                }
            }
        }
    }

    #[test]
    fn amplitude_stays_inside_the_budget() {
        for (dark, _) in themes() {
            let mut rms_by_strength = Vec::new();
            for strength in STRENGTHS {
                let key = key(dark, strength);
                let budget = key.budget();
                let n = key.tile_px();
                let field = dequantize(&generate_delta(&key));
                let low = field.iter().copied().fold(f32::MAX, f32::min);
                let high = field.iter().copied().fold(f32::MIN, f32::max);
                let stats = field_stats(&field, n);
                let label = format!("dark={dark} {strength:?}");
                assert!(low >= budget.min, "{label}: min {low}");
                assert!(high <= budget.max, "{label}: max {high}");
                assert!(stats.rms <= budget.rms, "{label}: rms {}", stats.rms);
                assert!(stats.hf <= budget.hf, "{label}: hf {}", stats.hf);
                assert!(stats.rms > 0.0004, "{label}: the texture vanished");
                rms_by_strength.push(stats.rms);
            }
            assert!(
                rms_by_strength.windows(2).all(|pair| pair[0] < pair[1]),
                "dark={dark}: strength does not order the amplitude: {rms_by_strength:?}"
            );
        }
    }

    #[test]
    fn budget_values_match_the_specification() {
        assert_eq!(LIGHT_BUDGET.min, -0.020);
        assert_eq!(LIGHT_BUDGET.max, 0.006);
        assert_eq!(LIGHT_BUDGET.rms, 0.006);
        assert_eq!(LIGHT_BUDGET.hf, 0.004);
        assert_eq!(DARK_BUDGET.min, -0.018);
        assert_eq!(DARK_BUDGET.max, 0.008);
        assert_eq!(DARK_BUDGET.rms, 0.006);
        assert_eq!(DARK_BUDGET.hf, 0.003);
    }

    #[test]
    fn the_same_key_generates_the_same_image() {
        for (dark, theme) in themes() {
            let key = key(dark, PaperTextureStrength::Standard);
            let first = generate_tile(&key, theme.editor_background);
            let second = generate_tile(&key, theme.editor_background);
            assert_eq!(first.size, key.tile_px());
            assert_eq!(first.bgra.len(), first.size * first.size * 4);
            assert_eq!(fnv1a(&first.bgra), fnv1a(&second.bgra));
            assert!(first.bgra.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0xff));
        }
        let standard = PaperTextureStrength::Standard;
        let light_bg = DEFAULT_THEME.editor_background;
        let light = generate_tile(&key(false, standard), light_bg);
        let dark = generate_tile(&key(true, standard), DARK_THEME.editor_background);
        let strong = generate_tile(&key(false, PaperTextureStrength::Strong), light_bg);
        assert_ne!(fnv1a(&light.bgra), fnv1a(&dark.bgra));
        assert_ne!(fnv1a(&light.bgra), fnv1a(&strong.bgra));
    }

    #[test]
    fn the_scale_factor_is_rounded_to_hundredths_for_the_key() {
        let standard = PaperTextureStrength::Standard;
        assert_eq!(
            TextureKey::new(false, standard, 1.999),
            TextureKey::new(false, standard, 2.0)
        );
        assert_ne!(
            TextureKey::new(false, standard, 1.5),
            TextureKey::new(false, standard, 2.0)
        );
        assert_ne!(
            TextureKey::new(false, standard, 2.0),
            TextureKey::new(true, standard, 2.0)
        );
        assert_eq!(TextureKey::new(false, standard, 1.0).tile_px(), 384);
        assert_eq!(TextureKey::new(false, standard, 1.5).tile_px(), 576);
        assert_eq!(TextureKey::new(false, standard, 2.0).tile_px(), 768);
    }

    #[test]
    fn tiles_cover_the_area_and_follow_the_scroll() {
        let covers = |origins: &[(i32, i32)], left, top, width, height, tile| {
            (top..top + height).step_by(7).all(|y| {
                (left..left + width).step_by(7).all(|x| {
                    origins
                        .iter()
                        .any(|&(ox, oy)| x >= ox && x < ox + tile && y >= oy && y < oy + tile)
                })
            })
        };
        for scroll in [0, 1, 383, 384, 385, 1000, -50] {
            let origins = tile_origins(10, 20, 1000, 700, 384, scroll);
            assert!(covers(&origins, 10, 20, 1000, 700, 384), "scroll {scroll}");
            // The first row starts at or above the top, at most one tile away.
            let first_row = origins.iter().map(|&(_, y)| y).min().unwrap();
            assert!(first_row <= 20 && first_row > 20 - 384, "scroll {scroll}");
            // Rows stay on the document's grid.
            assert!(
                origins
                    .iter()
                    .all(|&(_, y)| (y - 20 + scroll).rem_euclid(384) == 0),
                "scroll {scroll}"
            );
        }
        assert!(tile_origins(0, 0, 0, 100, 384, 0).is_empty());
        assert!(tile_origins(0, 0, 100, 100, 0, 0).is_empty());
    }
}
