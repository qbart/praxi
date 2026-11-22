//! Encoder for praxi video files (`docs/features/encoder.md`, `docs/features/video-codec.md`,
//! `docs/features/audio.md`).

mod decode;
mod hdr;
mod motion;
mod opus;

pub use decode::{decode_frame, decode_frame_hdr};
pub use hdr::ColorConfig;
pub use praxi_format::Primaries;
pub use motion::MotionStats;

use praxi_format::{
    AudioParams, COLOR_FLAG_ALPHA, COLOR_FLAG_HDR, adpcm, COLOR_FLAG_SRGB, Compression, Cue, StreamInfo, TextureFormat, TimeBase,
    VideoParams, Writer,
};

pub use praxi_format::Chroma;
use motion::MotionEncoder;

pub use praxi_format::{SampleFormat, Value};
use std::collections::BTreeSet;
use std::fmt;
use std::io::{Cursor, Read, Seek, Write};
use std::time::Duration;

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
    /// BC7 (`docs/features/video-codec.md#encoding-paths`).
    Bc7,
    /// Opaque BC1 (`docs/features/texture-variants.md#formats`).
    Bc1,
    /// ASTC with square blocks of 4, 6 or 8 pixels, with optional astcenc LZ RDO
    /// (`rdo_lambda > 0`).
    Astc { block: u8, rdo_lambda: f32 },
    /// The motion codec (`docs/features/motion-codec.md`).
    Motion { qp: u8, chroma: Chroma, slices: u32 },
    /// BC6H, HDR (`docs/features/hdr.md`).
    Bc6h,
    /// ASTC HDR with square blocks of 4 or 6 pixels.
    AstcHdr { block: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VariantConfig {
    pub kind: VariantKind,
    pub quality: Quality,
}

impl VariantConfig {
    pub fn bc7() -> VariantConfig {
        VariantConfig { kind: VariantKind::Bc7, quality: Quality::Fast }
    }

    /// Opaque BC1: half the size of BC7, lower quality; alpha is dropped.
    pub fn bc1() -> VariantConfig {
        VariantConfig { kind: VariantKind::Bc1, quality: Quality::Fast }
    }

    pub fn astc(block: u8) -> VariantConfig {
        VariantConfig { kind: VariantKind::Astc { block, rdo_lambda: 0.0 }, quality: Quality::Fast }
    }

    /// BC6H HDR; needs [`ColorConfig::hdr`].
    pub fn bc6h() -> VariantConfig {
        VariantConfig { kind: VariantKind::Bc6h, quality: Quality::Fast }
    }

    /// ASTC HDR with blocks of 4 or 6 pixels; needs [`ColorConfig::hdr`].
    pub fn astc_hdr(block: u8) -> VariantConfig {
        VariantConfig { kind: VariantKind::AstcHdr { block }, quality: Quality::Fast }
    }

    /// The motion codec with `qp` 16 (about BC7's PSNR), 4:2:0 chroma and 16 slices.
    pub fn motion() -> VariantConfig {
        VariantConfig { kind: VariantKind::Motion { qp: 16, chroma: Chroma::C420, slices: 16 }, quality: Quality::Fast }
    }

    /// Sets the quantizer of a motion variant (0 to 51); ignored by others.
    pub fn qp(mut self, value: u8) -> VariantConfig {
        if let VariantKind::Motion { qp, .. } = &mut self.kind {
            *qp = value;
        }
        self
    }

    /// Sets the chroma resolution of a motion variant; ignored by others.
    pub fn chroma(mut self, value: Chroma) -> VariantConfig {
        if let VariantKind::Motion { chroma, .. } = &mut self.kind {
            *chroma = value;
        }
        self
    }

    /// Sets the slices per frame of a motion variant; ignored by others.
    pub fn slices(mut self, value: u32) -> VariantConfig {
        if let VariantKind::Motion { slices, .. } = &mut self.kind {
            *slices = value;
        }
        self
    }

    pub fn quality(mut self, quality: Quality) -> VariantConfig {
        self.quality = quality;
        self
    }

    /// Sets the astcenc LZ RDO strength of an ASTC variant (typically 10 to 40, 0 off);
    /// ignored by others.
    pub fn rdo(mut self, lambda: f32) -> VariantConfig {
        if let VariantKind::Astc { rdo_lambda, .. } = &mut self.kind {
            *rdo_lambda = lambda;
        }
        self
    }

    pub fn format(&self) -> Option<TextureFormat> {
        match self.kind {
            VariantKind::Bc7 => Some(TextureFormat::Bc7),
            VariantKind::Bc1 => Some(TextureFormat::Bc1),
            VariantKind::Astc { block: 4, .. } => Some(TextureFormat::Astc4x4),
            VariantKind::Astc { block: 6, .. } => Some(TextureFormat::Astc6x6),
            VariantKind::Astc { block: 8, .. } => Some(TextureFormat::Astc8x8),
            VariantKind::Astc { .. } => None,
            VariantKind::Motion { .. } => Some(TextureFormat::Motion),
            VariantKind::Bc6h => Some(TextureFormat::Bc6h),
            VariantKind::AstcHdr { block: 4 } => Some(TextureFormat::Astc4x4Hdr),
            VariantKind::AstcHdr { block: 6 } => Some(TextureFormat::Astc6x6Hdr),
            VariantKind::AstcHdr { .. } => None,
        }
    }
}

/// One cue (`docs/features/cues.md`); times in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct CueConfig {
    pub time: f64,
    pub duration: f64,
    pub kind: String,
    pub fields: Vec<(String, Value)>,
}

