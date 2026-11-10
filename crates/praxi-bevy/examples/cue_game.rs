//! A cutscene that consumes its cues: subtitles, speech bubbles, camera shake, markers and
//! sounds announced ahead (`docs/features/cue-runtime.md#example-game`).
//!
//! `cargo run --release -p praxi-bevy --example cue_game -- FILE [SHEET]`

use bevy::prelude::*;
use bevy::render::renderer::RenderAdapter;
use bevy::sprite::Anchor;
use clap::Parser;
use praxi_bevy::praxi::format::cue_sheet;
use praxi_bevy::praxi::{CueEvent, Event};
use praxi_bevy::{PraxiCue, PraxiCuePlugin, PraxiEvent, PraxiPlugin, PraxiSystems, PraxiVideo, position_on_sprite};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(about = "A cutscene that consumes the cues of a praxi file")]
struct Args {
    file: PathBuf,
    /// Cue sheet played in place of the file's cues.
    sheet: Option<PathBuf>,
}

/// The cue types this game knows; others are ignored.
#[derive(Deserialize, Debug, Clone)]
enum GameCue {
    #[serde(rename = "subtitle")]
    Subtitle { text: String, speaker: Option<String> },
    #[serde(rename = "camera.shake")]
    Shake {
        strength: f32,
        #[serde(default = "default_frequency")]
        frequency: f32,
    },
    #[serde(rename = "marker")]
    Marker { label: String },
    #[serde(rename = "sound")]
    Sound { name: String },
}

fn default_frequency() -> f32 {
    12.0
}

/// How long an instantaneous marker stays on screen.
const INSTANT_MARKER: Duration = Duration::from_secs(1);
/// How far ahead sounds are announced.
const LOOKAHEAD: Duration = Duration::from_secs(2);
const MARKER_COLOR: Color = Color::srgb(1.0, 0.63, 0.42);
const BUBBLE_COLOR: Color = Color::srgba(0.05, 0.05, 0.08, 0.85);

fn main() -> AppExit {
    let args = Args::parse();
    let tracks = match &args.sheet {
        Some(sheet) => match std::fs::read_to_string(sheet).map_err(|e| e.to_string()).and_then(|t| cue_sheet::parse(&t).map_err(|e| e.to_string())) {
            Ok(tracks) => Some(tracks),
            Err(e) => {
                eprintln!("{}: {e}", sheet.display());
                return AppExit::error();
            }
        },
        None => None,
    };
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window { title: format!("praxi cue game: {}", args.file.display()), resolution: (1280, 720).into(), ..default() }),
            ..default()
        }))
        .add_plugins((PraxiPlugin, PraxiCuePlugin::<GameCue>::default()))
        .insert_resource(ClearColor(Color::BLACK))
        .insert_resource(Game { file: args.file, tracks, ..default() })
        .add_systems(Startup, setup)
        .add_systems(Update, (input, fit_video).before(PraxiSystems::Advance))
        .add_systems(Update, (react, shake_camera, overlays, hud).chain().after(PraxiSystems::Cues))
        .run()
}

#[derive(Resource, Default)]
struct Game {
    file: PathBuf,
    tracks: Option<Vec<praxi_bevy::praxi::CueTrack>>,
    video_size: Vec2,
    /// Active shakes by cue id: strength, frequency, start, duration (in video time).
    shakes: HashMap<u64, (f32, f32, Duration, Duration)>,
    /// Instantaneous markers on screen: cue id, label, position, until (video time).
    flashes: Vec<(u64, String, (f32, f32), Duration)>,
    /// Sounds started, newest first.
    played: Vec<(String, Duration)>,
}

#[derive(Component)]
struct Hud;

#[derive(Component)]
struct Subtitle;

/// A label or bubble drawn for the cue with this id.
#[derive(Component)]
struct Overlay(u64);

fn setup(mut commands: Commands, adapter: Res<RenderAdapter>, mut images: ResMut<Assets<Image>>, mut game: ResMut<Game>) {
    commands.spawn(Camera2d);
    let mut video = match PraxiVideo::open(&game.file, &adapter, &mut images) {
        Ok(video) => video,
        Err(e) => {
            eprintln!("{}: {e}", game.file.display());
            commands.write_message(AppExit::error());
            return;
        }
    };
    let params = video.player().tracks()[0].params;
    game.video_size = Vec2::new(params.width as f32, params.height as f32);
    if let Some(tracks) = game.tracks.take() {
        video.player().replace_cues(tracks);
    }
    video.player().set_looping(true);
    if let Err(e) = video.player().play() {
        eprintln!("{e}");
    }
    commands.spawn((Sprite::from_image(video.image.clone()), video));

    let text = |size: f32| (TextFont { font_size: FontSize::Px(size), ..default() }, TextColor(Color::WHITE));
    commands.spawn((
        Hud,
        Text::default(),
        text(16.0),
        Node { position_type: PositionType::Absolute, top: px(10), left: px(12), ..default() },
    ));
    commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            bottom: px(40),
            width: percent(100),
            justify_content: JustifyContent::Center,
            ..default()
        })
        .with_children(|row| {
            row.spawn((
                Subtitle,
                Text::default(),
                text(30.0),
                Node { padding: UiRect::axes(px(14), px(6)), ..default() },
                BackgroundColor(Color::NONE),
            ));
        });
}

