//! `praxi-editor`: edits the cues of a `.praxi` file on a timeline
//! (`docs/features/cue-editor.md`).

#[cfg(feature = "audio")]
use praxi_audio as audio;
mod doc;
mod file;
mod inspector;
mod timeline;
mod video;
mod waveform;

use clap::Parser;
use doc::{Document, EditCue, Stream};
use file::SharedFile;
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use inspector::{CueDraft, valid_stream_name};
use praxi::format::{Cue, Reader, TimeBase, cue_sheet};
use praxi::{Capabilities, CueEvent, CueTrack, Event, Player, TextureFormat, Value};
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use timeline::{Action, Timeline, View, clock};
use video::VideoView;
use waveform::Waveform;

type FilePlayer = Player<BufReader<SharedFile>>;

/// Player events kept in the log.
const MAX_EVENTS: usize = 200;

#[derive(Parser)]
#[command(name = "praxi-editor", version, about = "Edits the cues of a praxi file")]
struct Args {
    file: PathBuf,
    /// Cue sheet to edit (default: FILE with the extension .cues.json).
    sheet: Option<PathBuf>,
    /// Only accept this texture format.
    #[arg(long, value_parser = parse_format)]
    format: Option<TextureFormat>,
    /// Do not open an audio device.
    #[arg(long)]
    no_audio: bool,
}

fn parse_format(s: &str) -> Result<TextureFormat, String> {
    TextureFormat::parse(s).ok_or_else(|| format!("unknown format {s:?}"))
}

/// `intro.praxi` → `intro.cues.json`.
fn default_sheet(file: &Path) -> PathBuf {
    file.with_extension("cues.json")
}

fn main() -> ExitCode {
    let args = Args::parse();
    let sheet = args.sheet.clone().unwrap_or_else(|| default_sheet(&args.file));
    // Read the cues first, so a bad sheet fails before a window opens.
    let tracks = match load_tracks(&args.file, &sheet) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 860.0]).with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        let base = setup.device_descriptor.clone();
        setup.device_descriptor = Arc::new(move |adapter| {
            let base = base(adapter);
            wgpu::DeviceDescriptor {
                required_features: praxi_wgpu::required_features(adapter),
                required_limits: praxi_wgpu::required_limits(adapter, base.required_limits.clone()),
                ..base
            }
        });
    }
    let mut error = None;
    let result = eframe::run_native(
        "praxi-editor",
        options,
        Box::new(|cc| match Editor::new(cc, &args, sheet, tracks) {
            Ok(editor) => Ok(Box::new(editor) as Box<dyn eframe::App>),
            Err(e) => {
                error = Some(e.clone());
                Err(e.into())
            }
        }),
    );
    match (result, error) {
        (_, Some(e)) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
        (Err(e), None) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
        (Ok(()), None) => ExitCode::SUCCESS,
    }
}

/// The sheet's tracks if it exists, otherwise the file's cues.
fn load_tracks(file: &Path, sheet: &Path) -> Result<Vec<CueTrack>, String> {
    if sheet.exists() {
        let text = std::fs::read_to_string(sheet).map_err(|e| format!("{}: {e}", sheet.display()))?;
        return cue_sheet::parse(&text).map_err(|e| format!("{}: {e}", sheet.display()));
    }
    let mut reader = Reader::new(BufReader::new(File::open(file).map_err(|e| format!("{}: {e}", file.display()))?))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    praxi::format::read_cues(&mut reader).map_err(|e| format!("{}: {e}", file.display()))
}

