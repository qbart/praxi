//! A simple cue editor in Bevy and egui (`docs/features/bevy.md#cue-editor-example`).
//!
//! `cargo run --release -p praxi-bevy --example cue_editor -- FILE [SHEET]`

use bevy::prelude::*;
use bevy::render::renderer::RenderAdapter;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, EguiTextureHandle, egui};
use clap::Parser;
use praxi_bevy::praxi::format::{Cue, Reader, cue_sheet};
use praxi_bevy::praxi::{CueTrack, Event, TimedCue, Value};
use praxi_bevy::{PraxiEvent, PraxiPlugin, PraxiVideo};
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(about = "A simple cue editor for praxi files")]
struct Args {
    file: PathBuf,
    /// Cue sheet (default: FILE with the extension .cues.json).
    sheet: Option<PathBuf>,
}

/// Player events shown in the inspector.
const MAX_EVENTS: usize = 20;
const RULER_H: f32 = 20.0;
const LANE_H: f32 = 26.0;

fn main() -> AppExit {
    let args = Args::parse();
    let sheet = args.sheet.clone().unwrap_or_else(|| args.file.with_extension("cues.json"));
    let tracks = match load(&args.file, &sheet) {
        Ok(tracks) => tracks,
        Err(e) => {
            eprintln!("{e}");
            return AppExit::error();
        }
    };
    let title = format!("praxi cues: {}", args.file.display());
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window { title, resolution: (1280, 800).into(), ..default() }),
            ..default()
        }))
        .add_plugins((EguiPlugin::default(), PraxiPlugin))
        .insert_resource(Editor::new(args.file, sheet, tracks))
        .add_systems(Startup, setup)
        .add_systems(Update, log_events)
        .add_systems(EguiPrimaryContextPass, ui)
        .run()
}

