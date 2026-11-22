//! Color math of HDR streams (`docs/features/hdr.md#color`): transfer functions, primaries
//! conversion and the tonemap to SDR. Values are linear light relative to reference white
//! unless noted.

use crate::Primaries;

/// SMPTE ST 2084 (PQ) constants.
const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;

/// ITU-R BT.2100 HLG constants.
const HLG_A: f32 = 0.178_832_77;
const HLG_B: f32 = 0.284_668_92;
const HLG_C: f32 = 0.559_910_7;
/// Peak luminance of the display HLG is rendered for, in nits, and its system gamma.
pub const HLG_DISPLAY_PEAK: f32 = 1000.0;
const HLG_GAMMA: f32 = 1.2;

/// Largest value a half float holds; HDR values are clamped to `0..=MAX_HDR`.
pub const MAX_HDR: f32 = 65504.0;

/// Default reference white in nits (ITU-R BT.2408).
pub const DEFAULT_REFERENCE_WHITE: u16 = 203;
/// Default tonemap white: the linear value that the SDR tonemap maps to 1.0.
pub const DEFAULT_TONEMAP_WHITE: f32 = 4.0;

/// BT.2020 to BT.709, linear values (`docs/spec/PRAXI.md#hdr-color`).
pub const BT2020_TO_BT709: [[f32; 3]; 3] =
    [[1.6605, -0.5876, -0.0728], [-0.1246, 1.1329, -0.0083], [-0.0182, -0.1006, 1.1187]];
/// BT.709 to BT.2020, linear values.
pub const BT709_TO_BT2020: [[f32; 3]; 3] = [[0.6274, 0.3293, 0.0433], [0.0691, 0.9195, 0.0114], [0.0164, 0.0880, 0.8956]];

/// PQ code value (`0..=1`) to nits.
pub fn pq_to_nits(code: f32) -> f32 {
    let p = code.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    ((p - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * p)).powf(1.0 / PQ_M1) * 10000.0
}

/// Nits to PQ code value (`0..=1`).
pub fn nits_to_pq(nits: f32) -> f32 {
    let y = (nits / 10000.0).clamp(0.0, 1.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)).powf(PQ_M2)
}

/// 10-bit PQ code of a linear value at `reference_white` nits, for temporal skip
/// (`docs/features/hdr.md#temporal-skip`).
pub fn pq_code10(value: f32, reference_white: f32) -> u16 {
    (nits_to_pq(value * reference_white) * 1023.0).round() as u16
}

/// HLG signal (BT.2020 RGB, `0..=1`) to display light in nits on a
/// [`HLG_DISPLAY_PEAK`] display: inverse OETF, then the OOTF on luminance.
pub fn hlg_to_nits(signal: [f32; 3]) -> [f32; 3] {
    let e = signal.map(|v| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.5 { v * v / 3.0 } else { (((v - HLG_C) / HLG_A).exp() + HLG_B) / 12.0 }
    });
    let ys = 0.2627 * e[0] + 0.6780 * e[1] + 0.0593 * e[2];
    let gain = if ys > 0.0 { HLG_DISPLAY_PEAK * ys.powf(HLG_GAMMA - 1.0) } else { 0.0 };
    e.map(|v| v * gain)
}

/// sRGB-encoded value (`0..=1`) to linear.
pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Linear value (`0..=1`) to an 8-bit sRGB code.
pub fn linear_to_srgb8(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0).round() as u8
}

/// `m * rgb`.
pub fn mul(m: &[[f32; 3]; 3], rgb: [f32; 3]) -> [f32; 3] {
    m.map(|row| row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2])
}

/// Converts linear `rgb` between primaries.
pub fn convert_primaries(rgb: [f32; 3], from: Primaries, to: Primaries) -> [f32; 3] {
    match (from, to) {
        (Primaries::Bt2020, Primaries::Bt709) => mul(&BT2020_TO_BT709, rgb),
        (Primaries::Bt709, Primaries::Bt2020) => mul(&BT709_TO_BT2020, rgb),
        _ => rgb,
    }
}

