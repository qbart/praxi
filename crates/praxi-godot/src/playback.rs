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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

type FilePlayer = Player<BufReader<GodotFile>>;

/// Samples kept for `mix_audio` at most, per channel: half a second at 48 kHz.
const MAX_PENDING_FRAMES: usize = 24_000;
/// Samples read from the output at a time, per channel.
const READ_FRAMES: usize = 4096;

/// The texture formats this rendering device samples (`docs/features/godot.md#texture-formats`).
/// ASTC HDR is only taken when `formats` names it: Godot does not report it.
fn device_formats() -> Vec<TextureFormat> {
    let rs = RenderingServer::singleton();
    let mut formats = Vec::new();
    if rs.has_os_feature("bptc") {
        formats.extend([TextureFormat::Bc7, TextureFormat::Bc6h]);
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
        TextureFormat::Bc6h => Some(Format::BPTC_RGBFU),
        TextureFormat::Astc4x4Hdr => Some(Format::ASTC_4x4_HDR),
        _ => None,
    }
}

/// `formats` with HDR formats first when `hdr`, last otherwise, each part in its order
/// (`docs/features/hdr.md#godot`).
fn order_formats(mut formats: Vec<TextureFormat>, hdr: bool) -> Vec<TextureFormat> {
    formats.sort_by_key(|f| f.is_hdr() != hdr);
    formats
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

/// A praxi player behind `VideoStreamPlayer`.
///
/// `VideoStreamPlayer` calls `_is_playing()` and `_is_paused()` from the audio thread, so
/// those read atomics, and `_update` does its work with the playback released
/// (`base_mut()`) and the player behind a mutex: the audio thread never waits for a frame.
#[derive(GodotClass)]
#[class(base = VideoStreamPlayback, init, tool)]
pub struct VideoStreamPlaybackPraxi {
    base: Base<VideoStreamPlayback>,
    state: Arc<Mutex<State>>,
    /// Kept apart from `state`: `ImageTexture.update` makes Godot ask for the texture.
    texture: Arc<Mutex<Option<Gd<ImageTexture>>>>,
    stream: Option<Gd<VideoStreamPraxi>>,
    /// Between `_play` and `_stop` or the end.
    playing: AtomicBool,
    paused: AtomicBool,
}

#[derive(Default)]
struct State {
    player: Option<FilePlayer>,
    /// `VideoFrame::index` of the frame in the texture.
    shown: Option<u64>,
    audio: Option<AudioOutput>,
    /// Interleaved samples read but not yet accepted by `mix_audio`.
    pending: Vec<f32>,
    scratch: Vec<f32>,
}

impl State {
    fn select_audio_track(&mut self, track: usize) {
        let Some(player) = &mut self.player else { return };
        for i in 0..player.audio_tracks().len() {
            player.set_audio_track_enabled(i, i == track);
        }
        self.audio = player.audio_output(track);
        self.pending.clear();
    }

    /// The frame of video track 0 as an image, if it is new (`docs/features/godot.md#video`).
    fn new_frame(&mut self) -> Option<Gd<Image>> {
        let frame = self.player.as_ref()?.video_frame(0)?;
        if self.shown == Some(frame.index) {
            return None;
        }
        self.shown = Some(frame.index);
        let format = image_format(frame.format)?;
        let (w, h) = (frame.width as i32, frame.height as i32);
        let data = PackedByteArray::from(frame.data);
        let image = Image::create_from_data(w, h, false, format, &data);
        if image.is_none() {
            godot_error!("praxi: cannot make a {w}x{h} {format:?} image of {} bytes", data.len());
        }
        image
    }

    /// Decoded audio for `mix_audio`: the samples and their frame count
    /// (`docs/features/godot.md#audio`).
    fn audio_to_mix(&mut self) -> Option<(PackedFloat32Array, usize, usize)> {
        let out = self.audio.as_ref()?;
        let channels = out.channels().max(1);
        self.scratch.resize(READ_FRAMES * channels, 0.0);
        while self.pending.len() < MAX_PENDING_FRAMES * channels {
            let n = out.read(&mut self.scratch);
            if n == 0 {
                break;
            }
            self.pending.extend_from_slice(&self.scratch[..n]);
        }
        let frames = self.pending.len() / channels;
        (frames > 0).then(|| (PackedFloat32Array::from(&self.pending[..frames * channels]), frames, channels))
    }
}

impl VideoStreamPlaybackPraxi {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Opens `path`; on failure prints the error and stays not playing.
    pub fn open(&mut self, path: &str, formats: &PackedStringArray, hdr: bool, looping: bool, stream: Gd<VideoStreamPraxi>) {
        self.stream = Some(stream);
        match Self::open_player(path, formats, hdr) {
            Ok(mut player) => {
                player.set_looping(looping);
                // VideoStreamPlayer takes the texture once, when the stream is set, so it
                // exists from the start, black until the first frame, and is updated in place.
                let p = &player.tracks()[0].params;
                let image = image_format(p.format).and_then(|f| Image::create_empty(p.width as i32, p.height as i32, false, f));
                *self.texture.lock().unwrap_or_else(|e| e.into_inner()) = image.and_then(|i| ImageTexture::create_from_image(&i));
                let mut state = self.state();
                state.player = Some(player);
                state.select_audio_track(0);
            }
            Err(e) => {
                godot_error!("praxi: {path}: {e}");
            }
        }
    }

    fn open_player(path: &str, formats: &PackedStringArray, hdr: bool) -> Result<FilePlayer, String> {
        let accepted: Vec<TextureFormat> = if formats.is_empty() {
            device_formats()
        } else {
            formats
                .as_slice()
                .iter()
                .map(|f| TextureFormat::parse(&f.to_string()).ok_or_else(|| format!("unknown texture format {f}")))
                .collect::<Result<_, _>>()?
        };
        let accepted = order_formats(accepted.into_iter().filter(|f| image_format(*f).is_some()).collect(), hdr);
        let caps = Capabilities::new(accepted.clone()).max_audio_channels(2);
        let file = BufReader::with_capacity(256 * 1024, GodotFile::open(path)?);
        let player = Player::open(file, &caps).map_err(|e| e.to_string())?;
        if player.tracks().is_empty() {
            return Err(format!("no video variant in a format Godot can show here ({accepted:?})"));
        }
        Ok(player)
    }

    pub fn set_looping(&self, looping: bool) {
        if let Some(p) = &mut self.state().player {
            p.set_looping(looping);
        }
    }

    pub fn active_cues(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        if let Some(p) = &self.state().player {
            for cue in p.active_cues() {
                out.push(&cue_dictionary(cue));
            }
        }
        out
    }

    pub fn upcoming_cues(&self, within: f64) -> Array<VarDictionary> {
        let mut out = Array::new();
        if let Some(p) = &self.state().player {
            for (until, cue) in p.upcoming_cues(Duration::from_secs_f64(within.max(0.0))) {
                let mut d = cue_dictionary(cue);
                d.set("until", until.as_secs_f64());
                out.push(&d);
            }
        }
        out
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
        let Some(p) = &mut self.state().player else { return };
        if let Err(e) = p.play() {
            godot_error!("praxi: {e}");
            return;
        }
        self.playing.store(true, Ordering::Relaxed);
        self.paused.store(false, Ordering::Relaxed);
    }

    fn stop(&mut self) {
        let mut state = self.state();
        let Some(p) = &mut state.player else { return };
        p.pause();
        if let Err(e) = p.seek(Duration::ZERO) {
            godot_error!("praxi: {e}");
        }
        state.pending.clear();
        self.playing.store(false, Ordering::Relaxed);
        self.paused.store(false, Ordering::Relaxed);
    }

    fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    fn set_paused(&mut self, paused: bool) {
        let Some(p) = &mut self.state().player else { return };
        if paused {
            p.pause();
        } else if self.playing.load(Ordering::Relaxed)
            && let Err(e) = p.play()
        {
            godot_error!("praxi: {e}");
        }
        self.paused.store(paused, Ordering::Relaxed);
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    fn get_length(&self) -> f64 {
        self.state().player.as_ref().map_or(0.0, |p| p.duration().as_secs_f64())
    }

    fn get_playback_position(&self) -> f64 {
        self.state().player.as_ref().map_or(0.0, |p| p.position().as_secs_f64())
    }

    fn seek(&mut self, time: f64) {
        let mut state = self.state();
        let Some(p) = &mut state.player else { return };
        let to = Duration::from_secs_f64(time.clamp(0.0, p.duration().as_secs_f64()));
        if let Err(e) = p.seek(to) {
            godot_error!("praxi: {e}");
        }
        state.pending.clear();
    }

    fn set_audio_track(&mut self, idx: i32) {
        self.state().select_audio_track(idx.max(0) as usize);
    }

    fn get_texture(&self) -> Option<Gd<Texture2D>> {
        self.texture.lock().unwrap_or_else(|e| e.into_inner()).clone().map(Gd::upcast)
    }

    fn update(&mut self, delta: f64) {
        let (state, texture, stream) = (self.state.clone(), self.texture.clone(), self.stream.clone());
        let lock = || state.lock().unwrap_or_else(|e| e.into_inner());
        // Released for the audio thread and for calls back into this playback.
        let mut base = self.base_mut();
        let mut signals = Vec::new();
        let (finished, image, audio) = {
            let mut s = lock();
            let Some(p) = &mut s.player else { return };
            if let Err(e) = p.advance(Duration::from_secs_f64(delta.max(0.0))) {
                godot_error!("praxi: {e}");
                drop(s);
                drop(base);
                self.playing.store(false, Ordering::Relaxed);
                return;
            }
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
            let finished = p.is_finished();
            (finished, s.new_frame(), s.audio_to_mix())
        };
        if let Some(image) = image {
            let current = texture.lock().unwrap_or_else(|e| e.into_inner()).clone();
            match current {
                Some(mut t) if t.get_width() == image.get_width() && t.get_height() == image.get_height() && t.get_format() == image.get_format() => {
                    t.update(&image)
                }
                _ => *texture.lock().unwrap_or_else(|e| e.into_inner()) = ImageTexture::create_from_image(&image),
            }
        }
        if let Some((buffer, frames, channels)) = audio {
            let mixed = base.mix_audio_ex(frames as i32).buffer(&buffer).offset(0).done();
            if mixed > 0 {
                let mut s = lock();
                let n = (mixed as usize * channels).min(s.pending.len());
                s.pending.drain(..n);
            }
        }
        if let Some(stream) = stream {
            for signal in signals {
                match signal {
                    Signal::Cue { id, kind, fields, time, duration, position } => {
                        stream.signals().cue().emit(id, &kind, &fields, time, duration, &position)
                    }
                    Signal::CueEnded(id) => stream.signals().cue_ended().emit(id),
                    Signal::Looped => stream.signals().looped().emit(),
                }
            }
        }
        drop(base);
        if finished {
            self.playing.store(false, Ordering::Relaxed);
        }
    }

    fn get_channels(&self) -> i32 {
        self.state().audio.as_ref().map_or(0, |a| a.channels() as i32)
    }

    fn get_mix_rate(&self) -> i32 {
        self.state().audio.as_ref().map_or(0, |a| a.sample_rate() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr_formats_come_first_with_hdr() {
        use TextureFormat::*;
        let formats = vec![Bc7, Bc6h, Bc1, Astc4x4Hdr, Astc4x4];
        assert_eq!(order_formats(formats.clone(), true), [Bc6h, Astc4x4Hdr, Bc7, Bc1, Astc4x4]);
        assert_eq!(order_formats(formats, false), [Bc7, Bc1, Astc4x4, Bc6h, Astc4x4Hdr]);
        assert!(image_format(Astc6x6Hdr).is_none(), "Godot has no ASTC 6x6");
    }
}
