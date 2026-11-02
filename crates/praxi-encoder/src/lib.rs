//! Encoder for praxi video files (`docs/features/encoder.md`, `docs/features/video-codec.md`,
//! `docs/features/audio.md`).

mod decode;
mod motion;

pub use decode::decode_frame;
pub use motion::MotionStats;

use ktx::astcenc::{
    self, AstcencContext, AstcencImage, AstcencImageData, AstcencOutput, AstcencProfile,
    AstcencSwizzle,
};
use ktx::basisu::{uastc_bc7, uastc_enc};
use praxi_format::{
    AudioParams, COLOR_FLAG_ALPHA, COLOR_FLAG_SRGB, Compression, Cue, StreamInfo, TextureFormat,
    TimeBase, VideoParams, Writer, adpcm,
};

use motion::MotionEncoder;
pub use praxi_format::Chroma;

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
    /// UASTC transcoded to BC7, with optional UASTC RDO (`rdo_lambda > 0`).
    Bc7 { rdo_lambda: f32 },
    /// ASTC with square blocks of 4, 6 or 8 pixels.
    Astc { block: u8 },
    /// The motion codec (`docs/features/motion-codec.md`).
    Motion { qp: u8, chroma: Chroma, slices: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VariantConfig {
    pub kind: VariantKind,
    pub quality: Quality,
}

impl VariantConfig {
    pub fn bc7() -> VariantConfig {
        VariantConfig {
            kind: VariantKind::Bc7 { rdo_lambda: 0.0 },
            quality: Quality::Fast,
        }
    }

    pub fn astc(block: u8) -> VariantConfig {
        VariantConfig {
            kind: VariantKind::Astc { block },
            quality: Quality::Fast,
        }
    }

    /// The motion codec with `qp` 22, 4:2:0 chroma and 4 slices.
    pub fn motion() -> VariantConfig {
        VariantConfig {
            kind: VariantKind::Motion {
                qp: 22,
                chroma: Chroma::C420,
                slices: 4,
            },
            quality: Quality::Fast,
        }
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
            VariantKind::Motion { .. } => Some(TextureFormat::Motion),
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
        CueConfig {
            time,
            duration: 0.0,
            kind: kind.into(),
            fields: Vec::new(),
        }
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
        CueStreamConfig {
            name: name.into(),
            cues: Vec::new(),
        }
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
    /// `None` shares the group of earlier tracks with the same name, or takes a new one.
    pub group: Option<u16>,
    source: Box<dyn Read + Send>,
}

impl AudioTrackConfig {
    pub fn from_samples(
        name: impl Into<String>,
        sample_rate: u32,
        channels: u16,
        samples: Vec<f32>,
    ) -> AudioTrackConfig {
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
}

impl fmt::Debug for AudioTrackConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioTrackConfig")
            .field("name", &self.name)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("channel_mask", &self.channel_mask)
            .field("format", &self.format)
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
    /// Worker threads; 0 uses `std::thread::available_parallelism`.
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
    payload: Vec<u8>,
    stats: AudioStats,
}

enum BlockEncoder {
    Bc7 { level: u32, rdo_lambda: f32 },
    Astc { ctx: Box<AstcencContext> },
    Motion(Box<MotionEncoder>),
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
    audio: Vec<AudioTrack>,
    time_base: TimeBase,
    /// Cues as (start in microseconds, stream, cue), sorted by start.
    cues: Vec<(u64, u16, Cue)>,
    next_cue: usize,
    frame: u64,
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
            return Err(Error::Config(
                "plane_min_saving must be between 0 and 100".into(),
            ));
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
            let mut params = VideoParams::new(c.width, c.height, format, color_flags);
            if let VariantKind::Motion { chroma, .. } = v.kind {
                params.motion.chroma = chroma;
            }
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
                    let cfg =
                        astcenc::astcenc_config_init(AstcencProfile::LdrSrgb, b, b, 1, preset, 0)
                            .map_err(|e| Error::Encode(format!("astcenc config: {e:?}")))?;
                    let ctx = astcenc::astcenc_context_alloc(&cfg, threads as u32)
                        .map_err(|e| Error::Encode(format!("astcenc context: {e:?}")))?;
                    BlockEncoder::Astc { ctx }
                }
                VariantKind::Motion { qp, slices, .. } => {
                    if qp > praxi_format::motion::MAX_QP || slices == 0 {
                        return Err(Error::Config(format!(
                            "motion qp {qp} or slices {slices} out of range"
                        )));
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
                stats: VariantStats {
                    format: Some(format),
                    ..Default::default()
                },
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
                return Err(Error::Config(format!(
                    "audio track {:?} has zero sample rate or channels",
                    a.name
                )));
            }
            if a.channel_mask != 0 && a.channel_mask.count_ones() != a.channels as u32 {
                return Err(Error::Config(format!(
                    "audio track {:?}: channel mask does not match {} channels",
                    a.name, a.channels
                )));
            }
            let chunk_frames =
                (c.audio_chunk.as_micros() * a.sample_rate as u128 / 1_000_000) as usize;
            if chunk_frames == 0 {
                return Err(Error::Config(
                    "audio chunk is shorter than one sample frame".into(),
                ));
            }
            let group = match a.group {
                Some(g) => g,
                None => match named_groups.iter().find(|(n, _)| *n == a.name) {
                    Some((_, g)) => *g,
                    None => {
                        let used = |g: &u16| {
                            explicit.contains(g) || named_groups.iter().any(|(_, n)| n == g)
                        };
                        let g = (0..=u16::MAX)
                            .find(|g| !used(g))
                            .ok_or_else(|| Error::Config("too many audio groups".into()))?;
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
                payload: Vec::new(),
                stats: AudioStats {
                    name: a.name,
                    channels: a.channels,
                    ..Default::default()
                },
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
                        Err(Error::Config(format!(
                            "cue {:?} has invalid {what} {secs}",
                            cc.kind
                        )))
                    }
                };
                let cue = Cue {
                    duration: micros(cc.duration, "duration")?,
                    kind: cc.kind.clone(),
                    fields: cc.fields.clone(),
                };
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
            threads,
            variants,
            audio,
            time_base,
            cues,
            next_cue: 0,
            frame: 0,
        })
    }

    /// Encodes one frame: `width * height * 4` bytes of sRGB RGBA8, rows top to bottom.
    pub fn push_frame(&mut self, rgba: &[u8]) -> Result<()> {
        let (w, h) = (self.config.width as usize, self.config.height as usize);
        if rgba.len() != w * h * 4 {
            return Err(Error::Config(format!(
                "frame has {} bytes, expected {}",
                rgba.len(),
                w * h * 4
            )));
        }
        let pts = self.frame;
        self.write_due(self.time_base.to_micros(pts))?;

        let keyframe = pts.is_multiple_of(self.config.keyint as u64);
        for v in &mut self.variants {
            if let BlockEncoder::Motion(m) = &mut v.encoder {
                v.payload.clear();
                let keyframe = m.encode(rgba, w, h, keyframe, &mut v.payload);
                let before = self.writer.bytes_written();
                self.writer.write_chunk(
                    v.stream,
                    pts,
                    keyframe,
                    &v.payload,
                    Some(Compression::None),
                )?;
                v.stats.bytes += self.writer.bytes_written() - before;
                v.stats.frames += 1;
                v.stats.keyframes += keyframe as u64;
                v.stats.blocks += v.params.block_count() as u64;
                v.stats.skipped_blocks = m.stats.skip;
                v.stats.motion = Some(m.stats);
                continue;
            }
            let (bw, bh) = v.params.format.block_size();
            gather_blocks(rgba, w, h, bw as usize, bh as usize, &mut v.src_blocks);
            match &mut v.encoder {
                BlockEncoder::Bc7 { level, rdo_lambda } => encode_bc7(
                    &v.src_blocks,
                    *level,
                    *rdo_lambda,
                    self.threads,
                    &mut v.encoded,
                ),
                BlockEncoder::Astc { ctx } => {
                    encode_astc(ctx, rgba, w, h, &v.params, self.threads, &mut v.encoded)?
                }
                BlockEncoder::Motion(_) => unreachable!("handled above"),
            }
            build_payload(v, keyframe, self.config.skip_threshold);
            let before = self.writer.bytes_written();
            self.writer
                .write_chunk(v.stream, pts, keyframe, &v.payload, None)?;
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
        let frames_end = self.time_base.to_micros(self.frame);
        let cues_end = self
            .cues
            .iter()
            .map(|(at, _, c)| at.saturating_add(c.duration));
        let audio_end = self.audio.iter().map(|a| {
            self.writer.streams()[a.stream as usize]
                .time_base
                .to_micros(a.next_pts)
        });
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
                .map(|(i, a)| {
                    (
                        self.writer.streams()[a.stream as usize].decode_time_us(a.next_pts),
                        i,
                    )
                })
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
        if frames == 0 {
            return Ok(());
        }
        let input = &a.input[..frames * frame_in];
        a.payload.clear();
        let to_s16 = |b: &[u8; 4]| {
            (f32::from_le_bytes(*b) * 32768.0)
                .round()
                .clamp(-32768.0, 32767.0) as i16
        };
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
        let pixels: Vec<[u8; 4]> = src_blocks.as_chunks::<4>().0.to_vec();
        let params = uastc_enc::UastcRdoParams {
            lambda: rdo_lambda,
            ..Default::default()
        };
        uastc_enc::uastc_rdo(
            n as u32,
            &mut uastc,
            &pixels,
            &params,
            flags,
            threads as u32,
        );
    }
    out.resize(n * 16, 0);
    for (dst, src) in out.as_chunks_mut::<16>().0.iter_mut().zip(&uastc) {
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
                    s.spawn(move || {
                        astcenc::astcenc_compress_image(
                            ctx,
                            image,
                            &AstcencSwizzle::RGBA,
                            work,
                            t as u32,
                        )
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or(Err(astcenc::AstcencError::BadContext)))
                .collect::<Vec<_>>()
        })
    };
    if let Some(Err(e)) = result.into_iter().find(|r| r.is_err()) {
        return Err(Error::Encode(format!("astcenc: {e:?}")));
    }
    astcenc::astcenc_compress_reset(ctx)
        .map_err(|e| Error::Encode(format!("astcenc reset: {e:?}")))?;
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
        } else if src
            .iter()
            .zip(ref_src.iter())
            .all(|(a, b)| a.abs_diff(*b) <= threshold)
        {
            // Close enough to what the decoder already shows.
        } else {
            v.payload[i / 8] |= 1 << (i % 8);
            v.payload
                .extend(enc.iter().zip(ref_enc.iter()).map(|(a, b)| a ^ b));
            ref_src.copy_from_slice(src);
            ref_enc.copy_from_slice(enc);
            continue;
        }
        v.stats.skipped_blocks += 1;
    }
}
