//! HDR variants (`docs/features/hdr.md#tests`).

use praxi::format::color::{nits_to_pq, tonemap_srgb8};
use praxi::{Capabilities, Player, Primaries, TextureFormat};
use praxi_encoder::{ColorConfig, Encoder, EncoderConfig, VariantConfig};
use std::io::Cursor;
use std::time::Duration;

const W: u32 = 30;
const H: u32 = 14;

/// A gradient up to 40.0 with colored highlights and a transparent corner: four frames,
/// the last two with a brighter square.
fn frames() -> Vec<Vec<f32>> {
    let base: Vec<f32> = (0..W * H)
        .flat_map(|i| {
            let (x, y) = (i % W, i / W);
            let v = 40.0 * (x as f32 / (W - 1) as f32).powi(3);
            let a = if x < 4 && y < 4 { 0.0 } else { 1.0 };
            [v, v * 0.5 + y as f32 * 0.02, 0.05 + v * 0.1, a]
        })
        .collect();
    let mut bright = base.clone();
    for y in 4..8 {
        for x in 8..12 {
            let i = ((y * W + x) * 4) as usize;
            bright[i..i + 3].copy_from_slice(&[12.0, 3.0, 0.5]);
        }
    }
    vec![base.clone(), base, bright.clone(), bright]
}

fn encode(config: EncoderConfig) -> (Vec<u8>, praxi_encoder::Stats) {
    let mut enc = Encoder::new(Cursor::new(Vec::new()), config.threads(2)).unwrap();
    for f in frames() {
        enc.push_frame_hdr(&f).unwrap();
    }
    let (out, stats) = enc.finish().unwrap();
    (out.into_inner(), stats)
}

fn hdr_config() -> EncoderConfig {
    EncoderConfig::new(W, H, 10, 1).color(ColorConfig::hdr()).alpha(true)
}

/// PSNR of 10-bit PQ code values (linear 1.0 = 203 nits).
fn pq_psnr(a: &[f32], b: &[f32]) -> f64 {
    let mut se = 0.0;
    let mut n = 0;
    for (x, y) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0.iter()) {
        for c in 0..3 {
            let d = (nits_to_pq(x[c] * 203.0) - nits_to_pq(y[c] * 203.0)) as f64;
            se += d * d;
            n += 1;
        }
    }
    10.0 * (1.0 / (se / n as f64)).log10()
}

#[test]
fn hdr_variants_decode_close_to_the_source() {
    let source = frames();
    for (variant, format) in [
        (VariantConfig::bc6h(), TextureFormat::Bc6h),
        (VariantConfig::astc_hdr(4), TextureFormat::Astc4x4Hdr),
        (VariantConfig::astc_hdr(6), TextureFormat::Astc6x6Hdr),
    ] {
        let (file, stats) = encode(hdr_config().variant(variant).variant(VariantConfig::bc7()));
        assert_eq!(stats.variants[0].format, Some(format));
        // Brightest: 40.0 at 203 nits; brightest frame mean comes from the last frames.
        assert_eq!(stats.variants[0].light_level.map(|l| l.0), Some(8120), "{format}");
        assert_eq!(stats.variants[1].light_level, None, "SDR variants have no light level");

        let mut p = Player::open(Cursor::new(file), &Capabilities::new([format])).unwrap();
        let params = p.tracks()[0].params;
        assert!(params.hdr() && !params.srgb() && !params.alpha(), "{format}: HDR, opaque");
        assert_eq!(params.color.primaries, Primaries::Bt709);
        assert_eq!(params.color.reference_white, 203);
        assert_eq!((params.color.max_cll, params.color.max_fall), stats.variants[0].light_level.unwrap());
        p.seek(Duration::from_millis(300)).unwrap();
        let frame = p.video_frame(0).unwrap();
        assert!(frame.hdr);
        let rgba = praxi_encoder::decode_frame_hdr(&params, frame.data).unwrap();
        assert_eq!(rgba.len(), (W * H * 4) as usize, "{format}: padding cropped");
        assert!(rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 1.0), "{format}: opaque");
        let psnr = pq_psnr(&rgba, &source[3]);
        assert!(psnr > 35.0, "{format}: PQ-PSNR {psnr:.1} dB");
        assert!(praxi_encoder::decode_frame(&params, frame.data).is_err(), "HDR needs decode_frame_hdr");
    }
}

#[test]
fn sdr_variants_of_hdr_frames_are_tonemapped() {
    let source = frames();
    let (file, _) = encode(hdr_config().variant(VariantConfig::bc6h()).variant(VariantConfig::bc7()));
    let mut p = Player::open(Cursor::new(file), &Capabilities::new([TextureFormat::Bc7])).unwrap();
    let params = p.tracks()[0].params;
    assert!(params.srgb() && params.alpha() && !params.hdr(), "the SDR variant keeps alpha");
    assert_eq!(params.color.reference_white, 0);
    p.seek(Duration::from_millis(300)).unwrap();
    let rgba = praxi_encoder::decode_frame(&params, p.video_frame(0).unwrap().data).unwrap();
    let expected: Vec<u8> = source[3]
        .as_chunks::<4>().0.iter()
        .flat_map(|s| {
            let [r, g, b] = tonemap_srgb8([s[0], s[1], s[2]], Primaries::Bt709, 4.0);
            [r, g, b, (s[3] * 255.0) as u8]
        })
        .collect();
    let se: f64 = rgba.iter().zip(&expected).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum();
    let psnr = 10.0 * (255.0f64.powi(2) / (se / rgba.len() as f64)).log10();
    assert!(psnr > 30.0, "PSNR against the tonemapped source {psnr:.1} dB");
}

#[test]
fn sdr_frames_feed_hdr_variants() {
    let sdr: Vec<u8> = (0..W * H).flat_map(|i| [(i % 256) as u8, 255, 255, 255]).collect();
    let config = hdr_config().color(ColorConfig::hdr().primaries(Primaries::Bt2020)).variant(VariantConfig::bc6h());
    let mut enc = Encoder::new(Cursor::new(Vec::new()), config).unwrap();
    enc.push_frame(&sdr).unwrap();
    let (out, stats) = enc.finish().unwrap();
    // SDR white converted to BT.2020 stays 1.0, 203 nits.
    assert_eq!(stats.variants[0].light_level.map(|l| l.0), Some(203));
    let p = Player::open(Cursor::new(out.into_inner()), &Capabilities::new([TextureFormat::Bc6h])).unwrap();
    assert_eq!(p.tracks()[0].params.color.primaries, Primaries::Bt2020);
}

#[test]
fn hdr_variants_need_an_hdr_color_config() {
    let config = EncoderConfig::new(W, H, 10, 1).variant(VariantConfig::bc6h());
    assert!(Encoder::new(Cursor::new(Vec::new()), config).is_err());
    let config = hdr_config().color(ColorConfig::hdr().reference_white(0)).variant(VariantConfig::bc6h());
    assert!(Encoder::new(Cursor::new(Vec::new()), config).is_err());
}
