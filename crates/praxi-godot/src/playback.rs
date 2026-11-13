//! `VideoStreamPlaybackPraxi`: a praxi player behind `VideoStreamPlayer`
//! (`docs/features/godot.md#videostreamplaybackpraxi-extends-videostreamplayback`).

use crate::file::GodotFile;
use crate::stream::VideoStreamPraxi;
use godot::classes::image::Format;
use godot::classes::{IVideoStreamPlayback, Image, ImageTexture, RenderingServer, Texture2D, VideoStreamPlayback};
use godot::prelude::*;
use praxi::format::cue::Value;
use praxi::{AudioOutput, Capabilities, CueEvent, Event, Player, TextureFormat};
use std::io::BufReader;
use std::time::Duration;

type FilePlayer = Player<BufReader<GodotFile>>;

/// Samples kept for `mix_audio` at most, per channel: half a second at 48 kHz.
const MAX_PENDING_FRAMES: usize = 24_000;
/// Samples read from the output at a time, per channel.
const READ_FRAMES: usize = 4096;

#[derive(GodotClass)]
#[class(base = VideoStreamPlayback, init, tool)]
pub struct VideoStreamPlaybackPraxi {
    base: Base<VideoStreamPlayback>,
    player: Option<FilePlayer>,
    stream: Option<Gd<VideoStreamPraxi>>,
    /// Between `_play` and `_stop` or the end.
    playing: bool,
    paused: bool,
    texture: Option<Gd<ImageTexture>>,
    /// `VideoFrame::index` of the frame in `texture`.
    shown: Option<u64>,
    audio_track: usize,
    audio: Option<AudioOutput>,
    /// Interleaved samples read but not yet accepted by `mix_audio`.
    pending: Vec<f32>,
    scratch: Vec<f32>,
}

/// The texture formats this rendering device samples (`docs/features/godot.md#texture-formats`).
fn device_formats() -> Vec<TextureFormat> {
    let rs = RenderingServer::singleton();
    let mut formats = Vec::new();
    if rs.has_os_feature("bptc") {
        formats.push(TextureFormat::Bc7);
    }
    if rs.has_os_feature("s3tc") {
        formats.push(TextureFormat::Bc1);
    }
    if rs.has_os_feature("astc") {
        formats.extend([TextureFormat::Astc4x4, TextureFormat::Astc8x8]);
    }
    formats
}

/// The Godot image format of a praxi texture format, for those Godot has.
fn image_format(format: TextureFormat) -> Option<Format> {
    match format {
        TextureFormat::Bc7 => Some(Format::BPTC_RGBA),
        TextureFormat::Bc1 => Some(Format::DXT1),
        TextureFormat::Astc4x4 => Some(Format::ASTC_4x4),
        TextureFormat::Astc8x8 => Some(Format::ASTC_8x8),
        _ => None,
    }
}

fn value(v: &Value) -> Variant {
    match v {
        Value::String(s) => s.to_variant(),
        Value::Int(i) => i.to_variant(),
        Value::Float(f) => f.to_variant(),
        Value::Bool(b) => b.to_variant(),
        Value::Bytes(b) => PackedByteArray::from(b.as_slice()).to_variant(),
    }
}

fn position(cue: &CueEvent) -> Variant {
    cue.position().map_or_else(Variant::nil, |(x, y)| Vector2::new(x, y).to_variant())
}

fn fields(cue: &CueEvent) -> VarDictionary {
    let mut d = VarDictionary::new();
    for (k, v) in &cue.fields {
        d.set(k.as_str(), &value(v));
    }
    d
}

/// A cue as the dictionary of `active_cues()` (`docs/features/godot.md#cue-dictionaries`).
fn cue_dictionary(cue: &CueEvent) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set("id", cue.id as i64);
    d.set("type", cue.kind.as_str());
    d.set("fields", &fields(cue));
    d.set("time", cue.time.as_secs_f64());
    d.set("duration", cue.duration.as_secs_f64());
    d.set("position", &position(cue));
    d
}

