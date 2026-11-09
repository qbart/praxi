//! Video player for games (`docs/features/player.md`).
//!
//! Frames are GPU block-compressed textures; the player reconstructs them from keyframes and
//! delta frames and hands the raw blocks to the engine for upload. Audio is queued for the
//! engine's mixer (`docs/features/audio.md`).

mod audio;

use audio::us_to_frames;
use praxi_format::motion::{CodedFrame, Decoder, MAX_AHEAD};
use praxi_format::{AudioDecoder, CHUNK_HEADER_SIZE, ChunkHeader, Cue, IndexEntry, Reader, StreamInfo, StreamParams, TimeBase};
use std::cmp::Reverse;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io::{Read, Seek};
use std::time::Duration;

pub use audio::AudioOutput;
pub use praxi_format::{self as format, AudioParams, CueTrack, SampleFormat, TextureFormat, TimedCue, Value, VideoParams};

/// Default for [`Player::set_audio_sync_tolerance`].
pub const DEFAULT_AUDIO_SYNC_TOLERANCE: Duration = Duration::from_millis(60);

/// Time over which `Clock::Audio` corrects the clock toward the heard position, in
/// microseconds: each `advance(dt)` removes `dt / 500 ms` of the error.
const AUDIO_CLOCK_SMOOTHING_US: u64 = 500_000;

/// What drives the player clock (`docs/features/audio.md#audio-master-clock`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Clock {
    /// The clock advances by the `dt` given to `advance`; audio follows it.
    #[default]
    Game,
    /// `advance(dt)` steers the clock toward what audio track `track` has played: its read
    /// position minus the engine's output `latency`.
    Audio { track: usize, latency: Duration },
}

#[derive(Debug)]
pub enum Error {
    Format(praxi_format::Error),
    /// The group has no variant in a texture format the engine supports.
    NoSupportedVariant { group: u16 },
    Corrupt(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Format(e) => e.fmt(f),
            Error::NoSupportedVariant { group } => write!(f, "group {group} has no variant in a supported texture format"),
            Error::Corrupt(msg) => write!(f, "corrupt praxi file: {msg}"),
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

/// What the engine supports: texture formats its GPU can sample, most preferred first, and
/// the channel count of its audio output.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    formats: Vec<TextureFormat>,
    max_audio_channels: Option<u16>,
}

impl Capabilities {
    pub fn new(formats: impl IntoIterator<Item = TextureFormat>) -> Capabilities {
        Capabilities { formats: formats.into_iter().collect(), max_audio_channels: None }
    }

    /// Prefers audio variants with at most `channels` channels (default: unlimited).
    pub fn max_audio_channels(mut self, channels: u16) -> Capabilities {
        self.max_audio_channels = Some(channels);
        self
    }

    pub fn formats(&self) -> &[TextureFormat] {
        &self.formats
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Started,
    Paused,
    Resumed,
    Seeked { to: Duration },
    /// The clock reached the start of a cue (`docs/features/cues.md`).
    Cue(CueEvent),
    /// A cue with a non-zero duration ended.
    CueEnd { id: u64, stream: u16 },
    Looped,
    Finished,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CueEvent {
    /// Identifies the cue within the file; matches the `id` of its `CueEnd`.
    pub id: u64,
    pub stream: u16,
    pub time: Duration,
    /// Zero for instantaneous cues.
    pub duration: Duration,
    /// The cue type.
    pub kind: String,
    pub fields: Vec<(String, Value)>,
}

impl CueEvent {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The cue's position on the video, `(x, y)` in `0..=1` from the top left, if it has one
    /// (`docs/features/cues.md#screen-position`).
    pub fn position(&self) -> Option<(f32, f32)> {
        praxi_format::cue_position(&self.fields)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueStreamInfo {
    pub stream: u16,
    pub name: String,
}

/// Id of the first cue given to [`Player::replace_cues`].
const REPLACED_CUE_ID: u64 = 1 << 63;

/// A cue given to [`Player::replace_cues`].
struct ReplacedCue {
    start_us: u64,
    end_us: u64,
    stream: u16,
    cue: Cue,
}

/// A started cue with a non-zero duration.
struct ActiveCue {
    end_us: u64,
    id: u64,
    stream: u16,
}

/// The selected variant of one group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackInfo {
    pub stream: u16,
    pub group: u16,
    pub params: VideoParams,
}

/// The selected variant of one audio group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrackInfo {
    pub stream: u16,
    pub group: u16,
    pub params: AudioParams,
}

struct AudioTrack {
    info: AudioTrackInfo,
    output: AudioOutput,
    enabled: bool,
    /// Output frame of pts 0 in the current pass; grows by the duration on every loop.
    base_frame: u64,
    /// Chunks before this offset are ignored (set by seeking).
    resume_offset: u64,
    decoder: AudioDecoder,
}

impl AudioTrack {
    fn frames(&self, us: u64) -> u64 {
        us_to_frames(us, self.info.params.sample_rate)
    }
}

/// What the player does with the chunks of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Skip,
    Video(usize),
    Audio(usize),
    Cues,
}

/// The latest decoded frame of a track.
#[derive(Debug, Clone, Copy)]
pub struct VideoFrame<'a> {
    pub format: TextureFormat,
    pub srgb: bool,
    pub alpha: bool,
    pub width: u32,
    pub height: u32,
    /// Blocks in row-major order, ready to upload as a compressed texture. Empty for
    /// `motion` tracks, whose frames come from [`Player::take_coded_frames`].
    pub data: &'a [u8],
    /// Changes whenever a new frame is decoded.
    pub index: u64,
    pub time: Duration,
}

struct Track {
    info: TrackInfo,
    time_base: TimeBase,
    frame: Vec<u8>,
    has_frame: bool,
    index: u64,
    pts: u64,
    /// Chunks before this offset are ignored (set by seeking).
    resume_offset: u64,
    /// Entropy decoding of `motion` tracks.
    motion: Option<MotionQueue>,
}

/// The coded frames of a `motion` track waiting for the GPU decoder.
struct MotionQueue {
    decoder: Decoder,
    /// Recycled buffers; the first `queued` hold frames not taken yet, oldest first.
    frames: Vec<CodedFrame>,
    queued: usize,
    /// Chunk offsets of the frames the decoder is decoding ahead, in decoding order.
    ahead: VecDeque<u64>,
}

impl MotionQueue {
    /// The buffer for the next decoded frame.
    fn next(&mut self, keyframe: bool) -> &mut CodedFrame {
        // Frames before a keyframe are not needed to reconstruct it.
        if keyframe {
            self.queued = 0;
        }
        if self.frames.len() == self.queued {
            self.frames.push(CodedFrame::default());
        }
        &mut self.frames[self.queued]
    }