/// The cues of `sheet` if it exists, otherwise those stored in `file`.
fn load(file: &PathBuf, sheet: &PathBuf) -> Result<Vec<CueTrack>, String> {
    if sheet.exists() {
        let text = std::fs::read_to_string(sheet).map_err(|e| format!("{}: {e}", sheet.display()))?;
        return cue_sheet::parse(&text).map_err(|e| format!("{}: {e}", sheet.display()));
    }
    let f = File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut reader = Reader::new(BufReader::new(f)).map_err(|e| e.to_string())?;
    praxi_bevy::praxi::format::read_cues(&mut reader).map_err(|e| e.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Selection {
    None,
    Stream(usize),
    Cue(usize, usize),
}

/// Text being typed for the selected cue or stream, committed when a field loses focus.
struct Draft {
    of: Selection,
    start: String,
    duration: String,
    kind: String,
    name: String,
    fields: Vec<(String, String)>,
    new_key: String,
}

#[derive(Resource)]
struct Editor {
    file: PathBuf,
    sheet: PathBuf,
    tracks: Vec<CueTrack>,
    selected: Selection,
    draft: Option<Draft>,
    texture: Option<egui::TextureId>,
    status: String,
    events: VecDeque<String>,
    /// Microseconds per frame of video track 0.
    frame_us: u64,
    /// The video's size, for letterboxing.
    video_size: egui::Vec2,
    dirty: bool,
}

impl Editor {
    fn new(file: PathBuf, sheet: PathBuf, tracks: Vec<CueTrack>) -> Editor {
        let status = if sheet.exists() { format!("Opened {}", sheet.display()) } else { format!("Cues from the file; Save creates {}", sheet.display()) };
        let selected = if tracks.is_empty() { Selection::None } else { Selection::Stream(0) };
        Editor {
            file,
            sheet,
            tracks,
            selected,
            draft: None,
            texture: None,
            status,
            events: VecDeque::new(),
            frame_us: 1,
            video_size: egui::vec2(16.0, 9.0),
            dirty: false,
        }
    }

    /// Sorts the cues of every stream by time (stable), keeps the selection on the same
    /// cue and plays the new cues.
    fn changed(&mut self, video: &mut PraxiVideo) {
        for (s, track) in self.tracks.iter_mut().enumerate() {
            let mut order: Vec<usize> = (0..track.cues.len()).collect();
            order.sort_by_key(|&i| track.cues[i].time_us);
            if let Selection::Cue(ss, c) = self.selected
                && ss == s
            {
                self.selected = Selection::Cue(s, order.iter().position(|&i| i == c).unwrap_or(0));
            }
            let cues = std::mem::take(&mut track.cues);
            let mut cues: Vec<Option<TimedCue>> = cues.into_iter().map(Some).collect();
            track.cues = order.iter().map(|&i| cues[i].take().expect("each cue once")).collect();
        }
        self.draft = None;
        self.dirty = true;
        video.player().replace_cues(self.tracks.clone());
    }

    fn save(&mut self) {
        let result = cue_sheet::write(&self.tracks).and_then(|text| std::fs::write(&self.sheet, text).map_err(|e| e.to_string()));
        self.status = match result {
            Ok(()) => {
                self.dirty = false;
                format!("Saved {}", self.sheet.display())
            }
            Err(e) => format!("Save failed: {e}"),
        };
    }

    fn write_into_file(&mut self, video: &mut PraxiVideo) {
        // Windows cannot rename over a file that is open.
        video.release_file();
        let written = praxi_bevy::praxi::format::replace_cues_in_file(&self.file, &self.file, &self.tracks);
        let reopened = video.reopen();
        video.player().replace_cues(self.tracks.clone());
        self.status = match (written, reopened) {
            (Ok(()), Ok(())) => {
                self.dirty = false;
                format!("Wrote the cues into {}", self.file.display())
            }
            (Err(e), _) => format!("Write failed: {e}"),
            (Ok(()), Err(e)) => format!("Wrote the cues, but reopening failed: {e}"),
        };
    }
}

fn setup(mut commands: Commands, adapter: Res<RenderAdapter>, mut images: ResMut<Assets<Image>>, mut editor: ResMut<Editor>) {
    commands.spawn(Camera2d);
    match PraxiVideo::open(&editor.file, &adapter, &mut images) {
        Ok(mut video) => {
            let track = video.player().tracks()[0];
            editor.video_size = egui::vec2(track.params.width as f32, track.params.height as f32);
            let f = File::open(&editor.file).ok().and_then(|f| Reader::new(BufReader::new(f)).ok());
            if let Some(reader) = f {
                editor.frame_us = reader.streams()[track.stream as usize].time_base.to_micros(1).max(1);
            }
            video.player().replace_cues(editor.tracks.clone());
            commands.spawn(video);
        }
        Err(e) => {
            eprintln!("{}: {e}", editor.file.display());
            commands.write_message(AppExit::error());
        }
    }
}

fn log_events(mut events: MessageReader<PraxiEvent>, mut editor: ResMut<Editor>) {
    for e in events.read() {
        let text = match &e.event {
            Event::Cue(c) => format!("{:.3} cue {} +{:.3}", c.time.as_secs_f64(), c.kind, c.duration.as_secs_f64()),
            other => format!("{other:?}"),
        };
        editor.events.push_front(text);
        editor.events.truncate(MAX_EVENTS);
    }
}

fn ui(mut contexts: EguiContexts, mut editor: ResMut<Editor>, video: Option<Single<&mut PraxiVideo>>, keys: Res<ButtonInput<KeyCode>>) -> Result {
    let Some(mut video) = video else { return Ok(()) };
    let video = &mut **video;
    let editor = &mut *editor;
    if editor.texture.is_none() {
        editor.texture = Some(contexts.add_image(EguiTextureHandle::Strong(video.image.clone())));
    }
    let ctx = contexts.ctx_mut()?.clone();
    if !ctx.egui_wants_keyboard_input() {
        keys_input(editor, video, &keys);
    }
    let mut root = egui::Ui::new(ctx.clone(), "root".into(), egui::UiBuilder::new().layer_id(egui::LayerId::background()).max_rect(ctx.viewport_rect()));
    egui::Panel::top("top").show(&mut root, |ui| top_bar(ui, editor, video));
    egui::Panel::bottom("timeline").exact_size(RULER_H + LANE_H * editor.tracks.len().max(1) as f32 + 16.0).show(&mut root, |ui| timeline(ui, editor, video));
    egui::Panel::right("inspector").default_size(320.0).show(&mut root, |ui| inspector(ui, editor, video));
    egui::CentralPanel::default().show(&mut root, |ui| {
        let avail = ui.available_size();
        let scale = (avail.x / editor.video_size.x).min(avail.y / editor.video_size.y);
        let size = editor.video_size * scale;
        ui.centered_and_justified(|ui| {
            if let Some(id) = editor.texture {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(id, size)));
            }
        });
    });
    Ok(())
}

