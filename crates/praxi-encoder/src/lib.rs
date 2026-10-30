//! Encoder for praxi video files (`docs/features/encoder.md`, `docs/features/video-codec.md`).

use ktx::astcenc::{
    self, AstcencContext, AstcencImage, AstcencImageData, AstcencOutput, AstcencProfile,
    AstcencSwizzle,
};
use ktx::basisu::{uastc_bc7, uastc_enc};
use praxi_format::{
    COLOR_FLAG_ALPHA, COLOR_FLAG_SRGB, StreamInfo, TextureFormat, TimeBase, VideoParams, Writer,
};
use std::fmt;
use std::io::{Seek, Write};

#[derive(Debug)]
pub enum Error {
    Format(praxi_format::Error),
    Config(String),
    Encode(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Format(e) => e.fmt(f),
            Error::Config(msg) => write!(f, "invalid encoder config: {msg}"),
            Error::Encode(msg) => write!(f, "encoding failed: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Format(e) => Some(e),
            _ => None,
        }
    }
}

impl From<praxi_format::Error> for Error {
    fn from(e: praxi_format::Error) -> Self {
        Error::Format(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    #[default]
    Fast,
    Medium,
    Thorough,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VariantKind {
    /// UASTC transcoded to BC7, with optional UASTC RDO (`rdo_lambda > 0`).
    Bc7 { rdo_lambda: f32 },
    /// ASTC with square blocks of 4, 6 or 8 pixels.
    Astc { block: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VariantConfig {
    pub kind: VariantKind,
    pub quality: Quality,
}

impl VariantConfig {
    pub fn bc7() -> VariantConfig {
        VariantConfig { kind: VariantKind::Bc7 { rdo_lambda: 0.0 }, quality: Quality::Fast }
    }

    pub fn astc(block: u8) -> VariantConfig {
        VariantConfig { kind: VariantKind::Astc { block }, quality: Quality::Fast }
    }

    pub fn quality(mut self, quality: Quality) -> VariantConfig {
        self.quality = quality;
        self
    }

    /// Sets the UASTC RDO strength of a BC7 variant; ignored by ASTC.
    pub fn rdo(mut self, lambda: f32) -> VariantConfig {
        if let VariantKind::Bc7 { rdo_lambda } = &mut self.kind {
            *rdo_lambda = lambda;
        }
        self
    }

    pub fn format(&self) -> Option<TextureFormat> {
        match self.kind {
            VariantKind::Bc7 { .. } => Some(TextureFormat::Bc7),
            VariantKind::Astc { block: 4 } => Some(TextureFormat::Astc4x4),
            VariantKind::Astc { block: 6 } => Some(TextureFormat::Astc6x6),
            VariantKind::Astc { block: 8 } => Some(TextureFormat::Astc8x8),
            VariantKind::Astc { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub variants: Vec<VariantConfig>,
    pub keyint: u32,
    pub skip_threshold: u8,
    /// Marker times in seconds and names.
    pub markers: Vec<(f64, String)>,
    /// Whether the alpha channel is meaningful (sets the stream's alpha flag).
    pub alpha: bool,
    /// Worker threads; 0 uses `std::thread::available_parallelism`.
    pub threads: usize,
}

impl EncoderConfig {
    /// A config with no variants, keyframes every 60 frames and skip threshold 3.
    pub fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> EncoderConfig {
        EncoderConfig {
            width,
            height,
            fps_num,
            fps_den,
            variants: Vec::new(),
            keyint: 60,
            skip_threshold: 3,
            markers: Vec::new(),
            alpha: false,
            threads: 0,
        }
    }

    pub fn variant(mut self, v: VariantConfig) -> EncoderConfig {
        self.variants.push(v);
        self
    }

    pub fn keyint(mut self, keyint: u32) -> EncoderConfig {
        self.keyint = keyint;
        self
    }

    pub fn skip_threshold(mut self, threshold: u8) -> EncoderConfig {
        self.skip_threshold = threshold;
        self
    }

    pub fn marker(mut self, seconds: f64, name: impl Into<String>) -> EncoderConfig {
        self.markers.push((seconds, name.into()));
        self
    }

    pub fn alpha(mut self, alpha: bool) -> EncoderConfig {
        self.alpha = alpha;
        self
    }

    pub fn threads(mut self, threads: usize) -> EncoderConfig {
        self.threads = threads;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct VariantStats {
    pub format: Option<TextureFormat>,
    /// Bytes in the file, chunk headers included.
    pub bytes: u64,
    pub frames: u64,
    pub keyframes: u64,
    pub blocks: u64,
    /// Blocks left out of delta frames.
    pub skipped_blocks: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub frames: u64,
    pub duration_us: u64,
    pub variants: Vec<VariantStats>,
}

enum BlockEncoder {
    Bc7 { level: u32, rdo_lambda: f32 },
    Astc { ctx: Box<AstcencContext> },
}

struct Variant {
    stream: u16,
    params: VideoParams,
    encoder: BlockEncoder,
    /// Source pixels and encoded bytes of the last emitted block at each position.
    ref_src: Vec<u8>,
    ref_enc: Vec<u8>,
    src_blocks: Vec<u8>,
    encoded: Vec<u8>,
    payload: Vec<u8>,
    stats: VariantStats,
}

pub struct Encoder<W: Write + Seek> {
    writer: Writer<W>,
    config: EncoderConfig,
    threads: usize,
    variants: Vec<Variant>,
    time_base: TimeBase,
    /// Markers as (microseconds, name), sorted.
    markers: Vec<(u64, String)>,
    next_marker: usize,
    marker_stream: u16,
    frame: u64,
}

impl<W: Write + Seek> Encoder<W> {
    pub fn new(out: W, config: EncoderConfig) -> Result<Encoder<W>> {
        let c = &config;
        if c.width == 0 || c.height == 0 {
            return Err(Error::Config("zero frame size".into()));
        }
        if c.fps_num == 0 || c.fps_den == 0 {
            return Err(Error::Config("zero frame rate".into()));
        }
        if c.variants.is_empty() {
            return Err(Error::Config("no variants".into()));
        }
        if c.keyint == 0 {
            return Err(Error::Config("keyint must be at least 1".into()));
        }
        let mut markers = Vec::with_capacity(c.markers.len());
        for (secs, name) in &c.markers {
            if !secs.is_finite() || *secs < 0.0 {
                return Err(Error::Config(format!("marker {name:?} has invalid time {secs}")));
            }
            markers.push(((secs * 1e6).round() as u64, name.clone()));
        }
        markers.sort_by_key(|m| m.0);
        if let Some(w) = markers.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(Error::Config(format!("markers {:?} and {:?} share a time", w[0].1, w[1].1)));
        }

        let threads = match c.threads {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
            n => n,
        };
        let color_flags = COLOR_FLAG_SRGB | if c.alpha { COLOR_FLAG_ALPHA } else { 0 };
        let time_base = TimeBase::new(c.fps_den, c.fps_num);
        let mut streams = Vec::new();
        let mut variants = Vec::new();
        for (i, v) in c.variants.iter().enumerate() {
            let Some(format) = v.format() else {
                return Err(Error::Config(format!("unsupported variant {:?}", v.kind)));
            };
            let params = VideoParams { width: c.width, height: c.height, format, color_flags };
            streams.push(StreamInfo::video(0, time_base, params));
            let encoder = match v.kind {
                VariantKind::Bc7 { rdo_lambda } => {
                    let level = match v.quality {
                        Quality::Fast => uastc_enc::C_PACK_UASTC_LEVEL_FASTEST,
                        Quality::Medium => uastc_enc::C_PACK_UASTC_LEVEL_DEFAULT,
                        Quality::Thorough => uastc_enc::C_PACK_UASTC_LEVEL_SLOWER,
                    };
                    BlockEncoder::Bc7 { level, rdo_lambda }
                }
                VariantKind::Astc { block } => {
                    let preset = match v.quality {
                        Quality::Fast => astcenc::ASTCENC_PRE_FAST,
                        Quality::Medium => astcenc::ASTCENC_PRE_MEDIUM,
                        Quality::Thorough => astcenc::ASTCENC_PRE_THOROUGH,
                    };
                    let b = block as u32;
                    let cfg = astcenc::astcenc_config_init(AstcencProfile::LdrSrgb, b, b, 1, preset, 0)
                        .map_err(|e| Error::Encode(format!("astcenc config: {e:?}")))?;
                    let ctx = astcenc::astcenc_context_alloc(&cfg, threads as u32)
                        .map_err(|e| Error::Encode(format!("astcenc context: {e:?}")))?;
                    BlockEncoder::Astc { ctx }
                }
            };
            variants.push(Variant {
                stream: i as u16,
                params,
                encoder,
                ref_src: Vec::new(),
                ref_enc: Vec::new(),
                src_blocks: Vec::new(),
                encoded: Vec::new(),
                payload: Vec::new(),
                stats: VariantStats { format: Some(format), ..Default::default() },
            });
        }
        let marker_stream = streams.len() as u16;
        if !markers.is_empty() {
            streams.push(StreamInfo::markers(1));
        }
        let writer = Writer::new(out, streams)?;
        Ok(Encoder { writer, config, threads, variants, time_base, markers, next_marker: 0, marker_stream, frame: 0 })
    }

    /// Encodes one frame: `width * height * 4` bytes of sRGB RGBA8, rows top to bottom.
    pub fn push_frame(&mut self, rgba: &[u8]) -> Result<()> {
        let (w, h) = (self.config.width as usize, self.config.height as usize);
        if rgba.len() != w * h * 4 {
            return Err(Error::Config(format!("frame has {} bytes, expected {}", rgba.len(), w * h * 4)));
        }
        let pts = self.frame;
        self.write_markers_until(self.time_base.to_micros(pts))?;

        let keyframe = pts % self.config.keyint as u64 == 0;
        for v in &mut self.variants {
            let (bw, bh) = v.params.format.block_size();
            gather_blocks(rgba, w, h, bw as usize, bh as usize, &mut v.src_blocks);
            match &mut v.encoder {
                BlockEncoder::Bc7 { level, rdo_lambda } => {
                    encode_bc7(&v.src_blocks, *level, *rdo_lambda, self.threads, &mut v.encoded)
                }
                BlockEncoder::Astc { ctx } => encode_astc(ctx, rgba, w, h, &v.params, self.threads, &mut v.encoded)?,
            }
            build_payload(v, keyframe, self.config.skip_threshold);
            let before = self.writer.bytes_written();
            self.writer.write_chunk(v.stream, pts, keyframe, &v.payload, None)?;
            v.stats.bytes += self.writer.bytes_written() - before;
            v.stats.frames += 1;
            v.stats.keyframes += keyframe as u64;
            v.stats.blocks += v.params.block_count() as u64;
        }
        self.frame += 1;
        Ok(())
    }

    /// Writes the remaining markers and the index, and returns the output and statistics.
    pub fn finish(mut self) -> Result<(W, Stats)> {
        self.write_markers_until(u64::MAX)?;
        let mut duration_us = self.time_base.to_micros(self.frame);
        if let Some(last) = self.markers.last() {
            duration_us = duration_us.max(last.0);
        }
        let out = self.writer.finish(duration_us)?;
        let stats = Stats {
            frames: self.frame,
            duration_us,
            variants: self.variants.into_iter().map(|v| v.stats).collect(),
        };
        Ok((out, stats))
    }

    fn write_markers_until(&mut self, time_us: u64) -> Result<()> {
        while let Some((at, name)) = self.markers.get(self.next_marker) {
            if *at > time_us {
                break;
            }
            self.writer.write_chunk(self.marker_stream, *at, true, name.as_bytes(), None)?;
            self.next_marker += 1;
        }
        Ok(())
    }
}

/// Splits an RGBA image into blocks of `bw * bh` pixels (row-major blocks, row-major pixels
/// inside a block), repeating the last column and row to pad partial blocks.
fn gather_blocks(rgba: &[u8], w: usize, h: usize, bw: usize, bh: usize, out: &mut Vec<u8>) {
    let (nx, ny) = (w.div_ceil(bw), h.div_ceil(bh));
    out.clear();
    out.reserve(nx * ny * bw * bh * 4);
    for by in 0..ny {
        for bx in 0..nx {
            for y in 0..bh {
                let sy = (by * bh + y).min(h - 1);
                for x in 0..bw {
                    let sx = (bx * bw + x).min(w - 1);
                    let i = (sy * w + sx) * 4;
                    out.extend_from_slice(&rgba[i..i + 4]);
                }
            }
        }
    }
}

fn encode_bc7(src_blocks: &[u8], level: u32, rdo_lambda: f32, threads: usize, out: &mut Vec<u8>) {
    let n = src_blocks.len() / 64;
    let flags = level | uastc_enc::C_PACK_UASTC_FAVOR_BC7_ERROR;
    let mut uastc = vec![[0u8; 16]; n];
    let per_thread = n.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (ci, part) in uastc.chunks_mut(per_thread).enumerate() {
            s.spawn(move || {
                for (i, block) in part.iter_mut().enumerate() {
                    let at = (ci * per_thread + i) * 64;
                    let px: &[u8; 64] = src_blocks[at..at + 64].try_into().expect("64-byte block");
                    uastc_enc::encode_uastc(px, block, flags);
                }
            });
        }
    });
    if rdo_lambda > 0.0 {
        let pixels: Vec<[u8; 4]> = src_blocks.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
        let params = uastc_enc::UastcRdoParams { lambda: rdo_lambda, ..Default::default() };
        uastc_enc::uastc_rdo(n as u32, &mut uastc, &pixels, &params, flags, threads as u32);
    }
    out.resize(n * 16, 0);
    for (dst, src) in out.chunks_exact_mut(16).zip(&uastc) {
        let dst: &mut [u8; 16] = dst.try_into().expect("16-byte block");
        uastc_bc7::transcode_uastc_to_bc7(src, dst);
    }
}

fn encode_astc(
    ctx: &mut AstcencContext,
    rgba: &[u8],
    w: usize,
    h: usize,
    params: &VideoParams,
    threads: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    let image = AstcencImage {
        dim_x: w as u32,
        dim_y: h as u32,
        dim_z: 1,
        data: AstcencImageData::U8(vec![rgba.to_vec()]),
    };
    out.resize(params.frame_size(), 0);
    let result = {
        let work: AstcencOutput = std::sync::Mutex::new(&mut out[..]);
        let ctx: &AstcencContext = ctx;
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    let (image, work) = (&image, &work);
                    s.spawn(move || astcenc::astcenc_compress_image(ctx, image, &AstcencSwizzle::RGBA, work, t as u32))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or(Err(astcenc::AstcencError::BadContext))).collect::<Vec<_>>()
        })
    };
    if let Some(Err(e)) = result.into_iter().find(|r| r.is_err()) {
        return Err(Error::Encode(format!("astcenc: {e:?}")));
    }
    astcenc::astcenc_compress_reset(ctx).map_err(|e| Error::Encode(format!("astcenc reset: {e:?}")))?;
    Ok(())
}

/// Builds the chunk payload (`docs/spec/PRAXI.md#video-payload`) and updates the references.
fn build_payload(v: &mut Variant, keyframe: bool, threshold: u8) {
    let bb = v.params.format.block_bytes() as usize;
    let (bw, bh) = v.params.format.block_size();
    let sb = (bw * bh * 4) as usize;
    let n = v.params.block_count();
    v.payload.clear();
    if keyframe {
        v.payload.extend_from_slice(&v.encoded);
        v.ref_src.clone_from(&v.src_blocks);
        v.ref_enc.clone_from(&v.encoded);
        return;
    }
    v.payload.resize(v.params.mask_size(), 0);
    for i in 0..n {
        let src = &v.src_blocks[i * sb..(i + 1) * sb];
        let enc = &v.encoded[i * bb..(i + 1) * bb];
        let ref_src = &mut v.ref_src[i * sb..(i + 1) * sb];
        let ref_enc = &mut v.ref_enc[i * bb..(i + 1) * bb];
        if enc == ref_enc {
            ref_src.copy_from_slice(src);
        } else if src.iter().zip(ref_src.iter()).all(|(a, b)| a.abs_diff(*b) <= threshold) {
            // Close enough to what the decoder already shows.
        } else {
            v.payload[i / 8] |= 1 << (i % 8);
            v.payload.extend_from_slice(enc);
            ref_src.copy_from_slice(src);
            ref_enc.copy_from_slice(enc);
            continue;
        }
        v.stats.skipped_blocks += 1;
    }
}