    fn push(&mut self, keyframe: bool, payload: &[u8]) -> std::result::Result<(), String> {
        // Decoding cancels the decoder's decode-ahead.
        self.ahead.clear();
        self.next(keyframe);
        self.decoder.decode(keyframe, payload, &mut self.frames[self.queued])?;
        self.queued += 1;
        Ok(())
    }

    /// Collects the frame of `chunk` if it is the first frame decoded ahead.
    fn finish_ahead(&mut self, chunk: &ChunkHeader) -> Option<std::result::Result<(), String>> {
        if self.ahead.front() != Some(&chunk.offset) {
            return None;
        }
        self.ahead.pop_front();
        self.next(chunk.keyframe());
        let result = self.decoder.finish_ahead(chunk.offset, &mut self.frames[self.queued])?;
        if result.is_err() {
            // The decoder cancels the frames after a broken one.
            self.ahead.clear();
        }
        self.queued += 1;
        Some(result)
    }
}

impl Track {
    /// Applies the motion frame `chunk` if it was decoded ahead; `false` when it was not,
    /// and the payload has to be read and applied.
    fn apply_ahead(&mut self, chunk: &ChunkHeader, index: u64) -> Result<bool> {
        let Some(result) = self.motion.as_mut().and_then(|m| m.finish_ahead(chunk)) else {
            return Ok(false);
        };
        if let Err(e) = result {
            return corrupt(format!("motion frame at {}: {e}", chunk.offset));
        }
        self.applied(chunk, index);
        Ok(true)
    }

    fn applied(&mut self, chunk: &ChunkHeader, index: u64) {
        self.has_frame = true;
        self.index = index;
        self.pts = chunk.pts;
    }

