//! HDR sources for `praxi encode` (`docs/features/hdr.md#cli`): transfer functions from
//! `ffprobe`, and float frames from `ffmpeg` converted to linear light.

use clap::ValueEnum;
use praxi::Primaries;
use praxi::format::color::{clamp_hdr, convert_primaries, hlg_to_nits, pq_to_nits};

/// Transfer function of a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Transfer {
    /// SMPTE ST 2084 (HDR10).
    Pq,
    /// ARIB STD-B67 / BT.2100 HLG.
    Hlg,
    /// Linear light, 1.0 at reference white (OpenEXR).
    Linear,
    /// SDR: decoded as sRGB RGBA8.
    Srgb,
}

impl Transfer {
    /// The transfer of `ffprobe`'s `color_transfer`.
    pub fn from_ffprobe(name: &str) -> Transfer {
        match name {
            "smpte2084" => Transfer::Pq,
            "arib-std-b67" => Transfer::Hlg,
            "linear" => Transfer::Linear,
            _ => Transfer::Srgb,
        }
    }

    pub fn is_hdr(self) -> bool {
        self != Transfer::Srgb
    }

    /// The name of the transfer in `ffmpeg`'s `color_trc`.
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Transfer::Pq => "smpte2084",
            Transfer::Hlg => "arib-std-b67",
            Transfer::Linear => "linear",
            Transfer::Srgb => "iec61966-2-1",
        }
    }
}

/// The `setparams` filter tagging frames with `transfer` and `primaries`, and with the YUV
/// matrix of the primaries when `matrix` is set (`docs/features/hdr.md#cli`).
pub fn setparams(transfer: Transfer, primaries: Primaries, matrix: bool) -> String {
    let mut filter = format!("setparams=color_trc={}:color_primaries={}", transfer.ffmpeg_name(), primaries.name());
    if matrix {
        let colorspace = match primaries {
            Primaries::Bt709 => "bt709",
            Primaries::Bt2020 => "bt2020nc",
        };
        filter.push_str(&format!(":colorspace={colorspace}"));
    }
    filter
}

/// Source primaries from `ffprobe`'s `color_primaries`: BT.2020, or BT.709 for `bt709` and
/// unspecified; `None` for others.
pub fn primaries_from_ffprobe(name: &str) -> Option<Primaries> {
    match name {
        "bt2020" => Some(Primaries::Bt2020),
        "bt709" | "unknown" | "" => Some(Primaries::Bt709),
        _ => None,
    }
}

/// How a float source frame becomes linear RGBA in the target primaries.
#[derive(Debug, Clone, Copy)]
pub struct Conversion {
    pub transfer: Transfer,
    pub source: Primaries,
    pub target: Primaries,
    pub reference_white: f32,
}

impl Conversion {
    /// Converts one planar `gbrpf32le` / `gbrapf32le` frame (`planes` of 3 or 4) to
    /// interleaved linear RGBA (`docs/features/hdr.md#cli`).
    pub fn convert(&self, raw: &[u8], pixels: usize, planes: usize, out: &mut Vec<f32>) {
        let sample = |plane: usize, i: usize| {
            let at = (plane * pixels + i) * 4;
            f32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]])
        };
        out.clear();
        out.reserve(pixels * 4);
        for i in 0..pixels {
            // Planes are G, B, R(, A).
            let rgb = [sample(2, i), sample(0, i), sample(1, i)];
            let linear = match self.transfer {
                Transfer::Pq => rgb.map(|v| pq_to_nits(v) / self.reference_white),
                Transfer::Hlg => hlg_to_nits(rgb).map(|v| v / self.reference_white),
                Transfer::Linear | Transfer::Srgb => rgb,
            };
            let linear = convert_primaries(linear, self.source, self.target).map(clamp_hdr);
            let alpha = if planes == 4 { sample(3, i).clamp(0.0, 1.0) } else { 1.0 };
            out.extend_from_slice(&[linear[0], linear[1], linear[2], alpha]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_planar_float_frames() {
        // One pixel, G B R A planes: PQ reference white in every channel, alpha 0.5.
        let raw: Vec<u8> = [0.5807f32, 0.5807, 0.5807, 0.5].iter().flat_map(|v| v.to_le_bytes()).collect();
        let c = Conversion { transfer: Transfer::Pq, source: Primaries::Bt2020, target: Primaries::Bt709, reference_white: 203.0 };
        let mut out = Vec::new();
        c.convert(&raw, 1, 4, &mut out);
        assert!(out[..3].iter().all(|v| (v - 1.0).abs() < 0.01), "{out:?}");
        assert_eq!(out[3], 0.5);
        // Linear sources keep their values; planes are reordered to RGB.
        let raw: Vec<u8> = [2.0f32, 3.0, 1.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        let c = Conversion { transfer: Transfer::Linear, source: Primaries::Bt709, target: Primaries::Bt709, reference_white: 203.0 };
        c.convert(&raw, 1, 3, &mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 1.0]);
    }

    #[test]
    fn ffprobe_names() {
        assert_eq!(Transfer::from_ffprobe("smpte2084"), Transfer::Pq);
        assert_eq!(Transfer::from_ffprobe("arib-std-b67"), Transfer::Hlg);
        assert_eq!(Transfer::from_ffprobe("bt709"), Transfer::Srgb);
        assert_eq!(primaries_from_ffprobe("unknown"), Some(Primaries::Bt709));
        assert_eq!(primaries_from_ffprobe("smpte432"), None);
    }

    #[test]
    fn setparams_filters() {
        assert_eq!(
            setparams(Transfer::Pq, Primaries::Bt2020, true),
            "setparams=color_trc=smpte2084:color_primaries=bt2020:colorspace=bt2020nc"
        );
        assert_eq!(setparams(Transfer::Hlg, Primaries::Bt709, false), "setparams=color_trc=arib-std-b67:color_primaries=bt709");
    }
}