fn input(keys: Res<ButtonInput<KeyCode>>, mut videos: Query<&mut PraxiVideo>, mut exit: MessageWriter<AppExit>) {
    let Ok(mut video) = videos.single_mut() else { return };
    let player = video.player();
    let seek = |player: &mut praxi_bevy::FilePlayer, secs: f64| {
        let to = (player.position().as_secs_f64() + secs).clamp(0.0, player.duration().as_secs_f64());
        if let Err(e) = player.seek(Duration::from_secs_f64(to)) {
            eprintln!("{e}");
        }
    };
    if keys.just_pressed(KeyCode::Space) {
        if player.is_playing() {
            player.pause();
        } else if let Err(e) = player.play() {
            eprintln!("{e}");
        }
    }
    if keys.just_pressed(KeyCode::ArrowLeft) {
        seek(player, -2.0);
    }
    if keys.just_pressed(KeyCode::ArrowRight) {
        seek(player, 2.0);
    }
    if keys.just_pressed(KeyCode::KeyL) {
        let looping = !player.looping();
        player.set_looping(looping);
    }
    if keys.just_pressed(KeyCode::Escape) {
        exit.write(AppExit::Success);
    }
}

/// Letterboxes the video sprite in the window.
fn fit_video(windows: Query<&Window>, game: Res<Game>, mut sprites: Query<&mut Sprite, With<PraxiVideo>>) {
    let (Ok(window), Ok(mut sprite)) = (windows.single(), sprites.single_mut()) else { return };
    let scale = (window.width() / game.video_size.x).min(window.height() / game.video_size.y);
    let size = game.video_size * scale;
    if sprite.custom_size != Some(size) {
        sprite.custom_size = Some(size);
    }
}

/// Starts shakes, instantaneous markers and sounds from the typed cues, and ends shakes.
fn react(mut cues: MessageReader<PraxiCue<GameCue>>, mut events: MessageReader<PraxiEvent>, mut game: ResMut<Game>) {
    for c in cues.read() {
        match &c.cue {
            GameCue::Shake { strength, frequency } => {
                game.shakes.insert(c.id, (*strength, *frequency, c.time, c.duration));
            }
            GameCue::Marker { label } if c.duration.is_zero() => {
                if let Some(position) = c.position {
                    game.flashes.push((c.id, label.clone(), position, c.time + INSTANT_MARKER));
                }
            }
            GameCue::Sound { name } => {
                game.played.insert(0, (name.clone(), c.time));
                game.played.truncate(4);
            }
            GameCue::Subtitle { .. } | GameCue::Marker { .. } => {}
        }
    }
    for e in events.read() {
        match &e.event {
            Event::CueEnd { id, .. } => {
                game.shakes.remove(id);
            }
            Event::Seeked { .. } | Event::Looped => {
                game.flashes.clear();
                game.played.clear();
            }
            _ => {}
        }
    }
}

/// Offsets the camera by the active shakes, each fading out over its duration.
fn shake_camera(game: Res<Game>, mut videos: Query<&mut PraxiVideo>, mut cameras: Query<&mut Transform, With<Camera2d>>) {
    let (Ok(mut video), Ok(mut camera)) = (videos.single_mut(), cameras.single_mut()) else { return };
    let now = video.player().position();
    let mut offset = Vec2::ZERO;
    for &(strength, frequency, start, duration) in game.shakes.values() {
        let t = now.saturating_sub(start).as_secs_f32();
        let fade = 1.0 - (t / duration.as_secs_f32().max(1e-3)).min(1.0);
        let phase = t * frequency * std::f32::consts::TAU;
        offset += strength * fade * Vec2::new(phase.sin(), (phase * 1.3 + 1.0).cos());
    }
    camera.translation = offset.extend(camera.translation.z);
}