fn keys_input(editor: &mut Editor, video: &mut PraxiVideo, keys: &ButtonInput<KeyCode>) {
    if keys.just_pressed(KeyCode::Space) {
        toggle_play(video);
    }
    if keys.just_pressed(KeyCode::ArrowLeft) {
        step(editor, video, -1);
    }
    if keys.just_pressed(KeyCode::ArrowRight) {
        step(editor, video, 1);
    }
    if keys.just_pressed(KeyCode::Delete) {
        delete_selected(editor, video);
    }
}

fn toggle_play(video: &mut PraxiVideo) {
    let player = video.player();
    if player.is_playing() {
        player.pause();
    } else {
        if player.is_finished() {
            let _ = player.seek(Duration::ZERO);
        }
        let _ = player.play();
    }
}

fn step(editor: &mut Editor, video: &mut PraxiVideo, frames: i64) {
    let player = video.player();
    player.pause();
    let at = player.position().as_micros() as i64 + frames * editor.frame_us as i64;
    let _ = player.seek(Duration::from_micros(at.max(0) as u64));
}

fn delete_selected(editor: &mut Editor, video: &mut PraxiVideo) {
    if let Selection::Cue(s, c) = editor.selected {
        editor.tracks[s].cues.remove(c);
        editor.selected = Selection::Stream(s);
        editor.changed(video);
    }
}

fn top_bar(ui: &mut egui::Ui, editor: &mut Editor, video: &mut PraxiVideo) {
    ui.horizontal(|ui| {
        let playing = video.player().is_playing();
        if ui.button(if playing { "Pause" } else { "Play" }).clicked() {
            toggle_play(video);
        }
        if ui.button("<").on_hover_text("Previous frame").clicked() {
            step(editor, video, -1);
        }
        if ui.button(">").on_hover_text("Next frame").clicked() {
            step(editor, video, 1);
        }
        let player = video.player();
        ui.monospace(format!("{:8.3} / {:.3} s", player.position().as_secs_f64(), player.duration().as_secs_f64()));
        ui.separator();
        if ui.button("Save").on_hover_text(editor.sheet.display().to_string()).clicked() {
            editor.save();
        }
        if ui.button("Write into file").on_hover_text(editor.file.display().to_string()).clicked() {
            editor.write_into_file(video);
        }
        ui.separator();
        ui.label(format!("{}{}", if editor.dirty { "* " } else { "" }, editor.status));
    });
}