    fn apply(&mut self, chunk: &ChunkHeader, payload: &[u8], index: u64) -> Result<()> {
        let p = &self.info.params;
        if let Some(m) = &mut self.motion {
            if let Err(e) = m.push(chunk.keyframe(), payload) {
                return corrupt(format!("motion frame at {}: {e}", chunk.offset));
            }
        } else if chunk.keyframe() {
            if payload.len() != p.frame_size() {
                return corrupt(format!("keyframe at {} has {} bytes, expected {}", chunk.offset, payload.len(), p.frame_size()));
            }
            self.frame.copy_from_slice(payload);
        } else {
            if !self.has_frame {
                return corrupt(format!("delta frame at {} without a keyframe", chunk.offset));
            }
            apply_delta(&mut self.frame, payload, p, chunk.offset)?;
        }
        self.applied(chunk, index);
        Ok(())
    }
}

fn apply_delta(frame: &mut [u8], payload: &[u8], p: &VideoParams, offset: u64) -> Result<()> {
    let mask_size = p.mask_size();
    let bb = p.format.block_bytes() as usize;
    if payload.len() < mask_size {
        return corrupt(format!("delta frame at {offset} is shorter than its mask"));
    }
    let (mask, blocks) = payload.split_at(mask_size);
    let changed: usize = mask.iter().map(|b| b.count_ones() as usize).sum();
    if blocks.len() != changed * bb {
        return corrupt(format!("delta frame at {offset} has {} block bytes for {changed} blocks", blocks.len()));
    }
    let block_count = p.block_count();
    let mut src = 0;
    for (byte_i, &byte) in mask.iter().enumerate() {
        let mut bits = byte;
        while bits != 0 {
            let i = byte_i * 8 + bits.trailing_zeros() as usize;
            if i >= block_count {
                return corrupt(format!("delta frame at {offset} sets mask bits past the last block"));
            }
            for (dst, residual) in frame[i * bb..(i + 1) * bb].iter_mut().zip(&blocks[src..src + bb]) {
                *dst ^= residual;
            }
            src += bb;
            bits &= bits - 1;
        }
    }
    Ok(())
}

fn corrupt<T>(msg: String) -> Result<T> {
    Err(Error::Corrupt(msg))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Stopped,
    Playing,
    Paused,
    Finished,
}

pub struct Player<R: Read + Seek> {
    reader: Reader<R>,
    tracks: Vec<Track>,
    track_infos: Vec<TrackInfo>,
    audio: Vec<AudioTrack>,
    audio_infos: Vec<AudioTrackInfo>,
    audio_tolerance_us: u64,
    /// Largest preload of the selected audio tracks.
    max_preload_us: u64,
    /// While looping: offset of the next chunk to check for the next pass's audio.
    loop_scan: Option<u64>,
    routes: Vec<Route>,
    cue_streams: Vec<CueStreamInfo>,
    /// Per stream number: a cue stream that is enabled.
    cue_enabled: Vec<bool>,
    active_cues: Vec<ActiveCue>,
    /// Cues from `replace_cues` in emission order; the file's cue streams are then disabled.
    replaced_cues: Option<Vec<ReplacedCue>>,
    /// Clock up to which cues have been processed in this pass; `None` before any.
    cues_through: Option<u64>,
    index: Vec<IndexEntry>,
    clock_us: u64,
    /// Nanoseconds of `dt` below a whole microsecond, carried to the next `advance`.
    dt_carry_ns: u32,
    /// The clock step of the latest `advance`: `motion` frames due within it of the clock
    /// start decoding ahead.
    ahead_window_us: u64,
    clock: Clock,
    duration_us: u64,
    state: State,
    looping: bool,
    events: VecDeque<Event>,
    /// Next chunk, read but not yet due.
    pending: Option<ChunkHeader>,
    at_end: bool,
    payload: Vec<u8>,
    /// Decoded samples of the latest audio chunk.
    samples: Vec<f32>,
    frame_counter: u64,
}

impl<R: Read + Seek> Player<R> {
    /// Opens a file and selects one variant per group.
    pub fn open(reader: R, caps: &Capabilities) -> Result<Player<R>> {
        let mut reader = Reader::new(reader)?;
        let streams = reader.streams().to_vec();

        let mut groups: BTreeMap<u16, Vec<u16>> = BTreeMap::new();
        for (i, s) in streams.iter().enumerate() {
            if let StreamParams::Video(_) = s.params {
                groups.entry(s.group).or_default().push(i as u16);
            }
        }
        let mut routes = vec![Route::Skip; streams.len()];
        let mut tracks = Vec::new();
        for (&group, members) in &groups {
            let chosen = caps.formats.iter().find_map(|f| {
                members.iter().copied().find(|&s| streams[s as usize].video_params().is_some_and(|v| v.format == *f))
            });
            let Some(stream) = chosen else {
                return Err(Error::NoSupportedVariant { group });
            };
            let s = &streams[stream as usize];
            let params = *s.video_params().expect("selected stream is video");
            routes[stream as usize] = Route::Video(tracks.len());
            tracks.push(Track {
                info: TrackInfo { stream, group, params },
                time_base: s.time_base,
                frame: vec![0; params.frame_size()],
                has_frame: false,
                index: 0,
                pts: 0,
                resume_offset: 0,
                motion: (params.format == TextureFormat::Motion).then(|| MotionQueue {
                    decoder: Decoder::new(&params),
                    frames: Vec::new(),
                    queued: 0,
                    ahead: VecDeque::new(),
                }),
            });
        }
        let mut audio_groups: BTreeMap<u16, Vec<u16>> = BTreeMap::new();
        for (i, s) in streams.iter().enumerate() {
            if let StreamParams::Audio(_) = s.params {
                audio_groups.entry(s.group).or_default().push(i as u16);
            }
        }
        let audio_tolerance_us = DEFAULT_AUDIO_SYNC_TOLERANCE.as_micros() as u64;
        let mut audio = Vec::new();
        for (&group, members) in &audio_groups {
            let stream = select_audio_variant(&streams, members, caps.max_audio_channels);
            let params = streams[stream as usize].audio_params().expect("selected stream is audio").clone();
            routes[stream as usize] = Route::Audio(audio.len());
            let decoder = AudioDecoder::new(&params).map_err(|e| Error::Corrupt(format!("audio stream {stream}: {e}")))?;
            audio.push(AudioTrack {
                output: AudioOutput::new(&params, audio_tolerance_us),
                info: AudioTrackInfo { stream, group, params },
                enabled: true,
                base_frame: 0,
                resume_offset: 0,
                decoder,
            });
        }
        let mut cue_streams = Vec::new();
        let mut cue_enabled = vec![false; streams.len()];
        for (i, s) in streams.iter().enumerate() {
            if let Some(c) = s.cue_params() {
                cue_streams.push(CueStreamInfo { stream: i as u16, name: c.name.clone() });
                cue_enabled[i] = true;
                routes[i] = Route::Cues;
            }
        }
        let index = reader.read_index()?;
        let duration_us = reader.header().duration_us;
        Ok(Player {
            reader,
            track_infos: tracks.iter().map(|t| t.info).collect(),
            tracks,
            audio_infos: audio.iter().map(|a| a.info.clone()).collect(),
            max_preload_us: audio.iter().map(|a| a.info.params.preload_us as u64).max().unwrap_or(0),
            loop_scan: None,
            audio,
            audio_tolerance_us,
            routes,
            cue_streams,
            cue_enabled,
            active_cues: Vec::new(),
            replaced_cues: None,
            cues_through: None,
            index,
            clock_us: 0,
            dt_carry_ns: 0,
            ahead_window_us: 0,
            clock: Clock::Game,
            duration_us,
            state: State::Stopped,
            looping: false,
            events: VecDeque::new(),
            pending: None,
            at_end: false,
            payload: Vec::new(),
            samples: Vec::new(),
            frame_counter: 0,
        })
    }