impl VideoStreamPlaybackPraxi {
    /// Opens `path`; on failure prints the error and stays not playing.
    pub fn open(&mut self, path: &str, formats: &PackedStringArray, looping: bool, stream: Gd<VideoStreamPraxi>) {
        self.stream = Some(stream);
        match Self::open_player(path, formats) {
            Ok(mut player) => {
                player.set_looping(looping);
                self.player = Some(player);
                self.select_audio_track(0);
            }
            Err(e) => {
                godot_error!("praxi: {path}: {e}");
            }
        }
    }

    fn open_player(path: &str, formats: &PackedStringArray) -> Result<FilePlayer, String> {
        let accepted: Vec<TextureFormat> = if formats.is_empty() {
            device_formats()
        } else {
            formats
                .as_slice()
                .iter()
                .map(|f| TextureFormat::parse(&f.to_string()).ok_or_else(|| format!("unknown texture format {f}")))
                .collect::<Result<_, _>>()?
        };
        let accepted: Vec<TextureFormat> = accepted.into_iter().filter(|f| image_format(*f).is_some()).collect();
        let caps = Capabilities::new(accepted.clone()).max_audio_channels(2);
        let file = BufReader::with_capacity(256 * 1024, GodotFile::open(path)?);
        let player = Player::open(file, &caps).map_err(|e| e.to_string())?;
        if player.tracks().is_empty() {
            return Err(format!("no video variant in a format Godot can show here ({accepted:?})"));
        }
        Ok(player)
    }

    pub fn set_looping(&mut self, looping: bool) {
        if let Some(p) = &mut self.player {
            p.set_looping(looping);
        }
    }

    pub fn active_cues(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        if let Some(p) = &self.player {
            for cue in p.active_cues() {
                out.push(&cue_dictionary(cue));
            }
        }
        out
    }

    pub fn upcoming_cues(&self, within: f64) -> Array<VarDictionary> {
        let mut out = Array::new();
        if let Some(p) = &self.player {
            for (until, cue) in p.upcoming_cues(Duration::from_secs_f64(within.max(0.0))) {
                let mut d = cue_dictionary(cue);
                d.set("until", until.as_secs_f64());
                out.push(&d);
            }
        }
        out
    }

    fn select_audio_track(&mut self, track: usize) {
        let Some(player) = &mut self.player else { return };
        let count = player.audio_tracks().len();
        for i in 0..count {
            player.set_audio_track_enabled(i, i == track);
        }
        self.audio_track = track;
        self.audio = player.audio_output(track);
        self.pending.clear();
    }

    /// Uploads the frame of video track 0 if it is new (`docs/features/godot.md#video`).
    fn show_frame(&mut self) {
        let Some(frame) = self.player.as_ref().and_then(|p| p.video_frame(0)) else { return };
        if self.shown == Some(frame.index) {
            return;
        }
        let Some(format) = image_format(frame.format) else { return };
        let (w, h) = (frame.width as i32, frame.height as i32);
        let data = PackedByteArray::from(frame.data);
        self.shown = Some(frame.index);
        let Some(image) = Image::create_from_data(w, h, false, format, &data) else {
            godot_error!("praxi: cannot make a {w}x{h} {format:?} image of {} bytes", data.len());
            return;
        };
        match &mut self.texture {
            Some(t) if t.get_width() == w && t.get_height() == h && t.get_format() == format => t.update(&image),
            _ => self.texture = ImageTexture::create_from_image(&image),
        }
    }

    /// Hands decoded audio to `mix_audio` (`docs/features/godot.md#audio`).
    fn mix(&mut self) {
        let Some(out) = &self.audio else { return };
        let channels = out.channels().max(1);
        self.scratch.resize(READ_FRAMES * channels, 0.0);
        while self.pending.len() < MAX_PENDING_FRAMES * channels {
            let n = out.read(&mut self.scratch);
            if n == 0 {
                break;
            }
            self.pending.extend_from_slice(&self.scratch[..n]);
        }
        if self.pending.is_empty() {
            return;
        }
        let frames = self.pending.len() / channels;
        let buffer = PackedFloat32Array::from(&self.pending[..frames * channels]);
        let mixed = self.base_mut().mix_audio_ex(frames as i32).buffer(&buffer).offset(0).done();
        if mixed > 0 {
            self.pending.drain(..(mixed as usize * channels).min(self.pending.len()));
        }
    }
}

