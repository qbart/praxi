//! Reader and writer for the PRAXI container format (`docs/spec/PRAXI.md`).

mod cue;
mod planes;
mod reader;
mod writer;

pub use cue::{Cue, Value};
pub use planes::{from_planes, to_planes};
pub use reader::Reader;
pub use writer::{DEFAULT_PLANE_MIN_SAVING, Writer};

use std::fmt;

/// `\x89PRX\r\n\x1a\n`
pub const MAGIC: [u8; 8] = [0x89, b'P', b'R', b'X', 0x0D, 0x0A, 0x1A, 0x0A];
pub const VERSION_MAJOR: u16 = 0;
pub const VERSION_MINOR: u16 = 5;

pub const HEADER_SIZE: u64 = 48;
pub const STREAM_ENTRY_SIZE: u64 = 64;
pub const CHUNK_HEADER_SIZE: u64 = 24;
pub const INDEX_ENTRY_SIZE: u64 = 24;

/// Chunk flag bit 0.
pub const CHUNK_FLAG_KEYFRAME: u8 = 1;

/// Video `color_flags` bit 0.
pub const COLOR_FLAG_SRGB: u16 = 1;
/// Video `color_flags` bit 1.
pub const COLOR_FLAG_ALPHA: u16 = 2;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    BadMagic,
    UnsupportedVersion { major: u16, minor: u16 },
    Corrupt(String),
    /// The writer was used against the spec (bad stream table, out-of-order chunks...).
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "i/o error: {e}"),
            Error::BadMagic => write!(f, "not a praxi file"),
            Error::UnsupportedVersion { major, minor } => write!(f, "unsupported format version {major}.{minor}"),
            Error::Corrupt(msg) => write!(f, "corrupt praxi file: {msg}"),
            Error::Invalid(msg) => write!(f, "invalid praxi output: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn corrupt<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Corrupt(msg.into()))
}

/// GPU texture formats a video stream can be stored in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Bc1,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
    Astc4x4,
    Astc5x5,
    Astc6x6,
    Astc8x8,
}

impl TextureFormat {
    pub const ALL: [TextureFormat; 9] = [
        TextureFormat::Bc1,
        TextureFormat::Bc3,
        TextureFormat::Bc4,
        TextureFormat::Bc5,
        TextureFormat::Bc7,
        TextureFormat::Astc4x4,
        TextureFormat::Astc5x5,
        TextureFormat::Astc6x6,
        TextureFormat::Astc8x8,
    ];

    pub fn id(self) -> u16 {
        match self {
            TextureFormat::Bc1 => 0x0001,
            TextureFormat::Bc3 => 0x0002,
            TextureFormat::Bc4 => 0x0003,
            TextureFormat::Bc5 => 0x0004,
            TextureFormat::Bc7 => 0x0007,
            TextureFormat::Astc4x4 => 0x0101,
            TextureFormat::Astc5x5 => 0x0102,
            TextureFormat::Astc6x6 => 0x0103,
            TextureFormat::Astc8x8 => 0x0104,
        }
    }

    pub fn from_id(id: u16) -> Option<TextureFormat> {
        Self::ALL.into_iter().find(|f| f.id() == id)
    }

    /// Block width and height in pixels.
    pub fn block_size(self) -> (u32, u32) {
        match self {
            TextureFormat::Astc5x5 => (5, 5),
            TextureFormat::Astc6x6 => (6, 6),
            TextureFormat::Astc8x8 => (8, 8),
            _ => (4, 4),
        }
    }

    pub fn block_bytes(self) -> u32 {
        match self {
            TextureFormat::Bc1 | TextureFormat::Bc4 => 8,
            _ => 16,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TextureFormat::Bc1 => "BC1",
            TextureFormat::Bc3 => "BC3",
            TextureFormat::Bc4 => "BC4",
            TextureFormat::Bc5 => "BC5",
            TextureFormat::Bc7 => "BC7",
            TextureFormat::Astc4x4 => "ASTC 4x4",
            TextureFormat::Astc5x5 => "ASTC 5x5",
            TextureFormat::Astc6x6 => "ASTC 6x6",
            TextureFormat::Astc8x8 => "ASTC 8x8",
        }
    }
}

impl fmt::Display for TextureFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Parameters of a video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoParams {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub color_flags: u16,
}

impl VideoParams {
    pub fn blocks_x(&self) -> u32 {
        self.width.div_ceil(self.format.block_size().0)
    }

    pub fn blocks_y(&self) -> u32 {
        self.height.div_ceil(self.format.block_size().1)
    }

    pub fn block_count(&self) -> usize {
        self.blocks_x() as usize * self.blocks_y() as usize
    }

    pub fn frame_size(&self) -> usize {
        self.block_count() * self.format.block_bytes() as usize
    }

    /// Size of the block mask that starts a delta frame payload.
    pub fn mask_size(&self) -> usize {
        self.block_count().div_ceil(8)
    }

    /// Bytes before the blocks of a payload, which the byte-plane form leaves unchanged:
    /// none for keyframes, the mask for delta frames.
    pub fn plane_prefix(&self, keyframe: bool) -> usize {
        if keyframe { 0 } else { self.mask_size() }
    }

    pub fn srgb(&self) -> bool {
        self.color_flags & COLOR_FLAG_SRGB != 0
    }

    pub fn alpha(&self) -> bool {
        self.color_flags & COLOR_FLAG_ALPHA != 0
    }
}