fn describe_event(event: &Event) -> String {
    match event {
        Event::Cue(cue) => format!("cue {} {}", cue.kind, fields_text(&cue.fields)),
        Event::CueEnd { id, .. } => format!("end #{}", id & !(1 << 63)),
        Event::Seeked { to } => format!("seeked to {}", clock(to.as_micros() as u64)),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn fields_text(fields: &[(String, Value)]) -> String {
    let mut s = String::new();
    for (k, v) in fields {
        let v = match v {
            Value::String(v) => format!("{v:?}"),
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format!("{v:?}"),
            Value::Bool(v) => v.to_string(),
            Value::Bytes(v) => format!("<{} bytes>", v.len()),
        };
        s += &format!("{k}={v} ");
    }
    s
}

struct Editor {
    file: PathBuf,
    sheet: PathBuf,
    player: FilePlayer,
    /// The player's file, closed before "Write into file".
    player_file: SharedFile,
    caps: Capabilities,
    video: VideoView,
    waveform: Option<Waveform>,
    /// Time base of video track 0, for frame stepping and snapping.
    frame_base: TimeBase,
    doc: Document,
    selected_cue: Option<u64>,
    selected_stream: Option<u64>,
    cue_draft: Option<CueDraft>,
    /// The selected stream's uid and name being typed.
    name_draft: Option<(u64, String)>,
    timeline: Timeline,
    snap: bool,
    events: VecDeque<(Duration, String)>,
    active: Vec<CueEvent>,
    status: String,
    muted: Arc<AtomicBool>,
    #[cfg(feature = "audio")]
    audio_device: Option<audio::Device>,
    #[cfg(feature = "audio")]
    audio: Option<audio::Stream>,
    last: Instant,
    confirm_close: bool,
    allow_close: bool,
}

impl Editor {
    fn new(cc: &eframe::CreationContext, args: &Args, sheet: PathBuf, tracks: Vec<CueTrack>) -> Result<Editor, String> {
        let state = cc.wgpu_render_state.as_ref().ok_or("no wgpu renderer")?;
        let caps = praxi_wgpu::capabilities(&state.adapter, args.format)?;
        #[cfg(feature = "audio")]
        let device_audio = if args.no_audio { None } else { audio::Device::open_default() };
        #[cfg(feature = "audio")]
        let caps = match device_audio.as_ref().and_then(|d| d.channels()) {
            Some(channels) => caps.max_audio_channels(channels),
            None => caps,
        };
        let (mut player, player_file) = open_player(&args.file, &caps)?;
        let streams = Reader::new(BufReader::new(File::open(&args.file).map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?.streams().to_vec();
        let frame_base = streams[player.tracks()[0].stream as usize].time_base;
        let waveform = match player.audio_tracks().first() {
            Some(t) => Some(Waveform::load(&args.file, t.stream).map_err(|e| format!("audio waveform: {e}"))?),
            None => None,
        };
        let video = VideoView::new(state, &player);
        let doc = Document::new(tracks);
        player.replace_cues(doc.tracks());
        player.seek(Duration::ZERO).map_err(|e| e.to_string())?;
        let muted = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "audio")]
        let audio = device_audio.as_ref().and_then(|device| start_audio(device, &player, &muted));
        let status = if sheet.exists() { format!("Opened {}", sheet.display()) } else { format!("Cues from the file; Save creates {}", sheet.display()) };
        let selected_stream = doc.streams().first().map(|s| s.uid);
        Ok(Editor {
            file: args.file.clone(),
            sheet,
            player,
            player_file,
            caps,
            video,
            waveform,
            frame_base,
            doc,
            selected_cue: None,
            selected_stream,
            cue_draft: None,
            name_draft: None,
            timeline: Timeline::default(),
            snap: true,
            events: VecDeque::new(),
            active: Vec::new(),
            status,
            muted,
            #[cfg(feature = "audio")]
            audio_device: device_audio,
            #[cfg(feature = "audio")]
            audio,
            last: Instant::now(),
            confirm_close: false,
            allow_close: false,
        })
    }

    // --- Document changes ---------------------------------------------------------------

    /// Applies an edit as one undo step and plays the new cues.
    fn edit(&mut self, f: impl FnOnce(&mut Vec<Stream>)) {
        if self.doc.edit(f) {
            self.cues_changed();
        }
    }

    fn cues_changed(&mut self) {
        self.player.replace_cues(self.doc.tracks());
        if self.selected_cue.is_some_and(|uid| self.doc.find(uid).is_none()) {
            self.selected_cue = None;
        }
        if self.selected_stream.is_some_and(|uid| self.doc.stream_index(uid).is_none()) {
            self.selected_stream = self.doc.streams().first().map(|s| s.uid);
        }
    }

    fn undo(&mut self) {
        if self.doc.undo() {
            self.cues_changed();
        }
    }

    fn redo(&mut self) {
        if self.doc.redo() {
            self.cues_changed();
        }
    }

    /// Replaces cue `uid` by `cue` in stream `stream`.
    fn set_cue(&mut self, stream: usize, cue: EditCue) {
        self.edit(|streams| {
            for s in streams.iter_mut() {
                s.cues.retain(|c| c.uid != cue.uid);
            }
            streams[stream].cues.push(cue);
        });
    }

    /// The stream new cues go to: the selected one, else the first, else a new `cues`.
    fn target_stream(&mut self) -> usize {
        if let Some(i) = self.selected_stream.and_then(|uid| self.doc.stream_index(uid)) {
            return i;
        }
        if self.doc.streams().is_empty() {
            self.add_stream();
        }
        0
    }

    fn add_cue(&mut self, stream: usize, time_us: u64) {
        let uid = self.doc.new_uid();
        self.edit(|s| s[stream].cues.push(EditCue { uid, time_us, cue: Cue::new("cue") }));
        self.selected_cue = Some(uid);
        self.selected_stream = Some(self.doc.streams()[stream].uid);
    }

    fn add_stream(&mut self) {
        let uid = self.doc.new_uid();
        let n = self.doc.streams().len();
        let name = if n == 0 { "cues".to_string() } else { format!("stream{}", n + 1) };
        self.edit(|s| s.push(Stream { uid, name, cues: Vec::new() }));
        self.selected_stream = Some(uid);
        self.selected_cue = None;
    }

    fn delete_selected(&mut self) {
        if let Some(uid) = self.selected_cue.take() {
            self.edit(|s| s.iter_mut().for_each(|s| s.cues.retain(|c| c.uid != uid)));
        }
    }

    fn duplicate_selected(&mut self) {
        let Some((s, c)) = self.selected_cue.and_then(|uid| self.doc.find(uid)) else { return };
        let mut copy = self.doc.streams()[s].cues[c].clone();
        copy.uid = self.doc.new_uid();
        copy.time_us = self.player.position().as_micros() as u64;
        self.selected_cue = Some(copy.uid);
        self.edit(|streams| streams[s].cues.push(copy));
    }

    /// Sets the selected cue's start (`start = true`) or end to the playhead.
    fn set_edge(&mut self, start: bool) {
        let Some((s, c)) = self.selected_cue.and_then(|uid| self.doc.find(uid)) else { return };
        let mut cue = self.doc.streams()[s].cues[c].clone();
        let at = self.player.position().as_micros() as u64;
        if start {
            let end = cue.end_us();
            cue.time_us = at.min(end);
            cue.cue.duration = end - cue.time_us;
        } else {
            cue.cue.duration = at.saturating_sub(cue.time_us);
        }
        self.set_cue(s, cue);
    }

    // --- Files -------------------------------------------------------------------------

    fn save(&mut self) -> bool {
        let result = cue_sheet::write(&self.doc.tracks()).and_then(|text| std::fs::write(&self.sheet, text).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                self.doc.mark_saved();
                self.status = format!("Saved {}", self.sheet.display());
                true
            }
            Err(e) => {
                self.status = format!("Save failed: {e}");
                false
            }
        }
    }

    fn write_into_file(&mut self) {
        // Windows cannot rename over a file that is open.
        self.player_file.close();
        let written = praxi::format::replace_cues_in_file(&self.file, &self.file, &self.doc.tracks());
        let reopened = self.reopen();
        self.status = match (written, reopened) {
            (Ok(()), Ok(())) => {
                self.doc.mark_saved();
                format!("Wrote the cues into {}", self.file.display())
            }
            (Err(e), Ok(())) => format!("Write failed: {e}"),
            (Ok(()), Err(e)) => {
                self.doc.mark_saved();
                format!("Wrote the cues, but reopening failed: {e}")
            }
            (Err(e), Err(r)) => format!("Write failed: {e}; reopening failed: {r}"),
        };
    }

    /// Opens the file again and puts playback back where it was.
    fn reopen(&mut self) -> Result<(), String> {
        let (at, playing, looping) = (self.player.position(), self.player.is_playing(), self.player.looping());
        let (player, player_file) = open_player(&self.file, &self.caps)?;
        #[cfg(feature = "audio")]
        {
            self.audio = None;
        }
        self.player = player;
        self.player_file = player_file;
        self.player.set_looping(looping);
        self.player.replace_cues(self.doc.tracks());
        self.active.clear();
        self.player.seek(at).map_err(|e| e.to_string())?;
        if playing {
            self.player.play().map_err(|e| e.to_string())?;
        }
        #[cfg(feature = "audio")]
        {
            self.audio = self.audio_device.as_ref().and_then(|d| start_audio(d, &self.player, &self.muted));
        }
        Ok(())
    }

    // --- Playback ------------------------------------------------------------------------

    fn report(&mut self, result: praxi::Result<()>) {
        if let Err(e) = result {
            self.status = format!("Playback error: {e}");
            self.player.pause();
        }
    }

    fn seek(&mut self, us: u64) {
        let r = self.player.seek(Duration::from_micros(us));
        self.report(r);
    }

    fn toggle_play(&mut self) {
        if self.player.is_playing() {
            self.player.pause();
        } else {
            let r = self.player.play();
            self.report(r);
        }
    }

    /// Steps `frames` video frames from the shown one, pausing.
    fn step(&mut self, frames: i64) {
        self.player.pause();
        let tb = self.frame_base;
        let shown = self.player.video_frame(0).map_or(0, |f| f.time.as_micros() as u64);
        let next = (frame_index(tb, shown) as i64 + frames).max(0) as u64;
        self.seek(tb.to_micros(next).min(self.player.duration().as_micros() as u64));
    }

    /// The start of the video frame nearest to `us`.
    fn snap_to_frame(tb: TimeBase, us: u64) -> u64 {
        tb.to_micros(frame_index(tb, us))
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now - self.last;
        self.last = now;
        #[cfg(feature = "audio")]
        if let Some(audio) = &self.audio {
            self.player.set_clock(praxi::Clock::Audio { track: 0, latency: audio.latency() });
        }
        let r = self.player.advance(dt);
        self.report(r);
        while let Some(event) = self.player.poll_event() {
            match &event {
                Event::Cue(c) if !c.duration.is_zero() => self.active.push(c.clone()),
                Event::CueEnd { id, .. } => self.active.retain(|c| c.id != *id),
                _ => {}
            }
            self.events.push_front((self.player.position(), describe_event(&event)));
            self.events.truncate(MAX_EVENTS);
        }
    }

    // --- Input ---------------------------------------------------------------------------

    fn keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let shortcut = |ctx: &egui::Context, m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        if shortcut(ctx, ctrl_shift, Key::S) {
            self.write_into_file();
        } else if shortcut(ctx, Modifiers::COMMAND, Key::S) {
            self.save();
        }
        if shortcut(ctx, ctrl_shift, Key::Z) || shortcut(ctx, Modifiers::COMMAND, Key::Y) {
            self.redo();
        } else if shortcut(ctx, Modifiers::COMMAND, Key::Z) {
            self.undo();
        }
        if shortcut(ctx, Modifiers::COMMAND, Key::D) {
            self.duplicate_selected();
        }
        let (pressed, shift) = ctx.input(|i| {
            let keys = [Key::Space, Key::ArrowLeft, Key::ArrowRight, Key::Home, Key::End, Key::C, Key::I, Key::O, Key::Delete];
            (keys.into_iter().filter(|k| i.key_pressed(*k) && !i.modifiers.command).collect::<Vec<_>>(), i.modifiers.shift)
        });
        for key in pressed {
            let pos = self.player.position().as_micros() as u64;
            match key {
                Key::Space => self.toggle_play(),
                Key::ArrowLeft if shift => self.seek(pos.saturating_sub(1_000_000)),
                Key::ArrowRight if shift => self.seek(pos + 1_000_000),
                Key::ArrowLeft => self.step(-1),
                Key::ArrowRight => self.step(1),
                Key::Home => self.seek(0),
                Key::End => self.seek(self.player.duration().as_micros() as u64),
                Key::C => {
                    let s = self.target_stream();
                    self.add_cue(s, pos);
                }
                Key::I => self.set_edge(true),
                Key::O => self.set_edge(false),
                Key::Delete => self.delete_selected(),
                _ => {}
            }
        }
    }

    fn handle(&mut self, action: Action) {
        match action {
            Action::Seek(us) => self.seek(us),
            Action::SelectCue(uid) => {
                self.selected_cue = Some(uid);
                self.selected_stream = self.doc.find(uid).map(|(s, _)| self.doc.streams()[s].uid);
            }
            Action::SelectStream(uid) => {
                self.selected_stream = Some(uid);
                self.selected_cue = None;
            }
            Action::Change { uid, time_us, duration } => {
                if let Some((s, c)) = self.doc.find(uid) {
                    let mut cue = self.doc.streams()[s].cues[c].clone();
                    cue.time_us = time_us;
                    cue.cue.duration = duration;
                    self.set_cue(s, cue);
                }
            }
            Action::Add { stream, time_us } => self.add_cue(stream, time_us),
        }
    }

    // --- Panels --------------------------------------------------------------------------

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Save").on_hover_text(format!("Write {} (Ctrl+S)", self.sheet.display())).clicked() {
                self.save();
            }
            if ui.button("Write into file").on_hover_text(format!("Replace the cues in {} (Ctrl+Shift+S)", self.file.display())).clicked() {
                self.write_into_file();
            }
            ui.separator();
            if ui.add_enabled(self.doc.can_undo(), egui::Button::new("Undo")).clicked() {
                self.undo();
            }
            if ui.add_enabled(self.doc.can_redo(), egui::Button::new("Redo")).clicked() {
                self.redo();
            }
            ui.separator();
            if ui.button("Add stream").clicked() {
                self.add_stream();
            }
            if ui.button("Add cue").on_hover_text("At the playhead (C)").clicked() {
                let s = self.target_stream();
                self.add_cue(s, self.player.position().as_micros() as u64);
            }
            ui.checkbox(&mut self.snap, "Snap to frames").on_hover_text("Hold Alt while dragging to place freely");
            ui.separator();
            let dirty = if self.doc.is_dirty() { "unsaved changes · " } else { "" };
            ui.label(format!("{dirty}{}", self.status));
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("|<").on_hover_text("Previous frame (Left)").clicked() {
                self.step(-1);
            }
            let label = if self.player.is_playing() { "Pause" } else { "Play" };
            if ui.button(label).on_hover_text("Space").clicked() {
                self.toggle_play();
            }
            if ui.button(">|").on_hover_text("Next frame (Right)").clicked() {
                self.step(1);
            }
            ui.monospace(format!("{} / {}", clock(self.player.position().as_micros() as u64), clock(self.player.duration().as_micros() as u64)));
            let mut looping = self.player.looping();
            if ui.checkbox(&mut looping, "Loop").changed() {
                self.player.set_looping(looping);
            }
            let mut muted = self.muted.load(Ordering::Relaxed);
            if ui.checkbox(&mut muted, "Mute").changed() {
                self.muted.store(muted, Ordering::Relaxed);
            }
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let names: Vec<String> = self.doc.streams().iter().map(|s| s.name.clone()).collect();
        match self.selected_cue.and_then(|uid| self.doc.find(uid)) {
            Some((s, c)) => {
                let cue = self.doc.streams()[s].cues[c].clone();
                if self.cue_draft.as_ref().is_none_or(|d| d.uid != cue.uid || d.base != (s, cue.clone())) {
                    self.cue_draft = Some(CueDraft::new(s, &cue));
                }
                ui.heading("Cue");
                let draft = self.cue_draft.as_mut().expect("draft");
                let done = draft.ui(ui, &names);
                if done && let Some((stream, new)) = draft.parse() {
                    self.set_cue(stream, new);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").on_hover_text("Delete").clicked() {
                        self.delete_selected();
                    }
                    if ui.button("Duplicate at playhead").on_hover_text("Ctrl+D").clicked() {
                        self.duplicate_selected();
                    }
                });
            }
            None => {
                self.cue_draft = None;
                if let Some(si) = self.selected_stream.and_then(|uid| self.doc.stream_index(uid)) {
                    self.stream_inspector(ui, si);
                } else {
                    ui.label("Select a cue or a stream on the timeline.");
                }
            }
        }

        ui.separator();
        ui.heading("Active cues");
        if self.active.is_empty() {
            ui.weak("None at the playhead");
        }
        for c in &self.active {
            let name = self.player.cue_streams().iter().find(|s| s.stream == c.stream).map_or("", |s| s.name.as_str());
            ui.label(format!("{name} · {} · {}", c.kind, fields_text(&c.fields)));
        }
        ui.separator();
        ui.heading("Events");
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (at, text) in &self.events {
                ui.monospace(format!("{} {text}", clock(at.as_micros() as u64)));
            }
        });
    }

    fn stream_inspector(&mut self, ui: &mut egui::Ui, si: usize) {
        let stream = &self.doc.streams()[si];
        let (uid, name, count) = (stream.uid, stream.name.clone(), stream.cues.len());
        if self.name_draft.as_ref().is_none_or(|(u, _)| *u != uid) {
            self.name_draft = Some((uid, name.clone()));
        }
        ui.heading("Stream");
        ui.label(format!("{count} cues"));
        let red = ui.visuals().error_fg_color;
        let draft = &mut self.name_draft.as_mut().expect("draft").1;
        let ok = valid_stream_name(draft);
        let edit = egui::TextEdit::singleline(draft).hint_text("name");
        let lost = ui.horizontal(|ui| {
            ui.label("Name");
            ui.add(if ok { edit } else { edit.text_color(red) }).lost_focus()
        });
        if !ok {
            ui.colored_label(red, "At most 32 bytes, no zero bytes.");
        }
        let new_name = draft.clone();
        if lost.inner && ok && new_name != name {
            self.edit(|s| s[si].name = new_name);
        }
        let last = self.doc.streams().len() - 1;
        ui.horizontal(|ui| {
            if ui.add_enabled(si > 0, egui::Button::new("Move up")).clicked() {
                self.edit(|s| s.swap(si - 1, si));
            }
            if ui.add_enabled(si < last, egui::Button::new("Move down")).clicked() {
                self.edit(|s| s.swap(si, si + 1));
            }
            if ui.button("Delete stream").clicked() {
                self.edit(|s| {
                    s.remove(si);
                });
            }
        });
    }

    fn video(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_rect_before_wrap();
        let (w, h) = self.video.size;
        let fit = praxi_wgpu::Rect::fit(w, h, avail.width().max(1.0) as u32, avail.height().max(1.0) as u32);
        let rect = egui::Rect::from_min_size(avail.min + egui::vec2(fit.x, fit.y), egui::vec2(fit.w, fit.h));
        ui.allocate_rect(avail, egui::Sense::hover());
        let painter = ui.painter_at(avail);
        painter.rect_filled(avail, 0.0, egui::Color32::from_gray(16));
        if self.player.video_frame(0).is_some() {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(self.video.texture, rect, uv, egui::Color32::WHITE);
        }
    }

    fn title(&self) -> String {
        let dirty = if self.doc.is_dirty() { "* " } else { "" };
        format!("{dirty}{} — {} — praxi-editor", self.file.display(), self.sheet.display())
    }

    fn close_dialog(&mut self, ctx: &egui::Context) {
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("close")).show(ctx, |ui| {
            ui.heading("Unsaved changes");
            ui.label(format!("Save the cues to {} before closing?", self.sheet.display()));
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    choice = Some(0);
                }
                if ui.button("Discard").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(2);
                }
            });
        });
        match choice {
            Some(0) if self.save() => self.close(ctx),
            Some(1) => self.close(ctx),
            Some(_) => self.confirm_close = false,
            None => {}
        }
    }

    fn close(&mut self, ctx: &egui::Context) {
        self.allow_close = true;
        self.confirm_close = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// The frame whose start is nearest to `us`. Frame starts are rounded down to whole
/// microseconds, so rounding here maps a frame's start back to its own index.
fn frame_index(tb: TimeBase, us: u64) -> u64 {
    let unit = tb.num as f64 * 1e6 / tb.den as f64;
    (us as f64 / unit).round() as u64
}

fn open_player(file: &Path, caps: &Capabilities) -> Result<(FilePlayer, SharedFile), String> {
    let shared = SharedFile::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let player = Player::open(BufReader::new(shared.clone()), caps).map_err(|e| format!("{}: {e}", file.display()))?;
    if player.tracks().is_empty() {
        return Err("the file has no video track this GPU can show".into());
    }
    Ok((player, shared))
}

/// Starts the audio stream for every audio track, converted to the device's format where
/// needed, or prints why not.
#[cfg(feature = "audio")]
fn start_audio(device: &audio::Device, player: &FilePlayer, muted: &Arc<AtomicBool>) -> Option<audio::Stream> {
    let tracks = player.audio_tracks();
    let outputs: Vec<_> = (0..tracks.len()).filter_map(|i| player.audio_output(i)).collect();
    if outputs.is_empty() {
        return None;
    }
    match device.start(outputs, muted.clone()) {
        Ok(stream) => {
            let (rate, channels) = stream.config();
            println!("audio device: {}, {rate} Hz, {channels} ch", device.name());
            for (i, t) in tracks.iter().enumerate() {
                if (t.params.sample_rate, t.params.channels) != (rate, channels) {
                    println!("audio {i}: converted from {} Hz {} ch", t.params.sample_rate, t.params.channels);
                }
            }
            Some(stream)
        }
        Err(e) => {
            eprintln!("audio device: {}: {e}; editing without sound", device.name());
            None
        }
    }
}

impl eframe::App for Editor {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && self.doc.is_dirty() && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
        self.tick();
        if !self.confirm_close {
            self.keys(&ctx);
        }
        if let Some(state) = frame.wgpu_render_state() {
            self.video.update(state, &mut self.player);
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("timeline").resizable(true).default_size(260.0).min_size(140.0).show(ui, |ui| {
            let snap_base = self.frame_base;
            let snap = move |t: u64| Editor::snap_to_frame(snap_base, t);
            let view = View {
                streams: self.doc.streams(),
                selected_cue: self.selected_cue,
                selected_stream: self.selected_stream,
                playhead: self.player.position().as_micros() as u64,
                duration: self.player.duration().as_micros() as u64,
                waveform: self.waveform.as_ref(),
                snap: if self.snap { Some(&snap) } else { None },
                follow: self.player.is_playing(),
            };
            let actions = self.timeline.show(ui, view);
            for a in actions {
                self.handle(a);
            }
        });
        egui::Panel::right("inspector").resizable(true).default_size(340.0).show(ui, |ui| {
            ui.add_space(4.0);
            self.inspector(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.transport(ui);
            self.video(ui);
        });
        if self.confirm_close {
            self.close_dialog(&ctx);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title()));
        if self.player.is_playing() {
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_index_inverts_frame_starts() {
        for tb in [TimeBase::new(1, 60), TimeBase::new(1001, 30000), TimeBase::new(1, 24)] {
            for n in 0..10_000 {
                assert_eq!(frame_index(tb, tb.to_micros(n)), n, "{tb:?} frame {n}");
            }
        }
        assert_eq!(Editor::snap_to_frame(TimeBase::new(1, 60), 2_000_010), 2_000_000);
        assert_eq!(default_sheet(Path::new("a/intro.praxi")), Path::new("a/intro.cues.json"));
    }
}
