//! A video on two spinning boxes in 3D, left without mipmaps and right with them
//! (`docs/features/bevy.md#video-box-example`).
//!
//! Up/Down move the camera away and back, Space plays and pauses the video, R starts and
//! stops the spinning, Escape quits. `--hdr` puts the SDR variant (left) next to the HDR
//! one (right), glowing through bloom, with a panel for strength, bloom and the tonemapper
//! (`docs/features/hdr.md#hdr-demo`).

use bevy::mesh::VertexAttributeValues;
use bevy::prelude::*;
use bevy::render::renderer::RenderAdapter;
use bevy::camera::Hdr;
use bevy::post_process::bloom::Bloom;
use bevy::render::view::Tonemapping;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};
use clap::Parser;
use praxi_bevy::{PraxiPlugin, PraxiSystems, PraxiVideo, PraxiVideoOptions};
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    /// The praxi file to play.
    file: PathBuf,
    /// The SDR and HDR variants side by side, emissive on an HDR camera with bloom.
    #[arg(long)]
    hdr: bool,
}

/// The file and whether it plays as HDR.
#[derive(Resource)]
struct File(PathBuf, bool);

/// A box showing the video, spinning about its vertical axis by this angle.
#[derive(Component)]
struct Spin(f32);

/// Whether the boxes spin (R).
#[derive(Resource)]
struct Spinning(bool);

/// The label of the box on `.0`, with its name and whether its image is HDR.
#[derive(Component)]
struct Label(Entity, &'static str, bool);

/// Settings of the HDR demo's panel (`docs/features/hdr.md#hdr-demo`).
#[derive(Resource)]
struct HdrDemo {
    /// Emissive multiplier of the boxes.
    strength: f32,
    bloom: bool,
    bloom_intensity: f32,
    tonemapping: Tonemapping,
    /// The boxes' materials.
    materials: Vec<Handle<StandardMaterial>>,
}

/// Tonemappers that need no lookup tables (`tonemapping_luts`), with their names.
const TONEMAPPERS: [(Tonemapping, &str); 5] = [
    (Tonemapping::AcesFitted, "AcesFitted"),
    (Tonemapping::Reinhard, "Reinhard"),
    (Tonemapping::ReinhardLuminance, "ReinhardLuminance"),
    (Tonemapping::SomewhatBoringDisplayTransform, "SomewhatBoringDisplayTransform"),
    (Tonemapping::None, "None (clips at 1.0)"),
];

/// Tilts the boxes towards the camera, so their top shows.
const TILT: f32 = 0.35;

fn main() -> AppExit {
    let args = Args::parse();
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window { title: format!("praxi video box: {}", args.file.display()), resolution: (1280, 720).into(), ..default() }),
        ..default()
    }))
    .add_plugins(PraxiPlugin)
    .insert_resource(ClearColor(Color::srgb(0.05, 0.05, 0.07)))
    .insert_resource(Spinning(true))
    .add_systems(Startup, setup)
    .add_systems(Update, (input.before(PraxiSystems::Advance), spin, label.after(PraxiSystems::Advance)));
    if args.hdr {
        app.add_plugins(EguiPlugin::default())
            .insert_resource(HdrDemo {
                strength: 1.0,
                bloom: true,
                bloom_intensity: Bloom::NATURAL.intensity,
                tonemapping: Tonemapping::AcesFitted,
                materials: Vec::new(),
            })
            .add_systems(EguiPrimaryContextPass, panel)
            .add_systems(Update, apply_demo.run_if(resource_changed::<HdrDemo>));
    }
    app.insert_resource(File(args.file, args.hdr)).run()
}