    /// The selected video stream of each group, in group order.
    pub fn tracks(&self) -> &[TrackInfo] {
        &self.track_infos
    }

    /// The selected audio stream of each audio group, in group order.
    pub fn audio_tracks(&self) -> &[AudioTrackInfo] {
        &self.audio_infos
    }

    /// A handle to the samples of audio track `track`; all handles of a track share one queue.
    pub fn audio_output(&self, track: usize) -> Option<AudioOutput> {
        self.audio.get(track).map(|a| a.output.clone())
    }

    /// Turns decoding of an audio track on or off. A disabled track's outputs return nothing;
    /// an enabled one resumes at the clock with the next decoded chunk.
    pub fn set_audio_track_enabled(&mut self, track: usize, enabled: bool) {
        let clock_us = self.clock_us;
        let Some(a) = self.audio.get_mut(track) else {
            return;
        };
        if a.enabled != enabled {
            a.enabled = enabled;
            let at = a.base_frame + a.frames(clock_us);
            a.output.lock().reset(at);
            a.decoder.reset();
        }
        self.sync_audio();
    }

    /// How far an audio output's reads may fall behind the clock before they skip forward.
    pub fn set_audio_sync_tolerance(&mut self, tolerance: Duration) {
        self.audio_tolerance_us = tolerance.as_micros().min(u32::MAX as u128) as u64;
        for a in &self.audio {
            a.output.lock().set_tolerance(self.audio_tolerance_us);
        }
    }

    /// Cue streams in stream order.
    pub fn cue_streams(&self) -> &[CueStreamInfo] {
        &self.cue_streams
    }

    /// Turns emitting the cues of `stream` on or off. Disabling ends its active cues.
    /// Streams that are not cue streams are ignored.
    pub fn set_cue_stream_enabled(&mut self, stream: u16, enabled: bool) {
        if !self.cue_streams.iter().any(|c| c.stream == stream) {
            return;
        }
        self.cue_enabled[stream as usize] = enabled;
        if !enabled {
            self.end_cues(|c| c.stream == stream);
        }
    }

    /// Plays `tracks` in place of the file's cue streams, as if the file had been written
    /// with them (`docs/features/cue-preview.md#player`). Their streams are numbered from
    /// the file's stream count up; tracks beyond stream 65535 are dropped. For cues, this
    /// acts like a seek to the current position.
    pub fn replace_cues(&mut self, tracks: Vec<CueTrack>) {
        self.end_cues(|_| true);
        let first = self.reader.streams().len();
        let tracks = &tracks[..tracks.len().min((u16::MAX as usize + 1).saturating_sub(first))];
        self.cue_enabled = vec![true; first + tracks.len()];
        self.cue_enabled[..first].fill(false);
        self.cue_streams = tracks
            .iter()
            .enumerate()
            .map(|(i, t)| CueStreamInfo { stream: (first + i) as u16, name: t.name.clone() })
            .collect();
        let mut cues: Vec<ReplacedCue> = tracks
            .iter()
            .enumerate()
            .flat_map(|(i, t)| {
                t.cues.iter().map(move |c| ReplacedCue {
                    start_us: c.time_us,
                    end_us: c.time_us.saturating_add(c.cue.duration),
                    stream: (first + i) as u16,
                    cue: c.cue.clone(),
                })
            })
            .collect();
        cues.sort_by_key(|c| c.start_us);
        self.replaced_cues = Some(cues);
        if let Some(through) = self.cues_through.filter(|_| self.state != State::Finished) {
            self.begin_active_replaced_cues(through);
        }
    }

