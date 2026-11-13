//! Plays praxi files in Bevy through `praxi-wgpu` (`docs/features/bevy.md`).

mod file;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssets;
use bevy::render::renderer::{RenderAdapter, RenderDevice, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSystems};
use file::SharedFile;
use praxi::{Capabilities, Event, Player};
use praxi_wgpu::{Rect, VideoRenderer};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::io::BufReader;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use praxi;

/// The player type of a [`PraxiVideo`].
pub type FilePlayer = Player<BufReader<SharedFile>>;

/// Format of the video texture and image.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Advances every [`PraxiVideo`], sends its events as [`PraxiEvent`] messages and shows its
/// frames in its image.
pub struct PraxiPlugin;

impl Plugin for PraxiPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PraxiEvent>()
            .configure_sets(Update, (PraxiSystems::Advance, PraxiSystems::Cues).chain())
            .add_systems(Update, advance.in_set(PraxiSystems::Advance))
            .add_systems(PostUpdate, draw);
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .init_resource::<Copies>()
                .add_systems(ExtractSchedule, extract)
                .add_systems(Render, copy.in_set(RenderSystems::PrepareResources));
        }
    }
}

/// The `Update` systems of the plugins, in order.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PraxiSystems {
    /// Advances the players and sends [`PraxiEvent`]s.
    Advance,
    /// Sends [`PraxiCue`]s.
    Cues,
}

/// A player event of the video on `entity`.
#[derive(Message, Debug, Clone)]
pub struct PraxiEvent {
    pub entity: Entity,
    pub event: praxi::Event,
}

/// Sends every cue that parses into `T` (usually the game's cue enum) as a [`PraxiCue<T>`];
/// needs [`PraxiPlugin`] (`docs/features/cue-runtime.md#bevy-typed-cue-messages`).
pub struct PraxiCuePlugin<T>(PhantomData<fn() -> T>);

impl<T> Default for PraxiCuePlugin<T> {
    fn default() -> Self {
        PraxiCuePlugin(PhantomData)
    }
}

impl<T: DeserializeOwned + Send + Sync + 'static> Plugin for PraxiCuePlugin<T> {
    fn build(&self, app: &mut App) {
        app.add_message::<PraxiCue<T>>().add_systems(Update, parse_cues::<T>.in_set(PraxiSystems::Cues));
    }
}

/// A cue of the video on `entity`, parsed into `T`.
#[derive(Message, Debug, Clone)]
pub struct PraxiCue<T> {
    pub entity: Entity,
    /// Matches the `id` of its `CueEnd` in [`PraxiEvent`].
    pub id: u64,
    pub stream: u16,
    pub time: Duration,
    /// Zero for instantaneous cues.
    pub duration: Duration,
    pub position: Option<(f32, f32)>,
    pub cue: T,
}

/// Parses the cues among this frame's [`PraxiEvent`]s; unknown cue types are skipped,
/// malformed cues reported.
fn parse_cues<T: DeserializeOwned + Send + Sync + 'static>(
    mut events: MessageReader<PraxiEvent>,
    videos: Query<&PraxiVideo>,
    mut cues: MessageWriter<PraxiCue<T>>,
) {
    for PraxiEvent { entity, event } in events.read() {
        let Event::Cue(c) = event else { continue };
        match c.parse::<T>() {
            Ok(cue) => {
                let (id, stream, time, duration, position) = (c.id, c.stream, c.time, c.duration, c.position());
                cues.write(PraxiCue { entity: *entity, id, stream, time, duration, position, cue });
            }
            Err(e) if e.is_unknown_type() => {}
            Err(e) => {
                let file = videos.get(*entity).map(|v| v.path.display().to_string()).unwrap_or_else(|_| entity.to_string());
                eprintln!("praxi: {file}: {e}");
            }
        }
    }
}

/// The point at a cue position (`x`, `y` in `0..=1` from the top left of the video) in
/// `rect`, y down: a UI node's rectangle or the window in logical pixels.
pub fn position_in_rect((x, y): (f32, f32), rect: bevy::math::Rect) -> Vec2 {
    rect.min + Vec2::new(x, y) * rect.size()
}