fn setup(
    mut commands: Commands,
    file: Res<File>,
    adapter: Res<RenderAdapter>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut exit: MessageWriter<AppExit>,
    mut demo: Option<ResMut<HdrDemo>>,
) {
    let hdr = file.1;
    let camera = (Camera3d::default(), Transform::from_xyz(0.0, 0.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y));
    if hdr {
        // Values above 1.0 bloom and go through the camera's tonemapper like the scene.
        // AcesFitted needs no lookup tables (Bevy's default needs `tonemapping_luts`).
        commands.spawn((camera, Hdr, Bloom::NATURAL, Tonemapping::AcesFitted));
    } else {
        // The video's colors as they are: unlit, and no tonemapping on the camera.
        commands.spawn((camera, Tonemapping::None));
    }
    // Mipmaps compared, or with --hdr the SDR and HDR variants (both with mipmaps).
    let boxes = match hdr {
        false => [(-1.3, false, false, "no mipmaps"), (1.3, true, false, "mipmaps")],
        true => [(-1.3, true, false, "SDR"), (1.3, true, true, "HDR")],
    };
    for (i, (x, mipmaps, hdr_image, label)) in boxes.into_iter().enumerate() {
        let options = PraxiVideoOptions { mipmaps, hdr: hdr_image };
        let mut video = match PraxiVideo::open_with(&file.0, &adapter, &mut images, options) {
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
        let material = materials.add(match hdr {
            // Emissive: the video's linear values light the box, bright parts bloom.
            true => StandardMaterial {
                base_color: Color::BLACK,
                emissive: LinearRgba::WHITE,
                emissive_texture: Some(video.image.clone()),
                ..default()
            },
            false => StandardMaterial { base_color_texture: Some(video.image.clone()), unlit: true, ..default() },
        });
        if let Some(demo) = demo.as_mut() {
            demo.materials.push(material.clone());
        }
        let entity = commands.spawn((
            video,
            Mesh3d(meshes.add(video_box(Vec3::new(aspect, 1.0, 1.0)))),
            MeshMaterial3d(material),
            Transform::from_xyz(x, 0.0, 0.0).with_rotation(Quat::from_rotation_x(TILT)),
            Spin(0.0),
        )).id();
        commands.spawn((
            Text::new(label),
            Label(entity, label, hdr_image),
            Node { position_type: PositionType::Absolute, top: px(16), left: if i == 1 { percent(62) } else { percent(18) }, ..default() },
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

/// Shows each box's format, clock and the index of the frame decoded last; in the HDR demo
/// also the image format and the HDR stream's color description.
fn label(mut videos: Query<&mut PraxiVideo>, mut labels: Query<(&Label, &mut Text)>, demo: Option<Res<HdrDemo>>) {
    for (label, mut text) in &mut labels {
        let Ok(mut video) = videos.get_mut(label.0) else { continue };
        let player = video.player();
        let params = player.tracks()[0].params;
        let frame = player.video_frame(0).map_or(0, |f| f.index);
        let clock = format!("{:.2} s  frame {frame}", player.position().as_secs_f32());
        text.0 = match demo {
            None => format!("{}\n{:?}  {clock}", label.1, params.format),
            Some(_) => {
                let image = if label.2 { "Rgba16Float" } else { "Rgba8UnormSrgb" };
                let mut s = format!("{}\n{:?} -> {image}\n", label.1, params.format);
                if params.hdr() {
                    let c = params.color;
                    let peak = c.max_cll as f32 / c.reference_white as f32;
                    s += &format!(
                        "{}  white {} nits\nmaxCLL {}  maxFALL {} nits\npeak {peak:.1}x reference white\n",
                        c.primaries, c.reference_white, c.max_cll, c.max_fall
                    );
                } else {
                    s += "SDR: values up to 1.0\n";
                }
                s + &clock
            }
        };
    }
}

/// The HDR demo's panel: strength, bloom and tonemapper, frame rate and camera.
fn panel(mut contexts: EguiContexts, mut demo: ResMut<HdrDemo>, time: Res<Time>, mut fps: Local<f32>) -> Result {
    let dt = time.delta_secs();
    if dt > 0.0 {
        *fps = if *fps == 0.0 { 1.0 / dt } else { *fps * 0.95 + 0.05 / dt };
    }
    let ctx = contexts.ctx_mut()?;
    egui::Window::new("HDR")
        .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -16.0])
        .resizable(false)
        .collapsible(true)
        .show(ctx, |ui| {
            // Only mark the resource changed when a control moves, so the scene is updated then.
            let mut d = (demo.strength, demo.bloom, demo.bloom_intensity, demo.tonemapping);
            ui.add(egui::Slider::new(&mut d.0, 0.0..=4.0).text("strength"));
            ui.checkbox(&mut d.1, "bloom");
            ui.add_enabled(d.1, egui::Slider::new(&mut d.2, 0.0..=0.6).text("bloom intensity"));
            let name = TONEMAPPERS.iter().find(|t| t.0 == d.3).map_or("?", |t| t.1);
            egui::ComboBox::from_label("tonemapper").selected_text(name).show_ui(ui, |ui| {
                for (t, name) in TONEMAPPERS {
                    ui.selectable_value(&mut d.3, t, name);
                }
            });
            if d != (demo.strength, demo.bloom, demo.bloom_intensity, demo.tonemapping) {
                (demo.strength, demo.bloom, demo.bloom_intensity, demo.tonemapping) = d;
            }
            ui.separator();
            ui.label(format!("{:.0} fps", *fps));
            ui.label("camera: Hdr, Rgba16Float view target");
            ui.label("left: SDR variant, right: HDR variant;\nwhat glows only on the right is above 1.0");
        });
    Ok(())
}

/// Applies the panel's settings to the materials and the camera.
fn apply_demo(
    mut commands: Commands,
    demo: Res<HdrDemo>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut camera: Query<(Entity, &mut Tonemapping, Option<&mut Bloom>), With<Camera3d>>,
) {
    for handle in &demo.materials {
        if let Some(mut material) = materials.get_mut(handle) {
            material.emissive = LinearRgba::WHITE * demo.strength;
        }
    }
    for (entity, mut tonemapping, bloom) in &mut camera {
        *tonemapping = demo.tonemapping;
        match (demo.bloom, bloom) {
            (true, Some(mut bloom)) => bloom.intensity = demo.bloom_intensity,
            (true, None) => {
                commands.entity(entity).insert(Bloom { intensity: demo.bloom_intensity, ..Bloom::NATURAL });
            }
            (false, Some(_)) => {
                commands.entity(entity).remove::<Bloom>();
            }
            (false, None) => {}
        }
    }
}