    /// Starts or resumes playback and decodes every chunk due at the current clock.
    pub fn play(&mut self) -> Result<()> {
        match self.state {
            State::Playing => return Ok(()),
            State::Paused => {
                self.state = State::Playing;
                self.events.push_back(Event::Resumed);
            }
            State::Stopped | State::Finished => {
                if self.state == State::Finished {
                    self.rewind();
                }
                self.state = State::Playing;
                self.events.push_back(Event::Started);
            }
        }
        self.decode_due()
    }

    pub fn pause(&mut self) {
        if self.state == State::Playing {
            self.state = State::Paused;
            self.events.push_back(Event::Paused);
            self.sync_audio();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.state == State::Playing
    }

    pub fn is_finished(&self) -> bool {
        self.state == State::Finished
    }

    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    pub fn looping(&self) -> bool {
        self.looping
    }

    pub fn position(&self) -> Duration {
        Duration::from_micros(self.clock_us)
    }

    pub fn duration(&self) -> Duration {
        Duration::from_micros(self.duration_us)
    }

    /// Sets what drives the clock; takes effect at the next `advance`.
    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// Moves the clock forward by `dt` when playing (steered toward the master audio track
    /// with [`Clock::Audio`]) and decodes every chunk that became due.
    pub fn advance(&mut self, dt: Duration) -> Result<()> {
        if self.state != State::Playing {
            return Ok(());
        }
        let dt_ns = dt.as_nanos() + self.dt_carry_ns as u128;
        self.dt_carry_ns = (dt_ns % 1000) as u32;
        let next = self.next_clock((dt_ns / 1000).min(u64::MAX as u128) as u64);
        self.ahead_window_us = next.saturating_sub(self.clock_us);
        self.clock_us = next;
        self.decode_due()
    }

    /// Decodes every chunk due at the clock, then loops or finishes at the end.
    fn decode_due(&mut self) -> Result<()> {
        loop {
            self.process_due(true)?;
            if !(self.at_end && self.clock_us >= self.duration_us) {
                self.prefetch_next_pass()?;
                self.start_frames_ahead()?;
                self.sync_audio();
                return Ok(());
            }
            self.end_cues(|_| true);
            if self.looping && self.duration_us > 0 {
                let carry = self.clock_us - self.duration_us;
                self.rewind();
                self.clock_us = carry;
                self.events.push_back(Event::Looped);
            } else {
                self.clock_us = self.duration_us;
                self.state = State::Finished;
                self.events.push_back(Event::Finished);
                // The next pass, after `play()`, starts on a clean queue.
                for a in &self.audio {
                    let next_pass = a.base_frame + a.frames(self.duration_us);
                    a.output.lock().reset(next_pass);
                }
                self.sync_audio();
                return Ok(());
            }
        }
    }

    /// Jumps to `to` (clamped to the duration), decoding from the preceding keyframes.
    /// Instantaneous cues at or before `to` are not emitted; cues active at `to` are.
    /// A finished player becomes paused.
    pub fn seek(&mut self, to: Duration) -> Result<()> {
        let target = (to.as_micros().min(u64::MAX as u128) as u64).min(self.duration_us);
        self.end_cues(|_| true);
        let mut start = u64::MAX;
        for t in &mut self.tracks {
            let entries = self.index.iter().filter(|e| e.stream == t.info.stream);
            let before = entries.clone().rev().find(|e| t.time_base.to_micros(e.pts) <= target);
            let Some(entry) = before.or_else(|| entries.clone().next()) else {
                return corrupt(format!("stream {} has no keyframe in the index", t.info.stream));
            };
            t.resume_offset = entry.offset;
            start = start.min(entry.offset);
        }
        for a in &mut self.audio {
            let s = &self.reader.streams()[a.info.stream as usize];
            let entries = self.index.iter().filter(|e| e.stream == a.info.stream);
            // Opus decoders converge over a pre-roll before the target; its frames are dropped.
            let from = match a.info.params.format {
                SampleFormat::Opus => target.saturating_sub(format::opus::PREROLL_US),
                _ => target,
            };
            let before = entries.clone().rev().find(|e| s.time_base.to_micros(e.pts) <= from);
            a.resume_offset = match before.or_else(|| entries.clone().next()) {
                Some(entry) => entry.offset,
                None => u64::MAX,
            };
            start = start.min(a.resume_offset);
            let at = a.base_frame + a.frames(target);
            a.output.lock().reset(at);
            a.decoder.reset();
        }
        if start == u64::MAX {
            start = self.reader.data_start();
        }
        self.reader.seek_to(start)?;
        self.pending = None;
        self.at_end = false;
        self.loop_scan = None;
        self.clock_us = target;
        self.cues_through = None;
        self.process_due(false)?;
        if self.state == State::Finished {
            self.state = State::Paused;
        }
        self.events.push_back(Event::Seeked { to: Duration::from_micros(target) });
        self.sync_audio();
        self.begin_active_cues(target)
    }

    /// The latest decoded frame of `track`, or `None` before its first frame.
    pub fn video_frame(&self, track: usize) -> Option<VideoFrame<'_>> {
        let t = self.tracks.get(track)?;
        if !t.has_frame {
            return None;
        }
        let p = &t.info.params;
        Some(VideoFrame {
            format: p.format,
            srgb: p.srgb(),
            alpha: p.alpha(),
            width: p.width,
            height: p.height,
            data: &t.frame,
            index: t.index,
            time: Duration::from_micros(t.time_base.to_micros(t.pts)),
        })
    }