/// A signal to emit after the playback's update, when it is no longer borrowed.
enum Signal {
    Cue { id: i64, kind: GString, fields: VarDictionary, time: f64, duration: f64, position: Variant },
    CueEnded(i64),
    Looped,
}

#[godot_api]
impl IVideoStreamPlayback for VideoStreamPlaybackPraxi {
    fn play(&mut self) {
        let Some(p) = &mut self.player else { return };
        if let Err(e) = p.play() {
            godot_error!("praxi: {e}");
            return;
        }
        self.playing = true;
        self.paused = false;
    }

    fn stop(&mut self) {
        let Some(p) = &mut self.player else { return };
        p.pause();
        if let Err(e) = p.seek(Duration::ZERO) {
            godot_error!("praxi: {e}");
        }
        self.playing = false;
        self.paused = false;
        self.pending.clear();
    }

    fn is_playing(&self) -> bool {
        self.playing
    }

    fn set_paused(&mut self, paused: bool) {
        let Some(p) = &mut self.player else { return };
        if paused {
            p.pause();
        } else if self.playing
            && let Err(e) = p.play()
        {
            godot_error!("praxi: {e}");
        }
        self.paused = paused;
    }

    fn is_paused(&self) -> bool {
        self.paused
    }

    fn get_length(&self) -> f64 {
        self.player.as_ref().map_or(0.0, |p| p.duration().as_secs_f64())
    }

    fn get_playback_position(&self) -> f64 {
        self.player.as_ref().map_or(0.0, |p| p.position().as_secs_f64())
    }

    fn seek(&mut self, time: f64) {
        let Some(p) = &mut self.player else { return };
        let to = Duration::from_secs_f64(time.clamp(0.0, p.duration().as_secs_f64()));
        if let Err(e) = p.seek(to) {
            godot_error!("praxi: {e}");
        }
        self.pending.clear();
    }

    fn set_audio_track(&mut self, idx: i32) {
        self.select_audio_track(idx.max(0) as usize);
    }

    fn get_texture(&self) -> Option<Gd<Texture2D>> {
        self.texture.clone().map(Gd::upcast)
    }

    fn update(&mut self, delta: f64) {
        let Some(p) = &mut self.player else { return };
        if let Err(e) = p.advance(Duration::from_secs_f64(delta.max(0.0))) {
            godot_error!("praxi: {e}");
            self.playing = false;
            return;
        }
        let mut signals = Vec::new();
        while let Some(event) = p.poll_event() {
            match event {
                Event::Cue(c) => signals.push(Signal::Cue {
                    id: c.id as i64,
                    kind: GString::from(c.kind.as_str()),
                    fields: fields(&c),
                    time: c.time.as_secs_f64(),
                    duration: c.duration.as_secs_f64(),
                    position: position(&c),
                }),
                Event::CueEnd { id, .. } => signals.push(Signal::CueEnded(id as i64)),
                Event::Looped => signals.push(Signal::Looped),
                _ => {}
            }
        }
        if p.is_finished() {
            self.playing = false;
        }
        self.show_frame();
        self.mix();
        let Some(stream) = self.stream.clone() else { return };
        // Handlers may call back into this playback (active_cues), so it is released first.
        let _released = self.base_mut();
        for s in signals {
            match s {
                Signal::Cue { id, kind, fields, time, duration, position } => {
                    stream.signals().cue().emit(id, &kind, &fields, time, duration, &position)
                }
                Signal::CueEnded(id) => stream.signals().cue_ended().emit(id),
                Signal::Looped => stream.signals().looped().emit(),
            }
        }
    }

    fn get_channels(&self) -> i32 {
        self.audio.as_ref().map_or(0, |a| a.channels() as i32)
    }

    fn get_mix_rate(&self) -> i32 {
        self.audio.as_ref().map_or(0, |a| a.sample_rate() as i32)
    }
}