/// Clamps a linear HDR value to `0..=MAX_HDR`; NaN becomes 0.
pub fn clamp_hdr(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, MAX_HDR) }
}

/// The SDR tonemap (`docs/features/hdr.md#tonemap-to-sdr`): linear `rgb` in `primaries` to
/// linear BT.709 in `0..=1`. `white` is the value that maps to 1.0.
pub fn tonemap(rgb: [f32; 3], primaries: Primaries, white: f32) -> [f32; 3] {
    let rgb = convert_primaries(rgb, primaries, Primaries::Bt709).map(clamp_hdr);
    let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    let scale = if y > 0.0 { (1.0 + y / (white * white)) / (1.0 + y) } else { 1.0 };
    rgb.map(|v| (v * scale).clamp(0.0, 1.0))
}

/// [`tonemap`], then sRGB encoded to 8 bits.
pub fn tonemap_srgb8(rgb: [f32; 3], primaries: Primaries, white: f32) -> [u8; 3] {
    tonemap(rgb, primaries, white).map(linear_to_srgb8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, tolerance: f32) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn pq_reference_points() {
        // ITU-R BT.2408: reference white, 203 nits, is PQ 58%.
        assert!(close(nits_to_pq(203.0), 0.5807, 0.0005), "{}", nits_to_pq(203.0));
        assert!(close(pq_to_nits(0.5807), 203.0, 0.3), "{}", pq_to_nits(0.5807));
        assert!(close(pq_to_nits(1.0), 10000.0, 1.0));
        assert_eq!(pq_to_nits(0.0), 0.0);
        for nits in [0.1f32, 1.0, 100.0, 1000.0, 4000.0] {
            assert!(close(pq_to_nits(nits_to_pq(nits)), nits, nits * 1e-3), "{nits}");
        }
        assert_eq!(pq_code10(1.0, 203.0), 594);
    }

    #[test]
    fn hlg_reference_points() {
        // ITU-R BT.2408: HLG 75% is reference white, 203 nits on a 1000-nit display.
        let w = hlg_to_nits([0.75; 3]);
        assert!(w.iter().all(|&v| close(v, 203.0, 1.0)), "{w:?}");
        assert!(hlg_to_nits([1.0; 3]).iter().all(|&v| close(v, 1000.0, 1.0)));
        assert_eq!(hlg_to_nits([0.0; 3]), [0.0; 3]);
    }

    #[test]
    fn primaries_round_trip() {
        let rgb = [0.2, 0.5, 0.9];
        let back = convert_primaries(convert_primaries(rgb, Primaries::Bt709, Primaries::Bt2020), Primaries::Bt2020, Primaries::Bt709);
        assert!(rgb.iter().zip(back).all(|(a, b)| close(*a, b, 1e-3)), "{back:?}");
        // White stays white.
        let w = convert_primaries([1.0; 3], Primaries::Bt2020, Primaries::Bt709);
        assert!(w.iter().all(|&v| close(v, 1.0, 1e-3)), "{w:?}");
    }

    #[test]
    fn tonemap_fixed_points() {
        let t = |v: f32| tonemap([v; 3], Primaries::Bt709, 4.0)[0];
        assert_eq!(t(0.0), 0.0);
        // Extended Reinhard: white maps to 1, 1.0 to (1 + 1/16) / 2.
        assert!(close(t(4.0), 1.0, 1e-6));
        assert!(close(t(1.0), 17.0 / 32.0, 1e-6));
        assert_eq!(t(100.0), 1.0);
        // Hue is kept below white: the ratio of channels does not change.
        let c = tonemap([0.5, 0.25, 0.0], Primaries::Bt709, 4.0);
        assert!(close(c[1] / c[0], 0.5, 1e-6));
        assert_eq!(tonemap_srgb8([1.0; 3], Primaries::Bt709, 1.0), [255; 3]);
        assert_eq!(linear_to_srgb8(srgb_to_linear(0.5)), 128);
    }
}