    /// The coded frames of `motion` track `track` decoded since the last call, oldest
    /// first, for a GPU decoder to reconstruct in order (`docs/features/motion-codec.md`).
    /// Empty for other tracks. A keyframe drops the frames queued before it.
    pub fn take_coded_frames(&mut self, track: usize) -> &[CodedFrame] {
        let Some(m) = self.tracks.get_mut(track).and_then(|t| t.motion.as_mut()) else {
            return &[];
        };
        let n = std::mem::take(&mut m.queued);
        &m.frames[..n]
    }

    /// Sets the most threads entropy-decoding the slices of one `motion` frame; 1 decodes
    /// on the calling thread only.
    pub fn set_decode_threads(&mut self, threads: usize) {
        for m in self.tracks.iter_mut().filter_map(|t| t.motion.as_mut()) {
            // Changing threads cancels the decode-ahead.
            m.decoder.set_threads(threads);
            m.ahead.clear();
        }
    }

    /// The next queued event, oldest first.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// Restarts reading at the first chunk. Decoded frames stay visible until replaced;
    /// audio of the new pass is queued after the old one.
    fn rewind(&mut self) {
        self.pending = None;
        self.at_end = false;
        self.loop_scan = None;
        for t in &mut self.tracks {
            t.resume_offset = 0;
        }
        for a in &mut self.audio {
            a.resume_offset = 0;
            a.base_frame += a.frames(self.duration_us);
        }
        self.clock_us = 0;
        self.cues_through = None;
        let start = self.reader.data_start();
        self.reader.seek_to(start).expect("data start is a valid chunk offset");
    }

    /// Decodes every chunk due at the clock. `emit_cues = false` (seeking) skips cues.
    fn process_due(&mut self, emit_cues: bool) -> Result<()> {
        loop {
            let chunk = match self.pending.take() {
                Some(c) => c,
                None => match self.reader.next_chunk()? {
                    Some(c) => c,
                    None => {
                        self.at_end = true;
                        break;
                    }
                },
            };
            let stream = chunk.stream as usize;
            let time_us = self.reader.streams()[stream].decode_time_us(chunk.pts);
            if time_us > self.clock_us {
                self.pending = Some(chunk);
                break;
            }
            self.process_replaced_cues(time_us, emit_cues);
            self.end_cues(|c| c.end_us <= time_us);
            match self.routes[stream] {
                Route::Video(ti) => {
                    if chunk.offset >= self.tracks[ti].resume_offset {
                        self.frame_counter += 1;
                        if !self.tracks[ti].apply_ahead(&chunk, self.frame_counter)? {
                            self.reader.read_payload(&chunk, &mut self.payload)?;
                            self.tracks[ti].apply(&chunk, &self.payload, self.frame_counter)?;
                        }
                    }
                }
                Route::Audio(ai) => {
                    let a = &self.audio[ai];
                    if a.enabled && chunk.offset >= a.resume_offset {
                        self.queue_audio(ai, &chunk, a.base_frame + chunk.pts)?;
                    }
                }
                Route::Cues if self.cue_enabled[stream] && emit_cues => self.begin_cue(&chunk)?,
                Route::Cues | Route::Skip => {}
            }
        }
        let clock = self.clock_us;
        self.process_replaced_cues(clock, emit_cues);
        self.end_cues(|c| c.end_us <= clock);
        Ok(())
    }

    /// Processes the replaced cues after `cues_through` and up to `limit`, emitting them
    /// when `emit` is set, and moves `cues_through` to `limit`. Cues after the duration
    /// are never reached, even when the clock overshoots the end before looping.
    fn process_replaced_cues(&mut self, limit: u64, emit: bool) {
        let through = self.cues_through;
        let limit = limit.min(self.duration_us).max(through.unwrap_or(0));
        self.cues_through = Some(limit);
        let Some(cues) = self.replaced_cues.take() else { return };
        let start = through.map_or(0, |t| cues.partition_point(|c| c.start_us <= t));
        let end = cues.partition_point(|c| c.start_us <= limit);
        if emit {
            for (i, c) in cues.iter().enumerate().take(end).skip(start) {
                if self.cue_enabled[c.stream as usize] {
                    self.end_cues(|a| a.end_us <= c.start_us);
                    self.begin_replaced_cue(i, c);
                }
            }
        }
        self.replaced_cues = Some(cues);
    }