impl CueConfig {
    pub fn new(time: f64, kind: impl Into<String>) -> CueConfig {
        CueConfig { time, duration: 0.0, kind: kind.into(), fields: Vec::new() }
    }

    pub fn duration(mut self, seconds: f64) -> CueConfig {
        self.duration = seconds;
        self
    }

    pub fn field(mut self, key: impl Into<String>, value: impl Into<Value>) -> CueConfig {
        self.fields.push((key.into(), value.into()));
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CueStreamConfig {
    pub name: String,
    pub cues: Vec<CueConfig>,
}

impl CueStreamConfig {
    pub fn new(name: impl Into<String>) -> CueStreamConfig {
        CueStreamConfig { name: name.into(), cues: Vec::new() }
    }

    pub fn cue(mut self, cue: CueConfig) -> CueStreamConfig {
        self.cues.push(cue);
        self
    }
}

/// One audio stream (`docs/features/audio.md#encoder`). Samples are interleaved `f32`.
pub struct AudioTrackConfig {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// WAVE speaker bits, `0` for unspecified.
    pub channel_mask: u32,
    pub format: SampleFormat,
    /// Opus bitrate of the whole track in bits per second; `None` is 64000 per channel.
    pub bitrate: Option<u32>,
    /// `None` shares the group of earlier tracks with the same name, or takes a new one.
    pub group: Option<u16>,
    source: Box<dyn Read + Send>,
}

impl AudioTrackConfig {
    pub fn from_samples(name: impl Into<String>, sample_rate: u32, channels: u16, samples: Vec<f32>) -> AudioTrackConfig {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        AudioTrackConfig::from_reader(name, sample_rate, channels, Cursor::new(bytes))
    }

    /// Reads samples as little-endian `f32` bytes (what `ffmpeg -f f32le` writes), pulled
    /// as the encoder needs them.
    pub fn from_reader(
        name: impl Into<String>,
        sample_rate: u32,
        channels: u16,
        reader: impl Read + Send + 'static,
    ) -> AudioTrackConfig {
        AudioTrackConfig {
            name: name.into(),
            sample_rate,
            channels,
            channel_mask: 0,
            format: SampleFormat::S16,
            bitrate: None,
            group: None,
            source: Box::new(reader),
        }
    }

    pub fn channel_mask(mut self, mask: u32) -> AudioTrackConfig {
        self.channel_mask = mask;
        self
    }

    pub fn format(mut self, format: SampleFormat) -> AudioTrackConfig {
        self.format = format;
        self
    }

    pub fn group(mut self, group: u16) -> AudioTrackConfig {
        self.group = Some(group);
        self
    }

    /// The Opus bitrate of the whole track, in bits per second; other formats ignore it.
    pub fn bitrate(mut self, bits_per_second: u32) -> AudioTrackConfig {
        self.bitrate = Some(bits_per_second);
        self
    }
}

impl fmt::Debug for AudioTrackConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioTrackConfig")
            .field("name", &self.name)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("channel_mask", &self.channel_mask)
            .field("format", &self.format)
            .field("bitrate", &self.bitrate)
            .field("group", &self.group)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub variants: Vec<VariantConfig>,
    pub keyint: u32,
    pub skip_threshold: u8,
    /// Percent byte planes must save before a chunk uses them (`docs/features/video-codec.md`).
    pub plane_min_saving: f64,
    /// Cue streams; empty ones are left out of the file.
    pub cue_streams: Vec<CueStreamConfig>,
    /// Whether the alpha channel is meaningful (sets the stream's alpha flag).
    pub alpha: bool,
    /// Color of the frames and of HDR variants (`docs/features/hdr.md#library`).
    pub color: ColorConfig,
    /// Worker threads of the motion encoder; 0 uses `std::thread::available_parallelism`.
    /// Block formats are encoded by `imgio` on all cores.
    pub threads: usize,
    pub audio: Vec<AudioTrackConfig>,
    /// How far ahead of their time audio chunks are stored.
    pub audio_preload: Duration,
    /// Length of an audio chunk.
    pub audio_chunk: Duration,
}

impl EncoderConfig {
    /// A config with no variants, keyframes every 60 frames, skip threshold 3, and audio
    /// preload 250 ms and chunks 100 ms.
    pub fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> EncoderConfig {
        EncoderConfig {
            width,
            height,
            fps_num,
            fps_den,
            variants: Vec::new(),
            keyint: 60,
            skip_threshold: 3,
            plane_min_saving: praxi_format::DEFAULT_PLANE_MIN_SAVING,
            cue_streams: Vec::new(),
            alpha: false,
            color: ColorConfig::sdr(),
            threads: 0,
            audio: Vec::new(),
            audio_preload: Duration::from_millis(250),
            audio_chunk: Duration::from_millis(100),
        }
    }

