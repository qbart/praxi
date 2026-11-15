//! A video on two spinning boxes in 3D, left without mipmaps and right with them
//! (`docs/features/bevy.md#video-box-example`).
//!
//! Up/Down move the camera away and back, Space plays and pauses the video, R starts and
//! stops the spinning, Escape quits.

use bevy::mesh::VertexAttributeValues;
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

/// A box showing the video, spinning about its vertical axis by this angle.
#[derive(Component)]
struct Spin(f32);

/// Whether the boxes spin (R).
#[derive(Resource)]
struct Spinning(bool);

/// The label of the box on `.0`, with its name.
#[derive(Component)]
struct Label(Entity, &'static str);

/// Tilts the boxes towards the camera, so their top shows.
const TILT: f32 = 0.35;

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
        .insert_resource(Spinning(true))
        .add_systems(Startup, setup)
        .add_systems(Update, (input.before(PraxiSystems::Advance), spin, label.after(PraxiSystems::Advance)))
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
        let entity = commands.spawn((
            video,
            Mesh3d(meshes.add(video_box(Vec3::new(aspect, 1.0, 1.0)))),
            MeshMaterial3d(material),
            Transform::from_xyz(x, 0.0, 0.0).with_rotation(Quat::from_rotation_x(TILT)),
            Spin(0.0),
        )).id();
        commands.spawn((
            Text::new(label),
            Label(entity, label),
            Node { position_type: PositionType::Absolute, top: px(16), left: if mipmaps { percent(62) } else { percent(18) }, ..default() },
        ));
    }
    commands.spawn((
        Text::new("Up/Down: camera distance   Space: play/pause   R: spin   Esc: quit"),
        Node { position_type: PositionType::Absolute, bottom: px(16), left: px(16), ..default() },
    ));
}

fn input(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut camera: Query<&mut Transform, With<Camera3d>>,
    mut videos: Query<&mut PraxiVideo>,
    mut spinning: ResMut<Spinning>,
    mut exit: MessageWriter<AppExit>,
) {
    if keys.just_pressed(KeyCode::KeyR) {
        spinning.0 = !spinning.0;
    }
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

/// A box of `size` with the whole video upright on each face: the sides with up along y,
/// the top and bottom read from the front. `Cuboid`'s own UVs flip some faces.
fn video_box(size: Vec3) -> Mesh {
    let mut mesh = Cuboid::from_size(size).mesh().build();
    let (Some(VertexAttributeValues::Float32x3(positions)), Some(VertexAttributeValues::Float32x3(normals))) =
        (mesh.attribute(Mesh::ATTRIBUTE_POSITION), mesh.attribute(Mesh::ATTRIBUTE_NORMAL))
    else {
        unreachable!("a cuboid has positions and normals")
    };
    let uvs: Vec<[f32; 2]> = positions
        .iter()
        .zip(normals)
        .map(|(p, n)| {
            let (p, n) = (Vec3::from(*p), Vec3::from(*n));
            let up = if n.y.abs() > 0.5 { Vec3::NEG_Z * n.y.signum() } else { Vec3::Y };
            let right = up.cross(n);
            let extent = |axis: Vec3| (axis * size).length();
            [0.5 + p.dot(right) / extent(right), 0.5 - p.dot(up) / extent(up)]
        })
        .collect();
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh
}

fn spin(time: Res<Time>, spinning: Res<Spinning>, mut boxes: Query<(&mut Spin, &mut Transform)>) {
    if !spinning.0 {
        return;
    }
    for (mut spin, mut transform) in &mut boxes {
        spin.0 += 0.5 * time.delta_secs();
        transform.rotation = Quat::from_rotation_x(TILT) * Quat::from_rotation_y(spin.0);
    }
}

/// Shows each box's format, clock and the index of the frame decoded last.
fn label(mut videos: Query<&mut PraxiVideo>, mut labels: Query<(&Label, &mut Text)>) {
    for (label, mut text) in &mut labels {
        let Ok(mut video) = videos.get_mut(label.0) else { continue };
        let player = video.player();
        let format = player.tracks()[0].params.format;
        let frame = player.video_frame(0).map_or(0, |f| f.index);
        text.0 = format!("{}\n{format:?}  {:.2} s  frame {frame}", label.1, player.position().as_secs_f32());
    }
}