    /// Emits the `Cue` event of replaced cue `i`.
    fn begin_replaced_cue(&mut self, i: usize, c: &ReplacedCue) {
        let id = REPLACED_CUE_ID + i as u64;
        if c.end_us > c.start_us {
            self.active_cues.push(ActiveCue { end_us: c.end_us, id, stream: c.stream });
        }
        self.events.push_back(Event::Cue(CueEvent {
            id,
            stream: c.stream,
            time: Duration::from_micros(c.start_us),
            duration: Duration::from_micros(c.end_us - c.start_us),
            kind: c.cue.kind.clone(),
            fields: c.cue.fields.clone(),
        }));
    }

    /// Emits `Cue` for every enabled replaced cue with a duration that is active at `target`.
    fn begin_active_replaced_cues(&mut self, target: u64) {
        let Some(cues) = self.replaced_cues.take() else { return };
        let end = cues.partition_point(|c| c.start_us <= target);
        for (i, c) in cues.iter().enumerate().take(end) {
            if c.end_us > target && self.cue_enabled[c.stream as usize] {
                self.begin_replaced_cue(i, c);
            }
        }
        self.replaced_cues = Some(cues);
    }

    /// While looping near the end, queues the audio chunks of the next pass that are already
    /// due in its timeline, so outputs read ahead of the clock do not run dry at the loop
    /// point. Those chunks sit at the start of the file, all with decode time 0.
    fn prefetch_next_pass(&mut self) -> Result<()> {
        if !self.looping || self.audio.is_empty() || self.duration_us == 0 {
            return Ok(());
        }
        if self.clock_us.saturating_add(self.max_preload_us) < self.duration_us {
            return Ok(());
        }
        let mut offset = self.loop_scan.unwrap_or(self.reader.data_start());
        while offset < self.reader.header().index_offset {
            let chunk = self.reader.chunk_at(offset)?;
            let s = &self.reader.streams()[chunk.stream as usize];
            if s.decode_time_us(chunk.pts) > 0 {
                break;
            }
            if let Route::Audio(ai) = self.routes[chunk.stream as usize] {
                // Due in the next pass at `time - preload`, which is `duration` later on this clock.
                let time = s.time_base.to_micros(chunk.pts);
                let a = &self.audio[ai];
                if time.saturating_add(self.duration_us) > self.clock_us.saturating_add(a.info.params.preload_us as u64) {
                    break;
                }
                if a.enabled {
                    let at = a.base_frame + a.frames(self.duration_us) + chunk.pts;
                    self.queue_audio(ai, &chunk, at)?;
                }
            }
            offset = chunk.offset + CHUNK_HEADER_SIZE + chunk.stored_size as u64;
        }
        self.loop_scan = Some(offset);
        Ok(())
    }

    /// Starts decoding the frames of each `motion` track on the decoder's workers that the
    /// next `advance` of the same length will reach
    /// (`docs/features/motion-codec.md#decode-ahead`).
    fn start_frames_ahead(&mut self) -> Result<()> {
        // Tracks that may queue more frames.
        let mut open: Vec<bool> = self
            .tracks
            .iter()
            .map(|t| t.motion.as_ref().is_some_and(|m| m.decoder.threads() > 1 && m.decoder.ahead() < MAX_AHEAD))
            .collect();
        let limit = self.clock_us.saturating_add(self.ahead_window_us);
        let mut next = self.pending;
        while let Some(chunk) = next
            && open.contains(&true)
        {
            let stream = &self.reader.streams()[chunk.stream as usize];
            if stream.decode_time_us(chunk.pts) > limit {
                break;
            }
            if let Route::Video(ti) = self.routes[chunk.stream as usize]
                && open[ti]
            {
                let track = &self.tracks[ti];
                let m = track.motion.as_ref().expect("motion track");
                // Frames before the resume point are not decoded, so neither are later ones.
                if chunk.offset < track.resume_offset {
                    open[ti] = false;
                } else if m.ahead.back().is_none_or(|&last| chunk.offset > last) {
                    self.reader.read_payload(&chunk, &mut self.payload)?;
                    let m = self.tracks[ti].motion.as_mut().expect("motion track");
                    if m.decoder.decode_ahead(chunk.offset, chunk.keyframe(), &self.payload) {
                        m.ahead.push_back(chunk.offset);
                        open[ti] = m.decoder.ahead() < MAX_AHEAD;
                    } else {
                        open[ti] = false;
                    }
                }
            }
            let end = chunk.offset + CHUNK_HEADER_SIZE + chunk.stored_size as u64;
            next = if end < self.reader.header().index_offset { Some(self.reader.chunk_at(end)?) } else { None };
        }
        Ok(())
    }

    /// Reads the audio chunk `chunk` of track `ai` and queues it at output frame `at`.
    fn queue_audio(&mut self, ai: usize, chunk: &ChunkHeader, at: u64) -> Result<()> {
        self.reader.read_payload(chunk, &mut self.payload)?;
        let a = &mut self.audio[ai];
        if let Err(e) = a.decoder.decode(chunk.pts, &self.payload, &mut self.samples) {
            return corrupt(format!("audio chunk at {}: {e}", chunk.offset));
        }
        a.output.lock().push(at, &self.samples);
        Ok(())
    }