/// Subtitles, speech bubbles and markers from the active cues.
fn overlays(
    mut commands: Commands,
    mut game: ResMut<Game>,
    mut videos: Query<(&mut PraxiVideo, &Sprite, &GlobalTransform)>,
    mut subtitle: Query<(&mut Text, &mut BackgroundColor), With<Subtitle>>,
    mut existing: Query<(Entity, &Overlay, &mut Transform, &mut Text2d)>,
    mut gizmos: Gizmos,
) {
    let Ok((mut video, sprite, at)) = videos.single_mut() else { return };
    let size = sprite.custom_size.unwrap_or(game.video_size);
    let player = video.player();
    let now = player.position();
    game.flashes.retain(|f| f.3 > now);

    // Overlays wanted this frame: cue id, text, position, bubble or marker.
    let mut wanted: Vec<(u64, String, (f32, f32), bool)> = Vec::new();
    let mut lines = Vec::new();
    for c in player.active_cues() {
        match parse(c) {
            Some(GameCue::Subtitle { text, speaker }) => {
                let text = match speaker {
                    Some(s) => format!("{s}: {text}"),
                    None => text,
                };
                match c.position() {
                    Some(p) => wanted.push((c.id, text, p, true)),
                    None => lines.push(text),
                }
            }
            Some(GameCue::Marker { label }) => {
                if let Some(p) = c.position() {
                    wanted.push((c.id, label, p, false));
                }
            }
            _ => {}
        }
    }
    wanted.extend(game.flashes.iter().map(|(id, label, p, _)| (*id, label.clone(), *p, false)));

    if let Ok((mut text, mut background)) = subtitle.single_mut() {
        text.0 = lines.join("\n");
        background.0 = if lines.is_empty() { Color::NONE } else { BUBBLE_COLOR };
    }

    let mut shown = Vec::new();
    for (id, label, position, bubble) in &wanted {
        let point = position_on_sprite(*position, size, Vec2::ZERO, at);
        let at_label = if *bubble {
            point + Vec3::Z
        } else {
            gizmos.circle_2d(Isometry2d::from_translation(point.truncate()), 18.0, MARKER_COLOR);
            gizmos.circle_2d(Isometry2d::from_translation(point.truncate()), 4.0, MARKER_COLOR);
            point + Vec3::new(28.0, 0.0, 1.0)
        };
        // Spaces pad the bubble's background.
        let label = if *bubble { format!(" {label} ") } else { label.clone() };
        shown.push(*id);
        match existing.iter_mut().find(|(_, o, _, _)| o.0 == *id) {
            Some((_, _, mut transform, mut text)) => {
                transform.translation = at_label;
                if text.0 != label {
                    text.0 = label;
                }
            }
            None => {
                let (anchor, color, background) = match bubble {
                    true => (Anchor::CENTER, Color::WHITE, BUBBLE_COLOR),
                    false => (Anchor::CENTER_LEFT, MARKER_COLOR, Color::NONE),
                };
                commands.spawn((
                    Overlay(*id),
                    Text2d::new(label),
                    TextFont { font_size: FontSize::Px(26.0), ..default() },
                    TextColor(color),
                    TextBackgroundColor(background),
                    anchor,
                    Transform::from_translation(at_label),
                ));
            }
        }
    }
    for (entity, overlay, _, _) in &existing {
        if !shown.contains(&overlay.0) {
            commands.entity(entity).despawn();
        }
    }
}

fn parse(c: &CueEvent) -> Option<GameCue> {
    c.parse::<GameCue>().ok()
}

/// Playhead, active cues, sounds coming up and sounds played.
fn hud(game: Res<Game>, mut videos: Query<&mut PraxiVideo>, mut hud: Query<&mut Text, With<Hud>>) {
    let (Ok(mut video), Ok(mut text)) = (videos.single_mut(), hud.single_mut()) else { return };
    let player = video.player();
    let now = player.position();
    let state = match (player.is_playing(), player.looping()) {
        (true, true) => "playing, looping",
        (true, false) => "playing",
        (false, _) => "paused",
    };
    let mut s = format!("{:.2} / {:.2} s  {state}\n", now.as_secs_f64(), player.duration().as_secs_f64());
    let active: Vec<&str> = player.active_cues().map(|c| c.kind.as_str()).collect();
    s += &format!("active: {}\n", if active.is_empty() { "-".into() } else { active.join(", ") });
    for (until, c) in player.upcoming_cues(LOOKAHEAD) {
        if let Some(GameCue::Sound { name }) = parse(c) {
            s += &format!("preloading {name} in {:.1} s\n", until.as_secs_f64());
        }
    }
    for (name, at) in &game.played {
        if now >= *at && now - *at < Duration::from_secs(2) {
            s += &format!("playing {name}\n");
        }
    }
    s += "\nSpace play/pause  Left/Right seek 2 s  L loop  Esc quit";
    text.0 = s;
}