/// The world point at a cue position on a sprite showing the video: `size` is the sprite's
/// size, `anchor` the value of its `Anchor` (`Vec2::ZERO` for the centre). World y is up,
/// so `y = 0` is the sprite's top edge.
pub fn position_on_sprite((x, y): (f32, f32), size: Vec2, anchor: Vec2, transform: &GlobalTransform) -> Vec3 {
    let local = (Vec2::new(x - 0.5, 0.5 - y) - anchor) * size;
    transform.transform_point(local.extend(0.0))
}

/// A praxi file playing into an [`Image`].
#[derive(Component)]
pub struct PraxiVideo {
    /// Shows video track 0 and the tracks blended over it.
    pub image: Handle<Image>,
    player: SyncCell<FilePlayer>,
    file: SharedFile,
    path: PathBuf,
    caps: Capabilities,
    gpu: Option<Gpu>,
    /// Frames drawn into `gpu.target`, for the copy into `image`.
    version: u64,
}

/// The renderer and the texture the video is drawn into.
struct Gpu {
    renderer: VideoRenderer,
    target: wgpu::Texture,
    view: wgpu::TextureView,
}

impl PraxiVideo {
    /// Opens `path` with the texture formats `adapter` supports, paused at the first frame,
    /// and adds its image to `images`.
    pub fn open(path: &Path, adapter: &RenderAdapter, images: &mut Assets<Image>) -> Result<PraxiVideo, String> {
        let caps = praxi_wgpu::capabilities(adapter, None)?;
        let (player, file) = open_player(path, &caps)?;
        let video = player.tracks()[0].params;
        let size = wgpu::Extent3d { width: video.width, height: video.height, depth_or_array_layers: 1 };
        let image = Image::new_uninit(size, wgpu::TextureDimension::D2, FORMAT, RenderAssetUsages::RENDER_WORLD);
        Ok(PraxiVideo {
            image: images.add(image),
            player: SyncCell::new(player),
            file,
            path: path.to_path_buf(),
            caps,
            gpu: None,
            version: 0,
        })
    }

    pub fn player(&mut self) -> &mut FilePlayer {
        self.player.get()
    }

    /// The file the video plays.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Closes the player's file so that it can be replaced; the player fails to read until
    /// [`PraxiVideo::reopen`].
    pub fn release_file(&mut self) {
        self.file.close();
    }

    /// Opens the file again and restores the position, play state and loop setting. Cue
    /// replacements are not kept.
    pub fn reopen(&mut self) -> Result<(), String> {
        let old = self.player.get();
        let (at, playing, looping) = (old.position(), old.is_playing(), old.looping());
        let (mut player, file) = open_player(&self.path, &self.caps)?;
        player.set_looping(looping);
        player.seek(at).map_err(|e| e.to_string())?;
        if playing {
            player.play().map_err(|e| e.to_string())?;
        }
        self.player = SyncCell::new(player);
        self.file = file;
        Ok(())
    }
}

fn open_player(path: &Path, caps: &Capabilities) -> Result<(FilePlayer, SharedFile), String> {
    let file = SharedFile::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut player = Player::open(BufReader::new(file.clone()), caps).map_err(|e| format!("{}: {e}", path.display()))?;
    if player.tracks().is_empty() {
        return Err("the file has no video track this GPU can show".into());
    }
    player.seek(Duration::ZERO).map_err(|e| e.to_string())?;
    Ok((player, file))
}

fn advance(time: Res<Time>, mut videos: Query<(Entity, &mut PraxiVideo)>, mut events: MessageWriter<PraxiEvent>) {
    for (entity, mut video) in &mut videos {
        let player = video.player.get();
        if let Err(e) = player.advance(time.delta()) {
            eprintln!("praxi: {entity}: {e}");
            player.pause();
        }
        while let Some(event) = player.poll_event() {
            events.write(PraxiEvent { entity, event });
        }
    }
}