    pub fn audio(mut self, track: AudioTrackConfig) -> EncoderConfig {
        self.audio.push(track);
        self
    }

    pub fn audio_preload(mut self, preload: Duration) -> EncoderConfig {
        self.audio_preload = preload;
        self
    }

    pub fn audio_chunk(mut self, chunk: Duration) -> EncoderConfig {
        self.audio_chunk = chunk;
        self
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

    /// Use byte planes only when they save at least `percent` (0 to 100, default 8).
    pub fn plane_min_saving(mut self, percent: f64) -> EncoderConfig {
        self.plane_min_saving = percent;
        self
    }

    /// Adds an instantaneous cue to the stream named `cues`, creating it if needed.
    pub fn cue(self, seconds: f64, kind: impl Into<String>) -> EncoderConfig {
        self.cue_with(CueConfig::new(seconds, kind))
    }

    /// Adds a cue to the stream named `cues`, creating it if needed.
    pub fn cue_with(mut self, cue: CueConfig) -> EncoderConfig {
        match self.cue_streams.iter_mut().find(|s| s.name == "cues") {
            Some(s) => s.cues.push(cue),
            None => self.cue_streams.push(CueStreamConfig::new("cues").cue(cue)),
        }
        self
    }

    pub fn cue_stream(mut self, stream: CueStreamConfig) -> EncoderConfig {
        self.cue_streams.push(stream);
        self
    }

    pub fn alpha(mut self, alpha: bool) -> EncoderConfig {
        self.alpha = alpha;
        self
    }

    pub fn color(mut self, color: ColorConfig) -> EncoderConfig {
        self.color = color;
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
    /// Blocks left out of delta frames (skipped macroblocks for motion variants).
    pub skipped_blocks: u64,
    /// Macroblock counts of motion variants.
    pub motion: Option<MotionStats>,
    /// HDR variants: `max_cll` and `max_fall` in nits.
    pub light_level: Option<(u16, u16)>,
}

#[derive(Debug, Clone, Default)]
pub struct AudioStats {
    pub name: String,
    pub channels: u16,
    /// Bytes in the file, chunk headers included.
    pub bytes: u64,
    /// Sample frames.
    pub frames: u64,
    pub chunks: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub frames: u64,
    pub duration_us: u64,
    pub variants: Vec<VariantStats>,
    pub audio: Vec<AudioStats>,
}

struct AudioTrack {
    stream: u16,
    channels: usize,
    format: SampleFormat,
    source: Box<dyn Read + Send>,
    chunk_frames: usize,
    next_pts: u64,
    /// The source is exhausted.
    done: bool,
    input: Vec<u8>,
    /// Rounded samples, for ADPCM.
    pcm: Vec<i16>,
    adpcm: adpcm::Encoder,
    opus: Option<opus::Encoder>,
    payload: Vec<u8>,
    stats: AudioStats,
}

enum BlockEncoder {
    /// A block format encoded by `imgio`.
    Texture { format: imgio::Format, options: imgio::EncodeOptions },
    Motion(Box<MotionEncoder>),
}

struct Variant {
    stream: u16,
    params: VideoParams,
    encoder: BlockEncoder,
    /// Skip keys (RGBA8 values, or 10-bit PQ codes for HDR) and encoded bytes of the last
    /// emitted block at each position.
    ref_src: Vec<u16>,
    ref_enc: Vec<u8>,
    src_blocks: Vec<u16>,
    encoded: Vec<u8>,
    payload: Vec<u8>,
    stats: VariantStats,
}

pub struct Encoder<W: Write + Seek> {
    writer: Writer<W>,
    config: EncoderConfig,
    variants: Vec<Variant>,
    audio: Vec<AudioTrack>,
    time_base: TimeBase,
    /// Cues as (start in microseconds, stream, cue), sorted by start.
    cues: Vec<(u64, u16, Cue)>,
    next_cue: usize,
    frame: u64,
    threads: usize,
    /// The frame as SDR RGBA8, linear HDR RGBA, and skip keys of each, when needed.
    sdr_frame: Vec<u8>,
    hdr_frame: Vec<f32>,
    sdr_keys: Vec<u16>,
    hdr_keys: Vec<u16>,
    /// Brightest pixel and brightest frame mean so far, linear.
    max_cll: f32,
    max_fall: f32,
}

/// A frame as pushed.
enum Frame<'a> {
    Sdr(&'a [u8]),
    Hdr(&'a [f32]),
}

impl<W: Write + Seek> Encoder<W> {
    pub fn new(out: W, mut config: EncoderConfig) -> Result<Encoder<W>> {
        let audio_configs = std::mem::take(&mut config.audio);
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
        if !(0.0..=100.0).contains(&config.plane_min_saving) {
            return Err(Error::Config("plane_min_saving must be between 0 and 100".into()));
        }
        if c.color.reference_white == 0 || !(c.color.tonemap_white.is_finite() && c.color.tonemap_white > 0.0) {
            return Err(Error::Config("reference white and tonemap white must be positive".into()));
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
            // BC1 is written opaque; HDR variants are opaque and linear.
            let flags = match format {
                f if f.is_hdr() => COLOR_FLAG_HDR,
                TextureFormat::Bc1 => color_flags & !COLOR_FLAG_ALPHA,
                _ => color_flags,
            };
            if format.is_hdr() && !c.color.hdr {
                return Err(Error::Config(format!("{format} needs an HDR color config (ColorConfig::hdr)")));
            }
            let mut params = VideoParams::new(c.width, c.height, format, flags);
            if format.is_hdr() {
                params.color.primaries = c.color.primaries;
                params.color.reference_white = c.color.reference_white;
            }
            if let VariantKind::Motion { chroma, .. } = v.kind {
                params.motion.chroma = chroma;
            }
            streams.push(StreamInfo::video(0, time_base, params));
            let encoder = match v.kind {
                VariantKind::Bc7 | VariantKind::Bc1 | VariantKind::Astc { .. } | VariantKind::Bc6h | VariantKind::AstcHdr { .. } => {
                    let (texture, quality) = texture_encoding(v.kind, v.quality);
                    let rdo_lambda = match v.kind {
                        VariantKind::Astc { rdo_lambda, .. } if rdo_lambda.is_finite() && rdo_lambda >= 0.0 => rdo_lambda,
                        VariantKind::Astc { rdo_lambda, .. } => {
                            return Err(Error::Config(format!("invalid RDO lambda {rdo_lambda}")));
                        }
                        _ => 0.0,
                    };
                    BlockEncoder::Texture { format: texture, options: imgio::EncodeOptions { quality, rdo_lambda } }
                }
                VariantKind::Motion { qp, slices, .. } => {
                    if qp > praxi_format::motion::MAX_QP || slices == 0 {
                        return Err(Error::Config(format!("motion qp {qp} or slices {slices} out of range")));
                    }
                    BlockEncoder::Motion(Box::new(MotionEncoder::new(&params, qp, slices, threads)))
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
        let preload_us = c.audio_preload.as_micros();
        if preload_us > u32::MAX as u128 {
            return Err(Error::Config("audio preload too long".into()));
        }
        let mut audio = Vec::new();
        let explicit: BTreeSet<u16> = audio_configs.iter().filter_map(|a| a.group).collect();
        let mut named_groups: Vec<(String, u16)> = Vec::new();
        for a in audio_configs {
            if a.sample_rate == 0 || a.channels == 0 {
                return Err(Error::Config(format!("audio track {:?} has zero sample rate or channels", a.name)));
            }
            if a.channel_mask != 0 && a.channel_mask.count_ones() != a.channels as u32 {
                return Err(Error::Config(format!("audio track {:?}: channel mask does not match {} channels", a.name, a.channels)));
            }
            let mut chunk_frames = (c.audio_chunk.as_micros() * a.sample_rate as u128 / 1_000_000) as usize;
            if chunk_frames == 0 {
                return Err(Error::Config("audio chunk is shorter than one sample frame".into()));
            }
            let group = match a.group {
                Some(g) => g,
                None => match named_groups.iter().find(|(n, _)| *n == a.name) {
                    Some((_, g)) => *g,
                    None => {
                        let used = |g: &u16| explicit.contains(g) || named_groups.iter().any(|(_, n)| n == g);
                        let g = (0..=u16::MAX).find(|g| !used(g)).ok_or_else(|| Error::Config("too many audio groups".into()))?;
                        named_groups.push((a.name.clone(), g));
                        g
                    }
                },
            };
            let params = AudioParams {
                sample_rate: a.sample_rate,
                channels: a.channels,
                format: a.format,
                channel_mask: a.channel_mask,
                preload_us: preload_us as u32,
                name: a.name.clone(),
            };
            let opus = match a.format {
                SampleFormat::Opus => {
                    let bitrate = a.bitrate.unwrap_or(opus::DEFAULT_BITRATE_PER_CHANNEL * a.channels as u32);
                    let e = opus::Encoder::new(&params, bitrate).map_err(|e| Error::Config(format!("audio track {:?}: {e}", a.name)))?;
                    // Chunks hold whole packets.
                    chunk_frames = chunk_frames.div_ceil(e.packet_frames()) * e.packet_frames();
                    Some(e)
                }
                _ => None,
            };
            audio.push(AudioTrack {
                stream: streams.len() as u16,
                channels: a.channels as usize,
                format: a.format,
                source: a.source,
                chunk_frames,
                next_pts: 0,
                done: false,
                input: Vec::new(),
                pcm: Vec::new(),
                adpcm: adpcm::Encoder::new(a.channels as usize),
                opus,
                payload: Vec::new(),
                stats: AudioStats { name: a.name, channels: a.channels, ..Default::default() },
            });
            streams.push(StreamInfo::audio(group, params));
        }
        let mut cues = Vec::new();
        let mut scratch = Vec::new();
        for cs in c.cue_streams.iter().filter(|s| !s.cues.is_empty()) {
            let stream = streams.len() as u16;
            streams.push(StreamInfo::cues(cs.name.clone()));
            for cc in &cs.cues {
                let micros = |secs: f64, what: &str| {
                    if secs.is_finite() && secs >= 0.0 {
                        Ok((secs * 1e6).round() as u64)
                    } else {
                        Err(Error::Config(format!("cue {:?} has invalid {what} {secs}", cc.kind)))
                    }
                };
                let cue = Cue { duration: micros(cc.duration, "duration")?, kind: cc.kind.clone(), fields: cc.fields.clone() };
                scratch.clear();
                cue.encode(&mut scratch)?;
                cues.push((micros(cc.time, "time")?, stream, cue));
            }
        }
        cues.sort_by_key(|(at, _, _)| *at);
        let mut writer = Writer::new(out, streams)?;
        writer.set_plane_min_saving(config.plane_min_saving);
        Ok(Encoder {
            writer,
            config,
            variants,
            audio,
            time_base,
            cues,
            next_cue: 0,
            frame: 0,
            threads,
            sdr_frame: Vec::new(),
            hdr_frame: Vec::new(),
            sdr_keys: Vec::new(),
            hdr_keys: Vec::new(),
            max_cll: 0.0,
            max_fall: 0.0,
        })
    }

    /// Encodes one frame: `width * height * 4` bytes of sRGB RGBA8, rows top to bottom. HDR
    /// variants get it as linear light (at most 1.0).
    pub fn push_frame(&mut self, rgba: &[u8]) -> Result<()> {
        self.push(Frame::Sdr(rgba))
    }

    /// Encodes one HDR frame: `width * height * 4` floats of linear RGBA in the configured
    /// primaries, 1.0 at reference white, straight alpha (`docs/features/hdr.md#library`).
    /// SDR variants get it tonemapped.
    pub fn push_frame_hdr(&mut self, rgba: &[f32]) -> Result<()> {
        self.push(Frame::Hdr(rgba))
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        let (w, h) = (self.config.width as usize, self.config.height as usize);
        let len = match frame {
            Frame::Sdr(f) => f.len(),
            Frame::Hdr(f) => f.len(),
        };
        if len != w * h * 4 {
            return Err(Error::Config(format!("frame has {len} values, expected {}", w * h * 4)));
        }
        let pts = self.frame;
        self.write_due(self.time_base.to_micros(pts))?;
        let color = self.config.color;
        let threads = self.threads;
        let hdr_variant = |v: &Variant| v.params.format.is_hdr();
        let needs_hdr = self.variants.iter().any(hdr_variant);
        let needs_sdr = self.variants.iter().any(|v| !hdr_variant(v));
        let needs_sdr_keys = self.variants.iter().any(|v| !hdr_variant(v) && v.params.format != TextureFormat::Motion);
        if needs_hdr {
            match frame {
                Frame::Sdr(rgba) => hdr::from_sdr(rgba, &color, threads, &mut self.hdr_frame),
                Frame::Hdr(rgba) => hdr::clamp_frame(rgba, threads, &mut self.hdr_frame),
            }
            hdr::skip_keys(&self.hdr_frame, color.reference_white as f32, threads, &mut self.hdr_keys);
            let (max, mean) = hdr::light_level(&self.hdr_frame);
            self.max_cll = self.max_cll.max(max);
            self.max_fall = self.max_fall.max(mean);
        }
        if needs_sdr && let Frame::Hdr(rgba) = frame {
            hdr::to_sdr(rgba, &color, threads, &mut self.sdr_frame);
        }
        let rgba: &[u8] = match frame {
            Frame::Sdr(rgba) => rgba,
            Frame::Hdr(_) => &self.sdr_frame,
        };
        if needs_sdr_keys {
            self.sdr_keys.clear();
            self.sdr_keys.extend(rgba.iter().map(|&v| v as u16));
        }
        let hdr_frame = &self.hdr_frame;
        let (sdr_keys, hdr_keys) = (&self.sdr_keys, &self.hdr_keys);

        let keyframe = pts.is_multiple_of(self.config.keyint as u64);
        for v in &mut self.variants {
            if let BlockEncoder::Motion(m) = &mut v.encoder {
                v.payload.clear();
                let keyframe = m.encode(rgba, w, h, keyframe, &mut v.payload);
                let before = self.writer.bytes_written();
                self.writer.write_chunk(v.stream, pts, keyframe, &v.payload, Some(Compression::None))?;
                v.stats.bytes += self.writer.bytes_written() - before;
                v.stats.frames += 1;
                v.stats.keyframes += keyframe as u64;
                v.stats.blocks += v.params.block_count() as u64;
                v.stats.skipped_blocks = m.stats.skip;
                v.stats.motion = Some(m.stats);
                continue;
            }
            let (bw, bh) = v.params.format.block_size();
            let is_hdr = v.params.format.is_hdr();
            gather_blocks(if is_hdr { hdr_keys } else { sdr_keys }, w, h, bw as usize, bh as usize, &mut v.src_blocks);
            match &v.encoder {
                BlockEncoder::Texture { format, options } if is_hdr => {
                    encode_texture(bytemuck::cast_slice(hdr_frame), imgio::Format::Rgba32Float, &v.params, *format, options, &mut v.encoded)?
                }
                BlockEncoder::Texture { format, options } => {
                    encode_texture(rgba, imgio::Format::Rgba8Srgb, &v.params, *format, options, &mut v.encoded)?
                }
                BlockEncoder::Motion(_) => unreachable!("handled above"),
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

    /// Writes the remaining cues and audio and the index, and returns the output and
    /// statistics.
    pub fn finish(mut self) -> Result<(W, Stats)> {
        self.write_due(u64::MAX)?;
        let white = self.config.color.reference_white;
        let (cll, fall) = (hdr::nits(self.max_cll, white), hdr::nits(self.max_fall, white));
        for v in self.variants.iter_mut().filter(|v| v.params.format.is_hdr()) {
            self.writer.set_light_level(v.stream, cll, fall)?;
            v.stats.light_level = Some((cll, fall));
        }
        let frames_end = self.time_base.to_micros(self.frame);
        let cues_end = self.cues.iter().map(|(at, _, c)| at.saturating_add(c.duration));
        let audio_end = self.audio.iter().map(|a| self.writer.streams()[a.stream as usize].time_base.to_micros(a.next_pts));
        let duration_us = cues_end.chain(audio_end).fold(frames_end, u64::max);
        let out = self.writer.finish(duration_us)?;
        let stats = Stats {
            frames: self.frame,
            duration_us,
            variants: self.variants.into_iter().map(|v| v.stats).collect(),
            audio: self.audio.into_iter().map(|a| a.stats).collect(),
        };
        Ok((out, stats))
    }

    /// Writes the cues and audio chunks due at or before `time_us`, in decode-time order
    /// with cues first on ties.
    fn write_due(&mut self, time_us: u64) -> Result<()> {
        loop {
            let cue_at = self.cues.get(self.next_cue).map(|(at, _, _)| *at);
            let audio_next = self
                .audio
                .iter()
                .enumerate()
                .filter(|(_, a)| !a.done)
                .map(|(i, a)| (self.writer.streams()[a.stream as usize].decode_time_us(a.next_pts), i))
                .min();
            match (cue_at, audio_next) {
                (Some(at), next) if at <= time_us && next.is_none_or(|(t, _)| at <= t) => {
                    let (at, stream, cue) = &self.cues[self.next_cue];
                    self.writer.write_cue(*stream, *at, cue)?;
                    self.next_cue += 1;
                }
                (_, Some((t, i))) if t <= time_us => self.write_audio_chunk(i)?,
                _ => return Ok(()),
            }
        }
    }

    /// Pulls one chunk of samples from an audio source and writes it.
    fn write_audio_chunk(&mut self, i: usize) -> Result<()> {
        let a = &mut self.audio[i];
        let frame_in = a.channels * 4;
        a.input.resize(a.chunk_frames * frame_in, 0);
        let got = read_full(&mut a.source, &mut a.input)
            .map_err(|e| Error::Encode(format!("audio track {:?}: {e}", a.stats.name)))?;
        let frames = got / frame_in;
        if frames < a.chunk_frames {
            a.done = true;
        }
        if let Some(enc) = &mut a.opus {
            let samples: Vec<f32> = a.input[..frames * frame_in].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            let chunk = enc.chunk(&samples, a.done, &mut a.payload).map_err(|e| Error::Encode(format!("audio track {:?}: {e}", a.stats.name)))?;
            // At the end, the frame after the source's last, which ends the track.
            a.next_pts = if a.done { enc.source_frames() } else { enc.next_pts() };
            if let Some((pts, kept)) = chunk {
                let before = self.writer.bytes_written();
                self.writer.write_audio(a.stream, pts, &a.payload)?;
                a.stats.bytes += self.writer.bytes_written() - before;
                a.stats.frames += kept;
                a.stats.chunks += 1;
            }
            return Ok(());
        }
        if frames == 0 {
            return Ok(());
        }
        let input = &a.input[..frames * frame_in];
        a.payload.clear();
        let to_s16 = |b: &[u8; 4]| (f32::from_le_bytes(*b) * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        match a.format {
            SampleFormat::F32 => a.payload.extend_from_slice(input),
            SampleFormat::S16 => {
                for b in input.as_chunks::<4>().0 {
                    a.payload.extend_from_slice(&to_s16(b).to_le_bytes());
                }
            }
            SampleFormat::Adpcm => {
                a.pcm.clear();
                a.pcm.extend(input.as_chunks::<4>().0.iter().map(to_s16));
                a.adpcm.encode(&a.pcm, &mut a.payload);
            }
            SampleFormat::Opus => unreachable!("encoded above"),
        }
        let before = self.writer.bytes_written();
        self.writer.write_audio(a.stream, a.next_pts, &a.payload)?;
        a.stats.bytes += self.writer.bytes_written() - before;
        a.stats.frames += frames as u64;
        a.stats.chunks += 1;
        a.next_pts += frames as u64;
        Ok(())
    }
}

/// Reads until `buf` is full or the reader ends; returns the bytes read.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Splits an RGBA image into blocks of `bw * bh` pixels (row-major blocks, row-major pixels
/// inside a block), repeating the last column and row to pad partial blocks.
fn gather_blocks(rgba: &[u16], w: usize, h: usize, bw: usize, bh: usize, out: &mut Vec<u16>) {
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

/// The `imgio` format and quality of a block variant (`docs/features/video-codec.md#encoding-paths`).
fn texture_encoding(kind: VariantKind, quality: Quality) -> (imgio::Format, imgio::Quality) {
    use imgio::Quality as Q;
    let q = |fast, medium, thorough| match quality {
        Quality::Fast => fast,
        Quality::Medium => medium,
        Quality::Thorough => thorough,
    };
    match kind {
        // BC7: imgio's bc7f (fastest, partially analytical) and bc7e (slow).
        VariantKind::Bc7 => (imgio::Format::Bc7Srgb, q(Q::Fastest, Q::Default, Q::High)),
        // BC1: basisu encode_bc1 with 1, 2 or 3 least-squares passes.
        VariantKind::Bc1 => (imgio::Format::Bc1Srgb, q(Q::Fast, Q::Default, Q::High)),
        // ASTC: astcenc presets fast, medium and thorough.
        VariantKind::Astc { block, .. } => {
            let b = imgio::AstcBlock::from_dim(block as u32, block as u32).expect("square ASTC block size");
            (imgio::Format::Astc(b, imgio::AstcKind::Srgb), q(Q::Fast, Q::Default, Q::High))
        }
        // BC6H: basisu fast_encode_bc6h; ASTC HDR: astcenc presets fastest, fast and medium.
        VariantKind::Bc6h => (imgio::Format::Bc6hUfloat, q(Q::Default, Q::High, Q::Best)),
        VariantKind::AstcHdr { block } => {
            let b = imgio::AstcBlock::from_dim(block as u32, block as u32).expect("square ASTC block size");
            (imgio::Format::Astc(b, imgio::AstcKind::Float), q(Q::Fastest, Q::Fast, Q::Default))
        }
        VariantKind::Motion { .. } => unreachable!("motion is not a block format"),
    }
}

/// Encodes a `width * height` frame of `pixels` (RGBA8 sRGB or RGBA32 float) into `out`
/// (`params.frame_size()` bytes). Partial blocks repeat the last column and row.
fn encode_texture(
    frame: &[u8],
    pixels: imgio::Format,
    params: &VideoParams,
    format: imgio::Format,
    options: &imgio::EncodeOptions,
    out: &mut Vec<u8>,
) -> Result<()> {
    let fail = |e: imgio::Error| Error::Encode(format!("{format:?}: {e}"));
    let src = imgio::Surface::new(pixels, params.width, params.height, frame).map_err(fail)?;
    out.resize(params.frame_size(), 0);
    imgio::encode(&src, format, options, out).map_err(fail)
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
        } else if src.iter().zip(ref_src.iter()).all(|(a, b)| a.abs_diff(*b) <= threshold as u16) {
            // Close enough to what the decoder already shows.
        } else {
            v.payload[i / 8] |= 1 << (i % 8);
            v.payload.extend(enc.iter().zip(ref_enc.iter()).map(|(a, b)| a ^ b));
            ref_src.copy_from_slice(src);
            ref_enc.copy_from_slice(enc);
            continue;
        }
        v.stats.skipped_blocks += 1;
    }
}