fn timeline(ui: &mut egui::Ui, editor: &mut Editor, video: &mut PraxiVideo) {
    let duration_us = video.player().duration().as_micros().max(1) as f32;
    let lanes = editor.tracks.len().max(1);
    let size = egui::vec2(ui.available_width(), RULER_H + LANE_H * lanes as f32);
    let (response, painter) = ui.allocate_painter(size, egui::Sense::click());
    let rect = response.rect;
    let x_of = |us: u64| rect.left() + us as f32 / duration_us * rect.width();
    let us_of = |x: f32| (((x - rect.left()) / rect.width()).clamp(0.0, 1.0) * duration_us) as u64;
    let visuals = ui.visuals().clone();
    painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
    // Ruler: a tick every second.
    let seconds = (duration_us / 1e6).ceil() as u64;
    let every = (seconds / 20).max(1);
    for s in (0..=seconds).step_by(every as usize) {
        let x = x_of(s * 1_000_000);
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.top() + 6.0)], visuals.widgets.noninteractive.fg_stroke);
        painter.text(egui::pos2(x + 2.0, rect.top() + 4.0), egui::Align2::LEFT_TOP, format!("{s}s"), egui::FontId::monospace(10.0), visuals.text_color());
    }
    let lane_rect = |s: usize| egui::Rect::from_min_size(egui::pos2(rect.left(), rect.top() + RULER_H + LANE_H * s as f32), egui::vec2(rect.width(), LANE_H));
    let mut cue_rects = Vec::new();
    for (s, track) in editor.tracks.iter().enumerate() {
        let lane = lane_rect(s);
        if editor.selected == Selection::Stream(s) {
            painter.rect_filled(lane, 0.0, visuals.faint_bg_color);
        }
        painter.text(lane.left_top() + egui::vec2(4.0, 2.0), egui::Align2::LEFT_TOP, &track.name, egui::FontId::proportional(11.0), visuals.weak_text_color());
        for (c, cue) in track.cues.iter().enumerate() {
            let x0 = x_of(cue.time_us);
            let x1 = x_of(cue.time_us + cue.cue.duration).max(x0 + 3.0);
            let r = egui::Rect::from_min_max(egui::pos2(x0, lane.top() + 12.0), egui::pos2(x1, lane.bottom() - 2.0));
            let selected = editor.selected == Selection::Cue(s, c);
            let color = if selected { visuals.selection.bg_fill } else { visuals.widgets.inactive.bg_fill };
            painter.rect_filled(r, 2.0, color);
            painter.text(r.left_top() + egui::vec2(2.0, 0.0), egui::Align2::LEFT_TOP, &cue.cue.kind, egui::FontId::proportional(10.0), visuals.strong_text_color());
            cue_rects.push((r, s, c));
        }
    }
    let playhead = x_of(video.player().position().as_micros() as u64);
    painter.line_segment([egui::pos2(playhead, rect.top()), egui::pos2(playhead, rect.bottom())], egui::Stroke::new(1.5, egui::Color32::RED));
    if let Some(pos) = response.interact_pointer_pos().filter(|_| response.clicked()) {
        if pos.y < rect.top() + RULER_H {
            let _ = video.player().seek(Duration::from_micros(us_of(pos.x)));
        } else if let Some(&(_, s, c)) = cue_rects.iter().rev().find(|(r, _, _)| r.expand(2.0).contains(pos)) {
            editor.selected = Selection::Cue(s, c);
        } else {
            let s = ((pos.y - rect.top() - RULER_H) / LANE_H) as usize;
            if s < editor.tracks.len() {
                editor.selected = Selection::Stream(s);
            }
        }
    }
}

fn draft_for(editor: &Editor) -> Draft {
    let mut d = Draft {
        of: editor.selected,
        start: String::new(),
        duration: String::new(),
        kind: String::new(),
        name: String::new(),
        fields: Vec::new(),
        new_key: String::new(),
    };
    match editor.selected {
        Selection::Cue(s, c) => {
            let cue = &editor.tracks[s].cues[c];
            d.start = format!("{:.6}", cue.time_us as f64 / 1e6);
            d.duration = format!("{:.6}", cue.cue.duration as f64 / 1e6);
            d.kind = cue.cue.kind.clone();
            d.fields = cue
                .cue
                .fields
                .iter()
                .map(|(k, v)| match v {
                    Value::String(s) => (k.clone(), s.clone()),
                    other => (k.clone(), format!("{other:?}")),
                })
                .collect();
        }
        Selection::Stream(s) => d.name = editor.tracks[s].name.clone(),
        Selection::None => {}
    }
    d
}

fn seconds(text: &str) -> Option<u64> {
    let v: f64 = text.trim().parse().ok()?;
    (v.is_finite() && v >= 0.0).then(|| (v * 1e6).round() as u64)
}