/// Parameters of a cue stream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CueParams {
    /// Largest cue duration in the stream's time base. Filled in by the writer.
    pub max_duration: u64,
    /// At most 32 bytes.
    pub name: String,
}

pub const CUE_NAME_SIZE: usize = 32;

/// Sample formats of audio streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    S16,
    F32,
}

impl SampleFormat {
    pub fn id(self) -> u16 {
        match self {
            SampleFormat::S16 => 1,
            SampleFormat::F32 => 2,
        }
    }

    pub fn from_id(id: u16) -> Option<SampleFormat> {
        match id {
            1 => Some(SampleFormat::S16),
            2 => Some(SampleFormat::F32),
            _ => None,
        }
    }

    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::S16 => 2,
            SampleFormat::F32 => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SampleFormat::S16 => "s16",
            SampleFormat::F32 => "f32",
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Parameters of an audio stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioParams {
    pub sample_rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
    /// WAVE speaker bits, `0` for unspecified.
    pub channel_mask: u32,
    /// How far ahead of their time chunks are stored.
    pub preload_us: u32,
    /// At most 32 bytes.
    pub name: String,
}

impl AudioParams {
    /// Bytes per sample frame (all channels).
    pub fn frame_bytes(&self) -> usize {
        self.channels as usize * self.format.bytes()
    }
}

pub const AUDIO_NAME_SIZE: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamParams {
    Video(VideoParams),
    Audio(AudioParams),
    Cues(CueParams),
    /// A stream of a kind this reader does not know; its chunks are skipped.
    Unknown(u8),
}

impl StreamParams {
    pub fn kind(&self) -> u8 {
        match self {
            StreamParams::Video(_) => 1,
            StreamParams::Audio(_) => 2,
            StreamParams::Cues(_) => 3,
            StreamParams::Unknown(kind) => *kind,
        }
    }
}

/// A rational time base: a pts of `p` is `p * num / den` seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeBase {
    pub num: u32,
    pub den: u32,
}

impl TimeBase {
    pub const MICROS: TimeBase = TimeBase { num: 1, den: 1_000_000 };

    pub fn new(num: u32, den: u32) -> TimeBase {
        TimeBase { num, den }
    }

    /// Converts a pts to microseconds, rounding down.
    pub fn to_micros(self, pts: u64) -> u64 {
        (pts as u128 * self.num as u128 * 1_000_000 / self.den as u128) as u64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub group: u16,
    pub time_base: TimeBase,
    /// Filled in by the writer; read from the file by the reader.
    pub chunk_count: u32,
    pub params: StreamParams,
}

impl StreamInfo {
    pub fn video(group: u16, time_base: TimeBase, params: VideoParams) -> StreamInfo {
        StreamInfo { group, time_base, chunk_count: 0, params: StreamParams::Video(params) }
    }

    /// An audio stream with a time base of `1 / sample_rate`.
    pub fn audio(group: u16, params: AudioParams) -> StreamInfo {
        StreamInfo {
            group,
            time_base: TimeBase::new(1, params.sample_rate),
            chunk_count: 0,
            params: StreamParams::Audio(params),
        }
    }

    /// A cue stream with a microsecond time base.
    pub fn cues(name: impl Into<String>) -> StreamInfo {
        StreamInfo {
            group: 0,
            time_base: TimeBase::MICROS,
            chunk_count: 0,
            params: StreamParams::Cues(CueParams { max_duration: 0, name: name.into() }),
        }
    }

    pub fn video_params(&self) -> Option<&VideoParams> {
        match &self.params {
            StreamParams::Video(v) => Some(v),
            _ => None,
        }
    }

    pub fn cue_params(&self) -> Option<&CueParams> {
        match &self.params {
            StreamParams::Cues(c) => Some(c),
            _ => None,
        }
    }

    pub fn audio_params(&self) -> Option<&AudioParams> {
        match &self.params {
            StreamParams::Audio(a) => Some(a),
            _ => None,
        }
    }

    /// The time in microseconds at which a chunk with `pts` is due: its time, less the
    /// preload for audio (`docs/spec/PRAXI.md#ordering`).
    pub fn decode_time_us(&self, pts: u64) -> u64 {
        let time = self.time_base.to_micros(pts);
        match &self.params {
            StreamParams::Audio(a) => time.saturating_sub(a.preload_us as u64),
            _ => time,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub version_major: u16,
    pub version_minor: u16,
    pub duration_us: u64,
    pub index_offset: u64,
    pub index_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Lz4,
    /// LZ4 over the byte-plane form of a video payload.
    Lz4Planes,
}

impl Compression {
    pub fn id(self) -> u8 {
        match self {
            Compression::None => 0,
            Compression::Lz4 => 1,
            Compression::Lz4Planes => 2,
        }
    }

    pub fn from_id(id: u8) -> Option<Compression> {
        match id {
            0 => Some(Compression::None),
            1 => Some(Compression::Lz4),
            2 => Some(Compression::Lz4Planes),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHeader {
    /// Offset of the chunk header in the file.
    pub offset: u64,
    pub stream: u16,
    pub flags: u8,
    pub compression: Compression,
    pub stored_size: u32,
    pub raw_size: u32,
    pub pts: u64,
}

impl ChunkHeader {
    pub fn keyframe(&self) -> bool {
        self.flags & CHUNK_FLAG_KEYFRAME != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexEntry {
    pub stream: u16,
    pub pts: u64,
    pub offset: u64,
}

pub(crate) fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

pub(crate) fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

pub(crate) fn le_u64(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}