    /// The clock after `advance(dt)`, before chunks are processed.
    fn next_clock(&self, dt_us: u64) -> u64 {
        let predicted = self.clock_us.saturating_add(dt_us);
        let Clock::Audio { track, latency } = self.clock else {
            return predicted;
        };
        let Some(a) = self.audio.get(track).filter(|a| a.enabled) else {
            return predicted;
        };
        // Output frames, in i128 so the subtractions cannot overflow.
        let rate = a.info.params.sample_rate as i128;
        let latency_frames = latency.as_micros() as i128 * rate / 1_000_000;
        let heard = a.output.lock().read_position() as i128 - latency_frames;
        let err_us = (heard - (a.base_frame + a.frames(predicted)) as i128) * 1_000_000 / rate;
        let tolerance = self.audio_tolerance_us as i128;
        let next = if err_us > tolerance {
            predicted as i128 + err_us
        } else if err_us < -tolerance {
            predicted as i128
        } else {
            let weight = dt_us.min(AUDIO_CLOCK_SMOOTHING_US) as i128;
            predicted as i128 + err_us * weight / AUDIO_CLOCK_SMOOTHING_US as i128
        };
        next.clamp(self.clock_us as i128, u64::MAX as i128) as u64
    }

    /// Hands the clock and transport state to the audio outputs.
    fn sync_audio(&self) {
        let playing = self.state == State::Playing;
        for a in &self.audio {
            let mut q = a.output.lock();
            q.open = playing && a.enabled;
            q.clock = a.base_frame + a.frames(self.clock_us);
        }
    }

    /// Reads the cue in `chunk` and emits its `Cue` event.
    fn begin_cue(&mut self, chunk: &ChunkHeader) -> Result<()> {
        self.reader.read_payload(chunk, &mut self.payload)?;
        let cue = Cue::decode(&self.payload)?;
        let time_base = self.reader.streams()[chunk.stream as usize].time_base;
        let start_us = time_base.to_micros(chunk.pts);
        let end_us = time_base.to_micros(chunk.pts.saturating_add(cue.duration));
        if cue.duration > 0 {
            self.active_cues.push(ActiveCue { end_us, id: chunk.offset, stream: chunk.stream });
        }
        self.events.push_back(Event::Cue(CueEvent {
            id: chunk.offset,
            stream: chunk.stream,
            time: Duration::from_micros(start_us),
            duration: Duration::from_micros(end_us - start_us),
            kind: cue.kind,
            fields: cue.fields,
        }));
        Ok(())
    }

    /// Emits `CueEnd` for the active cues matching `pred`, earliest end first.
    fn end_cues(&mut self, pred: impl Fn(&ActiveCue) -> bool) {
        let (mut ended, kept): (Vec<_>, Vec<_>) = self.active_cues.drain(..).partition(|c| pred(c));
        self.active_cues = kept;
        ended.sort_by_key(|c| (c.end_us, c.id));
        for c in ended {
            self.events.push_back(Event::CueEnd { id: c.id, stream: c.stream });
        }
    }

    /// Emits `Cue` for every enabled cue with a duration that is active at `target`
    /// (`docs/spec/PRAXI.md#seeking`).
    fn begin_active_cues(&mut self, target: u64) -> Result<()> {
        if self.replaced_cues.is_some() {
            self.begin_active_replaced_cues(target);
            return Ok(());
        }
        let mut found = Vec::new();
        for c in &self.cue_streams {
            if !self.cue_enabled[c.stream as usize] {
                continue;
            }
            let s = &self.reader.streams()[c.stream as usize];
            let max_us = s.cue_params().map_or(0, |p| s.time_base.to_micros(p.max_duration));
            let low = target.saturating_sub(max_us.saturating_add(1));
            for e in self.index.iter().filter(|e| e.stream == c.stream) {
                let t = s.time_base.to_micros(e.pts);
                if t >= low && t <= target {
                    found.push((t, e.offset));
                }
            }
        }
        found.sort();
        for (_, offset) in found {
            let chunk = self.reader.chunk_at(offset)?;
            self.reader.read_payload(&chunk, &mut self.payload)?;
            let cue = Cue::decode(&self.payload)?;
            let time_base = self.reader.streams()[chunk.stream as usize].time_base;
            if cue.duration > 0 && time_base.to_micros(chunk.pts.saturating_add(cue.duration)) > target {
                self.begin_cue(&chunk)?;
            }
        }
        Ok(())
    }
}

/// Picks the variant with the most channels at or below `max`, else the one with the fewest;
/// the earlier stream on ties (`docs/features/audio.md#variant-selection`).
fn select_audio_variant(streams: &[StreamInfo], members: &[u16], max: Option<u16>) -> u16 {
    let channels = |s: u16| streams[s as usize].audio_params().map_or(0, |a| a.channels);
    let fitting = members.iter().copied().filter(|&s| max.is_none_or(|m| channels(s) <= m));
    fitting
        .max_by_key(|&s| (channels(s), Reverse(s)))
        .or_else(|| members.iter().copied().min_by_key(|&s| (channels(s), s)))
        .expect("audio groups have members")
}