fn draw(device: Option<Res<RenderDevice>>, queue: Option<Res<RenderQueue>>, mut videos: Query<&mut PraxiVideo>) {
    let (Some(device), Some(queue)) = (device, queue) else { return };
    let (device, queue) = (device.wgpu_device(), &**queue);
    for mut video in &mut videos {
        let video = &mut *video;
        let player = video.player.get();
        let gpu = video.gpu.get_or_insert_with(|| {
            let params = player.tracks()[0].params;
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("praxi video"),
                size: wgpu::Extent3d { width: params.width, height: params.height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = target.create_view(&Default::default());
            Gpu { renderer: VideoRenderer::new(device, FORMAT, player), target, view }
        });
        if gpu.renderer.prepare(device, queue, player, 1.0) == 0 {
            continue;
        }
        let size = gpu.target.size();
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("praxi video"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &gpu.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            gpu.renderer.draw_replace(&mut pass, Rect { x: 0.0, y: 0.0, w: size.width as f32, h: size.height as f32 });
        }
        queue.submit([encoder.finish()]);
        video.version += 1;
    }
}

/// Video textures to copy into images, and the version last copied into each.
#[derive(Resource, Default)]
struct Copies {
    pending: Vec<(AssetId<Image>, wgpu::Texture, u64)>,
    copied: HashMap<AssetId<Image>, u64>,
}

fn extract(videos: Extract<Query<&PraxiVideo>>, mut copies: ResMut<Copies>) {
    copies.pending.clear();
    for video in &videos {
        if let Some(gpu) = &video.gpu {
            copies.pending.push((video.image.id(), gpu.target.clone(), video.version));
        }
    }
}

fn copy(mut copies: ResMut<Copies>, images: Res<RenderAssets<GpuImage>>, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    let copies = &mut *copies;
    let mut encoder = None;
    for (id, texture, version) in &copies.pending {
        let Some(image) = images.get(*id) else { continue };
        if copies.copied.get(id) == Some(version) {
            continue;
        }
        let encoder = encoder.get_or_insert_with(|| device.wgpu_device().create_command_encoder(&Default::default()));
        encoder.copy_texture_to_texture(texture.as_image_copy(), image.texture.as_image_copy(), texture.size());
        copies.copied.insert(*id, *version);
    }
    if let Some(encoder) = encoder {
        queue.submit([encoder.finish()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::app::PluginsState;
    use bevy::time::TimeUpdateStrategy;
    use praxi_encoder::{Encoder, EncoderConfig, VariantConfig};

    /// Skips a test that cannot run here, or fails it when `PRAXI_TEST_STRICT` is set
    /// (`docs/features/testing.md`).
    fn skip(why: &str) {
        assert!(std::env::var_os("PRAXI_TEST_STRICT").is_none(), "PRAXI_TEST_STRICT: {why}");
        eprintln!("skipped: {why}");
    }

    /// A headless app with the render plugins and the praxi plugin, once the renderer is
    /// ready; `None` without an adapter.
    fn app() -> Option<App> {
        // Bevy's renderer panics without an adapter, so look for one first.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        bevy::tasks::block_on(instance.request_adapter(&Default::default())).ok()?;
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::window::WindowPlugin { primary_window: None, ..Default::default() },
            AssetPlugin::default(),
            bevy::render::RenderPlugin { synchronous_pipeline_compilation: true, ..Default::default() },
            ImagePlugin::default(),
            bevy::mesh::MeshPlugin,
            PraxiPlugin,
            PraxiCuePlugin::<TestCue>::default(),
        ))
        .init_resource::<ClearColor>()
        .init_resource::<Seen>()
        .add_systems(Update, collect.after(PraxiSystems::Cues));
        while app.plugins_state() == PluginsState::Adding {
            bevy::tasks::tick_global_task_pools_on_main_thread();
        }
        app.finish();
        app.cleanup();
        app.world().get_resource::<RenderAdapter>()?;
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(50)));
        Some(app)
    }

    #[derive(Debug, Clone, PartialEq, serde::Deserialize)]
    enum TestCue {
        #[serde(rename = "boom")]
        Boom,
    }

    /// Every [`PraxiEvent`] and [`PraxiCue`] so far.
    #[derive(Resource, Default)]
    struct Seen(Vec<PraxiEvent>, Vec<PraxiCue<TestCue>>);

    fn collect(mut events: MessageReader<PraxiEvent>, mut cues: MessageReader<PraxiCue<TestCue>>, mut seen: ResMut<Seen>) {
        seen.0.extend(events.read().cloned());
        seen.1.extend(cues.read().cloned());
    }

    #[test]
    fn positions_map_to_rects_and_sprites() {
        let rect = bevy::math::Rect::new(100.0, 50.0, 500.0, 250.0);
        assert_eq!(position_in_rect((0.0, 0.0), rect), Vec2::new(100.0, 50.0));
        assert_eq!(position_in_rect((0.25, 1.0), rect), Vec2::new(200.0, 250.0));

        let size = Vec2::new(400.0, 200.0);
        let at = GlobalTransform::from_translation(Vec3::new(10.0, 20.0, 5.0));
        assert_eq!(position_on_sprite((0.5, 0.5), size, Vec2::ZERO, &at), Vec3::new(10.0, 20.0, 5.0));
        assert_eq!(position_on_sprite((0.0, 0.0), size, Vec2::ZERO, &at), Vec3::new(-190.0, 120.0, 5.0), "top left, y up");
        let top_left_anchor = Vec2::new(-0.5, 0.5);
        assert_eq!(position_on_sprite((0.0, 0.0), size, top_left_anchor, &at), Vec3::new(10.0, 20.0, 5.0));
        let scaled = GlobalTransform::from(Transform::from_scale(Vec3::splat(2.0)));
        assert_eq!(position_on_sprite((1.0, 1.0), size, Vec2::ZERO, &scaled), Vec3::new(400.0, -200.0, 0.0));
    }

    fn clip(path: &Path, variant: VariantConfig) {
        let config = EncoderConfig::new(40, 24, 10, 1).variant(variant).keyint(4).cue(0.25, "boom");
        let mut enc = Encoder::new(std::fs::File::create(path).unwrap(), config).unwrap();
        for t in 0..8u32 {
            let frame: Vec<u8> = (0..40 * 24).flat_map(|i| [(i % 40 * 6 + t * 9) as u8, (i / 40 * 10) as u8, 90, 255]).collect();
            enc.push_frame(&frame).unwrap();
        }
        enc.finish().unwrap();
    }

    #[test]
    fn plays_into_the_image_and_sends_events() {
        let Some(mut app) = app() else {
            return skip("no adapter");
        };
        let dir = std::env::temp_dir().join(format!("praxi-bevy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let adapter = app.world().resource::<RenderAdapter>().clone();
        let formats = praxi_wgpu::supported_formats(adapter.features());
        let mut variants = vec![("motion", VariantConfig::motion())];
        if formats.contains(&praxi::TextureFormat::Bc7) {
            variants.push(("bc7", VariantConfig::bc7()));
        }
        for (name, variant) in variants {
            let path = dir.join(format!("{name}.praxi"));
            clip(&path, variant);
            if name == "motion" && !praxi_wgpu::supports_motion(&adapter) {
                skip("the adapter cannot run the motion decoder");
                continue;
            }
            let world = app.world_mut();
            let mut video =
                world.resource_scope(|_, mut images: Mut<Assets<Image>>| PraxiVideo::open(&path, &adapter, &mut images)).unwrap();
            video.player().play().unwrap();
            let image = video.image.id();
            let entity = world.spawn(video).id();
            for _ in 0..10 {
                app.update();
            }
            let video = app.world().get::<PraxiVideo>(entity).unwrap();
            assert!(video.version >= 2, "{name}: {} frames drawn", video.version);
            let copies = app.sub_app(RenderApp).world().resource::<Copies>();
            assert!(copies.copied.get(&image).is_some_and(|&v| v + 1 >= video.version), "{name}: copied {:?}", copies.copied);
            let seen = app.world().resource::<Seen>();
            let cue = |m: &PraxiEvent| m.entity == entity && matches!(&m.event, praxi::Event::Cue(c) if c.kind == "boom");
            assert!(seen.0.iter().any(cue), "{name}: the cue arrives as a message: {:?}", seen.0);
            let typed: Vec<_> = seen.1.iter().filter(|c| c.entity == entity).collect();
            assert_eq!(typed.len(), 1, "{name}: one typed cue: {:?}", seen.1);
            assert_eq!((typed[0].cue.clone(), typed[0].time), (TestCue::Boom, Duration::from_millis(250)));
            app.world_mut().despawn(entity);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
