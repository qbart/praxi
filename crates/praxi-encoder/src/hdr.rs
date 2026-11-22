//! HDR frames (`docs/features/hdr.md#encoder`): the color configuration, conversions
//! between SDR and HDR frames, temporal-skip keys and light levels.

use praxi_format::Primaries;
use praxi_format::color::{
    DEFAULT_REFERENCE_WHITE, DEFAULT_TONEMAP_WHITE, clamp_hdr, convert_primaries, pq_code10, srgb_to_linear,
    tonemap_srgb8,
};

/// Color of the pushed frames and of HDR variants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorConfig {
    /// Whether HDR variants may be added (`ColorConfig::hdr`).
    pub hdr: bool,
    /// Primaries of `push_frame_hdr` frames and of the HDR variants.
    pub primaries: Primaries,
    /// Nits of the value 1.0.
    pub reference_white: u16,
    /// The linear value the SDR variants' tonemap maps to 1.0.
    pub tonemap_white: f32,
}

impl ColorConfig {
    /// SDR only: HDR variants are rejected (the default).
    pub fn sdr() -> ColorConfig {
        ColorConfig { hdr: false, ..ColorConfig::hdr() }
    }

    /// HDR: reference white 203 nits, BT.709 primaries, tonemap white 4.0.
    pub fn hdr() -> ColorConfig {
        ColorConfig {
            hdr: true,
            primaries: Primaries::Bt709,
            reference_white: DEFAULT_REFERENCE_WHITE,
            tonemap_white: DEFAULT_TONEMAP_WHITE,
        }
    }

    pub fn primaries(mut self, primaries: Primaries) -> ColorConfig {
        self.primaries = primaries;
        self
    }

    pub fn reference_white(mut self, nits: u16) -> ColorConfig {
        self.reference_white = nits;
        self
    }

    pub fn tonemap_white(mut self, white: f32) -> ColorConfig {
        self.tonemap_white = white;
        self
    }
}

impl Default for ColorConfig {
    fn default() -> ColorConfig {
        ColorConfig::sdr()
    }
}

/// Runs `f` on matching pixel ranges of `src` and `dst` (`src_per` and `dst_per` values
/// per pixel) on `threads` threads.
pub(crate) fn par_pixels<A: Sync, B: Send>(
    src: &[A],
    src_per: usize,
    dst: &mut [B],
    dst_per: usize,
    threads: usize,
    f: impl Fn(&[A], &mut [B]) + Sync,
) {
    let pixels = src.len() / src_per;
    let per_thread = pixels.div_ceil(threads.max(1)).max(1);
    std::thread::scope(|s| {
        for (src, dst) in src.chunks(per_thread * src_per).zip(dst.chunks_mut(per_thread * dst_per)) {
            let f = &f;
            s.spawn(move || f(src, dst));
        }
    });
}

/// The tonemapped SDR frame (sRGB RGBA8) of a linear RGBA frame.
pub(crate) fn to_sdr(hdr: &[f32], color: &ColorConfig, threads: usize, out: &mut Vec<u8>) {
    out.resize(hdr.len(), 0);
    par_pixels(hdr, 4, out, 4, threads, |src, dst| {
        for (s, d) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0.iter_mut()) {
            let rgb = tonemap_srgb8([s[0], s[1], s[2]], color.primaries, color.tonemap_white);
            d[..3].copy_from_slice(&rgb);
            d[3] = (s[3].clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    });
}

/// The HDR frame of an sRGB RGBA8 frame: linear, in the configured primaries, alpha 1.
pub(crate) fn from_sdr(rgba: &[u8], color: &ColorConfig, threads: usize, out: &mut Vec<f32>) {
    out.resize(rgba.len(), 0.0);
    par_pixels(rgba, 4, out, 4, threads, |src, dst| {
        for (s, d) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0.iter_mut()) {
            let rgb = [s[0], s[1], s[2]].map(|v| srgb_to_linear(v as f32 / 255.0));
            let rgb = convert_primaries(rgb, Primaries::Bt709, color.primaries).map(clamp_hdr);
            d[..3].copy_from_slice(&rgb);
            d[3] = 1.0;
        }
    });
}

/// A pushed HDR frame as HDR variants store it: RGB clamped to `0..=65504` (NaN to 0),
/// alpha 1.
pub(crate) fn clamp_frame(hdr: &[f32], threads: usize, out: &mut Vec<f32>) {
    out.resize(hdr.len(), 0.0);
    par_pixels(hdr, 4, out, 4, threads, |src, dst| {
        for (s, d) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0.iter_mut()) {
            for c in 0..3 {
                d[c] = clamp_hdr(s[c]);
            }
            d[3] = 1.0;
        }
    });
}

/// Temporal-skip keys of an HDR frame: 10-bit PQ codes of R, G and B, alpha 0.
pub(crate) fn skip_keys(hdr: &[f32], reference_white: f32, threads: usize, out: &mut Vec<u16>) {
    out.resize(hdr.len(), 0);
    par_pixels(hdr, 4, out, 4, threads, |src, dst| {
        for (s, d) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0.iter_mut()) {
            for c in 0..3 {
                d[c] = pq_code10(s[c], reference_white);
            }
            d[3] = 0;
        }
    });
}

/// Brightest `max(R, G, B)` and mean `max(R, G, B)` of a clamped HDR frame, in its linear
/// units.
pub(crate) fn light_level(hdr: &[f32]) -> (f32, f32) {
    let (mut max, mut sum) = (0f32, 0f64);
    for p in hdr.as_chunks::<4>().0.iter() {
        let m = p[0].max(p[1]).max(p[2]);
        max = max.max(m);
        sum += m as f64;
    }
    (max, (sum / (hdr.len() / 4).max(1) as f64) as f32)
}

/// Nits, rounded up and clamped to `u16`.
pub(crate) fn nits(value: f32, reference_white: u16) -> u16 {
    (value as f64 * reference_white as f64).ceil().clamp(0.0, u16::MAX as f64) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use praxi_format::color;

    #[test]
    fn sdr_and_hdr_frames() {
        let color = ColorConfig::hdr();
        let mut hdr = Vec::new();
        from_sdr(&[255, 128, 0, 7], &color, 2, &mut hdr);
        assert!((hdr[0] - 1.0).abs() < 1e-6 && hdr[2] == 0.0 && hdr[3] == 1.0, "{hdr:?}");
        assert!((hdr[1] - color::srgb_to_linear(128.0 / 255.0)).abs() < 1e-6);
        let mut sdr = Vec::new();
        to_sdr(&[4.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 1.0], &color, 2, &mut sdr);
        assert_eq!(sdr[4..], [0, 0, 0, 255]);
        assert_eq!(sdr[3], 128, "alpha kept");
        let mut clamped = Vec::new();
        clamp_frame(&[f32::NAN, -1.0, 1e6, 0.0], 1, &mut clamped);
        assert_eq!(clamped, [0.0, 0.0, 65504.0, 1.0]);
    }

    #[test]
    fn light_levels() {
        let frame = [1.0, 0.5, 0.0, 1.0, 0.0, 0.0, 3.0, 1.0];
        assert_eq!(light_level(&frame), (3.0, 2.0));
        assert_eq!(nits(3.0, 203), 609);
        assert_eq!(nits(1e9, 203), u16::MAX);
    }
}
