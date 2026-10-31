//! Video player for games (`docs/features/player.md`).
//!
//! Frames are GPU block-compressed textures; the player reconstructs them from keyframes and
//! delta frames and hands the raw blocks to the engine for upload. Audio is queued for the
//! engine's mixer (`docs/features/audio.md`).

mod audio;

use audio::us_to_frames;
use praxi_format::{CHUNK_HEADER_SIZE, ChunkHeader, Cue, IndexEntry, Reader, StreamInfo, StreamParams, TimeBase};
use std::cmp::Reverse;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io::{Read, Seek};
use std::time::Duration;

pub use audio::AudioOutput;
pub use praxi_format::{self as format, AudioParams, SampleFormat, TextureFormat, Value, VideoParams};

/// Default for [`Player::set_audio_sync_tolerance`].
pub const DEFAULT_AUDIO_SYNC_TOLERANCE: Duration = Duration::from_millis(60);

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueStreamInfo {
    pub stream: u16,
    pub name: String,
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
    /// Blocks in row-major order, ready to upload as a compressed texture.
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
}

impl Track {
    fn apply(&mut self, chunk: &ChunkHeader, payload: &[u8], index: u64) -> Result<()> {
        let p = &self.info.params;
        if chunk.keyframe() {
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
        self.has_frame = true;
        self.index = index;
        self.pts = chunk.pts;
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
    index: Vec<IndexEntry>,
    clock_us: u64,
    duration_us: u64,
    state: State,
    looping: bool,
    events: VecDeque<Event>,
    /// Next chunk, read but not yet due.
    pending: Option<ChunkHeader>,
    at_end: bool,
    payload: Vec<u8>,
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
            audio.push(AudioTrack {
                output: AudioOutput::new(&params, audio_tolerance_us),
                info: AudioTrackInfo { stream, group, params },
                enabled: true,
                base_frame: 0,
                resume_offset: 0,
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
            index,
            clock_us: 0,
            duration_us,
            state: State::Stopped,
            looping: false,
            events: VecDeque::new(),
            pending: None,
            at_end: false,
            payload: Vec::new(),
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

    pub fn play(&mut self) {
        match self.state {
            State::Playing => {}
            State::Paused => {
                self.state = State::Playing;
                self.events.push_back(Event::Resumed);
                self.sync_audio();
            }
            State::Stopped | State::Finished => {
                if self.state == State::Finished {
                    self.rewind();
                }
                self.state = State::Playing;
                self.events.push_back(Event::Started);
                self.sync_audio();
            }
        }
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

    /// Moves the clock forward by `dt` when playing and decodes every chunk that became due.
    pub fn advance(&mut self, dt: Duration) -> Result<()> {
        if self.state != State::Playing {
            return Ok(());
        }
        self.clock_us = self.clock_us.saturating_add(dt.as_micros().min(u64::MAX as u128) as u64);
        loop {
            self.process_due(true)?;
            if !(self.at_end && self.clock_us >= self.duration_us) {
                self.prefetch_next_pass()?;
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
            let before = entries.clone().rev().find(|e| s.time_base.to_micros(e.pts) <= target);
            a.resume_offset = match before.or_else(|| entries.clone().next()) {
                Some(entry) => entry.offset,
                None => u64::MAX,
            };
            start = start.min(a.resume_offset);
            let at = a.base_frame + a.frames(target);
            a.output.lock().reset(at);
        }
        if start == u64::MAX {
            start = self.reader.data_start();
        }
        self.reader.seek_to(start)?;
        self.pending = None;
        self.at_end = false;
        self.loop_scan = None;
        self.clock_us = target;
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
            self.end_cues(|c| c.end_us <= time_us);
            match self.routes[stream] {
                Route::Video(ti) => {
                    if chunk.offset >= self.tracks[ti].resume_offset {
                        self.reader.read_payload(&chunk, &mut self.payload)?;
                        self.frame_counter += 1;
                        self.tracks[ti].apply(&chunk, &self.payload, self.frame_counter)?;
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
        self.end_cues(|c| c.end_us <= clock);
        Ok(())
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

    /// Reads the audio chunk `chunk` of track `ai` and queues it at output frame `at`.
    fn queue_audio(&mut self, ai: usize, chunk: &ChunkHeader, at: u64) -> Result<()> {
        self.reader.read_payload(chunk, &mut self.payload)?;
        let a = &self.audio[ai];
        let p = &a.info.params;
        if self.payload.is_empty() || !self.payload.len().is_multiple_of(p.frame_bytes()) {
            return corrupt(format!("audio chunk at {} is not a whole number of frames", chunk.offset));
        }
        a.output.lock().push(at, &self.payload, p.format);
        Ok(())
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
