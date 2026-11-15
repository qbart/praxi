//! A video on two spinning boxes in 3D, left without mipmaps and right with them
//! (`docs/features/bevy.md#video-box-example`).
//!
//! Up/Down move the camera away and back, Space pauses, Escape quits.

use bevy::prelude::*;
use bevy::render::renderer::RenderAdapter;
use bevy::render::view::Tonemapping;
use clap::Parser;
use praxi_bevy::{PraxiPlugin, PraxiSystems, PraxiVideo, PraxiVideoOptions};
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    /// The praxi file to play.
    file: PathBuf,
}

#[derive(Resource)]
struct File(PathBuf);

/// A box showing the video, spinning about `axis`.
#[derive(Component)]
struct Spin(Vec3);

fn main() -> AppExit {
    let args = Args::parse();
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window { title: format!("praxi video box: {}", args.file.display()), resolution: (1280, 720).into(), ..default() }),
            ..default()
        }))
        .add_plugins(PraxiPlugin)
        .insert_resource(ClearColor(Color::srgb(0.05, 0.05, 0.07)))
        .insert_resource(File(args.file))
        .add_systems(Startup, setup)
        .add_systems(Update, (input.before(PraxiSystems::Advance), spin))
        .run()
}

fn setup(
    mut commands: Commands,
    file: Res<File>,
    adapter: Res<RenderAdapter>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut exit: MessageWriter<AppExit>,
) {
    // The video's colors as they are: unlit, and no tonemapping on the camera.
    commands.spawn((Camera3d::default(), Tonemapping::None, Transform::from_xyz(0.0, 0.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y)));
    for (x, mipmaps, label) in [(-1.3, false, "no mipmaps"), (1.3, true, "mipmaps")] {
        let mut video = match PraxiVideo::open_with(&file.0, &adapter, &mut images, PraxiVideoOptions { mipmaps }) {
            Ok(video) => video,
            Err(e) => {
                eprintln!("{}: {e}", file.0.display());
                exit.write(AppExit::error());
                return;
            }
        };
        let player = video.player();
        player.set_looping(true);
        player.play().unwrap();
        let params = player.tracks()[0].params;
        let aspect = params.width as f32 / params.height as f32;
        let material = materials.add(StandardMaterial { base_color_texture: Some(video.image.clone()), unlit: true, ..default() });
        commands.spawn((
            video,
            Mesh3d(meshes.add(Cuboid::new(aspect, 1.0, 1.0))),
            MeshMaterial3d(material),
            Transform::from_xyz(x, 0.0, 0.0),
            Spin(Vec3::new(0.3, 1.0, 0.1).normalize()),
        ));
        commands.spawn((
            Text::new(label),
            Node { position_type: PositionType::Absolute, top: px(16), left: if mipmaps { percent(62) } else { percent(18) }, ..default() },
        ));
    }
    commands.spawn((
        Text::new("Up/Down: camera distance   Space: pause   Esc: quit"),
        Node { position_type: PositionType::Absolute, bottom: px(16), left: px(16), ..default() },
    ));
}

fn input(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut camera: Query<&mut Transform, With<Camera3d>>,
    mut videos: Query<&mut PraxiVideo>,
    mut exit: MessageWriter<AppExit>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        exit.write(AppExit::Success);
    }
    if keys.just_pressed(KeyCode::Space) {
        for mut video in &mut videos {
            let player = video.player();
            if player.is_playing() {
                player.pause();
            } else {
                let _ = player.play();
            }
        }
    }
    let mut step = 0.0;
    if keys.pressed(KeyCode::ArrowUp) {
        step += 1.0;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        step -= 1.0;
    }
    for mut transform in &mut camera {
        // Away and back, scaled with the distance so far views move as fast as near ones.
        let z = transform.translation.z;
        transform.translation.z = (z * (1.0 + step * time.delta_secs())).clamp(2.0, 120.0);
    }
}

fn spin(time: Res<Time>, mut boxes: Query<(&mut PraxiVideo, &Spin, &mut Transform)>) {
    for (mut video, spin, mut transform) in &mut boxes {
        // Spin while the video plays.
        if video.player().is_playing() {
            transform.rotate(Quat::from_axis_angle(spin.0, 0.5 * time.delta_secs()));
        }
    }
}