fn inspector(ui: &mut egui::Ui, editor: &mut Editor, video: &mut PraxiVideo) {
    if editor.draft.as_ref().is_none_or(|d| d.of != editor.selected) {
        editor.draft = Some(draft_for(editor));
    }
    let mut draft = editor.draft.take().expect("draft");
    let playhead = video.player().position().as_micros() as u64;
    let mut commit = false;
    match editor.selected {
        Selection::Cue(s, c) => {
            ui.heading(format!("Cue in {}", editor.tracks[s].name));
            egui::Grid::new("cue").num_columns(2).show(ui, |ui| {
                for (label, text) in [("Start (s)", &mut draft.start), ("Duration (s)", &mut draft.duration), ("Type", &mut draft.kind)] {
                    ui.label(label);
                    commit |= ui.text_edit_singleline(text).lost_focus();
                    ui.end_row();
                }
            });
            ui.separator();
            ui.label("Fields");
            let cue = &editor.tracks[s].cues[c].cue;
            let mut remove = None;
            for (i, (key, value)) in draft.fields.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let string = matches!(cue.fields.get(i), Some((_, Value::String(_))));
                    ui.add_sized([90.0, 18.0], egui::Label::new(key.as_str()));
                    if string {
                        commit |= ui.text_edit_singleline(value).lost_focus();
                    } else {
                        ui.label(value.as_str());
                    }
                    if ui.small_button("x").clicked() {
                        remove = Some(i);
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut draft.new_key).hint_text("new key").desired_width(120.0));
                if ui.button("Add field").clicked() && !draft.new_key.is_empty() {
                    draft.fields.push((std::mem::take(&mut draft.new_key), String::new()));
                    commit = true;
                }
            });
            if let Some(i) = remove {
                draft.fields.remove(i);
                commit = true;
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Set start to playhead").clicked() {
                    draft.start = format!("{:.6}", playhead as f64 / 1e6);
                    commit = true;
                }
                if ui.button("Delete").clicked() {
                    editor.draft = None;
                    delete_selected(editor, video);
                    commit = false;
                    draft.of = Selection::None;
                }
            });
            if commit {
                commit_cue(editor, video, &draft, s, c);
            }
        }
        Selection::Stream(s) => {
            ui.heading("Stream");
            ui.horizontal(|ui| {
                ui.label("Name");
                if ui.text_edit_singleline(&mut draft.name).lost_focus() && draft.name != editor.tracks[s].name {
                    if draft.name.len() <= 32 {
                        editor.tracks[s].name = draft.name.clone();
                        editor.changed(video);
                    } else {
                        editor.status = "Stream names have at most 32 bytes".into();
                    }
                }
            });
            if ui.button("Add cue at playhead").clicked() {
                editor.tracks[s].cues.push(TimedCue { time_us: playhead, cue: Cue::new("cue") });
                let c = editor.tracks[s].cues.len() - 1;
                editor.selected = Selection::Cue(s, c);
                editor.changed(video);
            }
        }
        Selection::None => {
            ui.label("Select a cue or a stream on the timeline.");
        }
    }
    if editor.draft.is_none() && draft.of == editor.selected {
        editor.draft = Some(draft);
    }
    if ui.button("Add stream").clicked() {
        editor.tracks.push(CueTrack { name: format!("stream{}", editor.tracks.len()), cues: Vec::new() });
        editor.selected = Selection::Stream(editor.tracks.len() - 1);
        editor.changed(video);
    }
    ui.separator();
    ui.label("Events");
    for e in &editor.events {
        ui.monospace(e);
    }
}

/// Applies the draft to cue `c` of stream `s` if it is valid; reports why not otherwise.
fn commit_cue(editor: &mut Editor, video: &mut PraxiVideo, draft: &Draft, s: usize, c: usize) {
    let (Some(start), Some(duration)) = (seconds(&draft.start), seconds(&draft.duration)) else {
        editor.status = "Start and duration are seconds, 0 or more".into();
        return;
    };
    let old = &editor.tracks[s].cues[c].cue;
    let mut fields = Vec::new();
    for (i, (key, value)) in draft.fields.iter().enumerate() {
        // Fields of other types keep their values; new and string fields are strings.
        let kept = old.fields.iter().find(|(k, v)| k == key && !matches!(v, Value::String(_)) && i < old.fields.len());
        fields.push((key.clone(), kept.map_or_else(|| Value::String(value.clone()), |(_, v)| v.clone())));
    }
    let cue = Cue { duration, kind: draft.kind.clone(), fields };
    if let Err(e) = cue.encode(&mut Vec::new()) {
        editor.status = format!("Not applied: {e}");
        return;
    }
    if editor.tracks[s].cues[c] != (TimedCue { time_us: start, cue: cue.clone() }) {
        editor.tracks[s].cues[c] = TimedCue { time_us: start, cue };
        editor.changed(video);
    }
}
