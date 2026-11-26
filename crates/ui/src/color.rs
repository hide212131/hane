//! Color math shared by the drying ink and the paper texture: sRGB <-> OKLab
//! conversion and lightness-only shifts.

fn srgb_to_linear(channel: u32) -> f64 {
    let value = f64::from(channel & 0xff) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f64) -> u32 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    // Rounded value is within 0..=255.
    (encoded.clamp(0.0, 1.0) * 255.0).round() as u32
}

pub(crate) fn to_oklab(rgb: u32) -> [f64; 3] {
    let r = srgb_to_linear(rgb >> 16);
    let g = srgb_to_linear(rgb >> 8);
    let b = srgb_to_linear(rgb);
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

fn oklab_to_linear(lightness: f64, a: f64, b: f64) -> [f64; 3] {
    let l = (lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m = (lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s = (lightness - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
    [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s,
    ]
}

fn in_gamut(linear: [f64; 3]) -> bool {
    linear.iter().all(|c| (-1e-4..=1.0 + 1e-4).contains(c))
}

/// Encodes an OKLab color. When the requested lightness leaves the sRGB gamut
/// at the original chroma, chroma is reduced just enough to fit; the lightness
/// is kept.
pub(crate) fn from_oklab(lightness: f64, a: f64, b: f64) -> u32 {
    let mut linear = oklab_to_linear(lightness, a, b);
    if !in_gamut(linear) {
        let (mut low, mut high) = (0.0_f64, 1.0_f64);
        for _ in 0..24 {
            let mid = (low + high) / 2.0;
            if in_gamut(oklab_to_linear(lightness, a * mid, b * mid)) {
                low = mid;
            } else {
                high = mid;
            }
        }
        linear = oklab_to_linear(lightness, a * low, b * low);
    }
    (linear_to_srgb(linear[0]) << 16) | (linear_to_srgb(linear[1]) << 8) | linear_to_srgb(linear[2])
}

pub(crate) fn shift_lightness(rgb: u32, delta: f64) -> u32 {
    let [lightness, a, b] = to_oklab(rgb);
    from_oklab((lightness + delta).clamp(0.0, 1.0), a, b)
}
