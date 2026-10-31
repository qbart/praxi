//! A cutscene over gameplay: the video is prerolled, fades in over the game scene, plays
//! with alpha and cues, and fades out at the end or when skipped
//! (`docs/features/engine-integration.md`).
//!
//! ```sh
//! cargo run --release -p praxi-wgpu --example cutscene -- intro.praxi [--auto]
//! ```
//!
//! Space starts the cutscene, Escape skips it (or quits during gameplay). `light.flash`
//! cues flash the screen in their `color` field. `--auto` starts at once and quits
//! after the fade-out.

use praxi::{Event, Player};
use praxi_wgpu::{Rect, VideoRenderer};
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const FADE: Duration = Duration::from_millis(500);

/// The stand-in for gameplay (moving diagonal stripes) and a screen flash drawn over
/// everything.
const GAME_SHADER: &str = r#"
struct Scene {
    time: f32,
    flash: f32,
    flash_color: vec2<f32>,
    flash_blue: f32,
};
@group(0) @binding(0) var<uniform> scene: Scene;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    return vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let stripe = 0.5 + 0.5 * sin((pos.x + pos.y) * 0.02 - scene.time * 3.0);
    return vec4<f32>(mix(vec3<f32>(0.05, 0.1, 0.2), vec3<f32>(0.1, 0.35, 0.3), stripe), 1.0);
}

@fragment
fn fs_flash() -> @location(0) vec4<f32> {
    return vec4<f32>(scene.flash_color, scene.flash_blue, scene.flash);
}
"#;

type FilePlayer = Player<BufReader<File>>;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Gameplay,
    /// The cutscene plays; `fade` goes from 0 to 1 over `FADE` after it starts.
    Cutscene { since: Instant },
    /// Fading out since `since`.
    Ending { since: Instant },
}

struct Game {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    scene_pipeline: wgpu::RenderPipeline,
    flash_pipeline: wgpu::RenderPipeline,
    scene_uniform: wgpu::Buffer,
    scene_bind_group: wgpu::BindGroup,
    flash_bind_group: wgpu::BindGroup,
    player: FilePlayer,
    video: VideoRenderer,
    phase: Phase,
    started: Instant,
    last: Instant,
    /// Flash color and the time it started and ends.
    flash: Option<([f32; 3], Instant, Duration)>,
    auto: bool,
}

impl Game {
    fn new(event_loop: &ActiveEventLoop, instance: &wgpu::Instance, path: &str, auto: bool) -> Result<Game, String> {
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_title("praxi cutscene").with_inner_size(winit::dpi::LogicalSize::new(1280, 720)))
                .map_err(|e| e.to_string())?,
        );
        let surface = instance.create_surface(window.clone()).map_err(|e| e.to_string())?;
        let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(instance, Some(&surface)))
            .map_err(|e| e.to_string())?;
        // The player picks the variant the GPU samples.
        let caps = praxi_wgpu::capabilities(&adapter, None)?;
        let file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
        let mut player = Player::open(BufReader::new(file), &caps).map_err(|e| e.to_string())?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("cutscene"),
            required_features: praxi_wgpu::required_features(&adapter),
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("the window surface is not supported")?;
        if let Some(&srgb) = surface.get_capabilities(&adapter).formats.iter().find(|f| f.is_srgb()) {
            config.format = srgb;
        }
        surface.configure(&device, &config);

        // Preroll: seeking decodes the first frame and `prepare` uploads it, so the
        // cutscene's first frame is on the GPU before it is needed.
        player.seek(Duration::ZERO).map_err(|e| e.to_string())?;
        while player.poll_event().is_some() {}
        let mut video = VideoRenderer::new(&device, config.format, &player);
        video.prepare(&queue, &player, 0.0);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene"),
            source: wgpu::ShaderSource::Wgsl(GAME_SHADER.into()),
        });
        let pipeline = |fragment, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fragment),
                layout: None,
                vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format: config.format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let scene_pipeline = pipeline("fs", None);
        let flash_pipeline = pipeline("fs_flash", Some(wgpu::BlendState::ALPHA_BLENDING));
        let scene_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Each pipeline has its own derived layout, so each gets its own bind group.
        let bind_group = |pipeline: &wgpu::RenderPipeline| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scene"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: scene_uniform.as_entire_binding() }],
            })
        };
        let (scene_bind_group, flash_bind_group) = (bind_group(&scene_pipeline), bind_group(&flash_pipeline));

        let now = Instant::now();
        let mut game = Game {
            window,
            surface,
            device,
            queue,
            config,
            scene_pipeline,
            flash_pipeline,
            scene_uniform,
            scene_bind_group,
            flash_bind_group,
            player,
            video,
            phase: Phase::Gameplay,
            started: now,
            last: now,
            flash: None,
            auto,
        };
        if auto {
            game.start_cutscene();
        }
        Ok(game)
    }

    fn start_cutscene(&mut self) {
        if self.phase == Phase::Gameplay {
            println!("cutscene: start");
            self.player.play();
            self.phase = Phase::Cutscene { since: Instant::now() };
        }
    }

    fn end_cutscene(&mut self) {
        if let Phase::Cutscene { .. } = self.phase {
            println!("cutscene: fade out");
            self.phase = Phase::Ending { since: Instant::now() };
        }
    }

    /// Returns `true` to quit.
    fn frame(&mut self) -> Result<bool, String> {
        let now = Instant::now();
        let dt = now - self.last;
        self.last = now;

        // The video keeps playing while it fades out.
        self.player.advance(dt).map_err(|e| e.to_string())?;
        while let Some(event) = self.player.poll_event() {
            match &event {
                Event::Cue(cue) if cue.kind == "light.flash" => {
                    let color = match cue.get_str("color") {
                        Some("red") => [1.0, 0.1, 0.1],
                        Some("green") => [0.1, 1.0, 0.1],
                        Some("blue") => [0.1, 0.2, 1.0],
                        _ => [1.0, 1.0, 1.0],
                    };
                    let length = if cue.duration.is_zero() { Duration::from_millis(300) } else { cue.duration };
                    self.flash = Some((color, now, length));
                }
                Event::Finished => self.end_cutscene(),
                _ => {}
            }
            println!("[{:7.3}] {event:?}", self.player.position().as_secs_f64());
        }

        let opacity = match self.phase {
            Phase::Gameplay => 0.0,
            Phase::Cutscene { since } => ((now - since).as_secs_f32() / FADE.as_secs_f32()).min(1.0),
            Phase::Ending { since } if now - since >= FADE => {
                // Back to gameplay; preroll again for the next time.
                println!("cutscene: done");
                self.phase = Phase::Gameplay;
                self.player.pause();
                self.player.seek(Duration::ZERO).map_err(|e| e.to_string())?;
                while self.player.poll_event().is_some() {}
                if self.auto {
                    return Ok(true);
                }
                0.0
            }
            Phase::Ending { since } => 1.0 - (now - since).as_secs_f32() / FADE.as_secs_f32(),
        };
        self.video.prepare(&self.queue, &self.player, opacity);

        let flash = match self.flash {
            Some((color, at, length)) if now - at < length => (color, 1.0 - (now - at).as_secs_f32() / length.as_secs_f32()),
            _ => ([0.0; 3], 0.0),
        };
        let scene = [(now - self.started).as_secs_f32(), flash.1 * 0.8, flash.0[0], flash.0[1], flash.0[2], 0.0, 0.0, 0.0];
        let bytes: Vec<u8> = scene.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.queue.write_buffer(&self.scene_uniform, 0, &bytes);

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            _ => {
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            // The game scene first, then the video blended over it, then the flash.
            pass.set_pipeline(&self.scene_pipeline);
            pass.set_bind_group(0, &self.scene_bind_group, &[]);
            pass.draw(0..4, 0..1);
            if opacity > 0.0 {
                let video = self.player.tracks()[0].params;
                self.video.draw(&mut pass, Rect::fit(video.width, video.height, self.config.width, self.config.height));
            }
            if flash.1 > 0.0 {
                // `VideoRenderer::draw` set the viewport to the video rectangle.
                pass.set_viewport(0.0, 0.0, self.config.width as f32, self.config.height as f32, 0.0, 1.0);
                pass.set_pipeline(&self.flash_pipeline);
                pass.set_bind_group(0, &self.flash_bind_group, &[]);
                pass.draw(0..4, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);
        self.window.request_redraw();
        Ok(false)
    }
}

struct App {
    instance: wgpu::Instance,
    path: String,
    auto: bool,
    game: Option<Game>,
    error: Option<String>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.game.is_none() {
            match Game::new(event_loop, &self.instance, &self.path, self.auto) {
                Ok(game) => self.game = Some(game),
                Err(e) => {
                    self.error = Some(e);
                    event_loop.exit();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(game) = &mut self.game else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                game.config.width = size.width.max(1);
                game.config.height = size.height.max(1);
                game.surface.configure(&game.device, &game.config);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && !event.repeat => {
                match event.logical_key {
                    Key::Named(NamedKey::Space) => game.start_cutscene(),
                    Key::Named(NamedKey::Escape) if game.phase == Phase::Gameplay => event_loop.exit(),
                    Key::Named(NamedKey::Escape) => game.end_cutscene(),
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => match game.frame() {
                Ok(true) => event_loop.exit(),
                Ok(false) => {}
                Err(e) => {
                    self.error = Some(e);
                    event_loop.exit();
                }
            },
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: cutscene FILE [--auto]")?;
    let auto = args.next().as_deref() == Some("--auto");
    let event_loop = EventLoop::new()?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(
        event_loop.owned_display_handle(),
    )));
    let mut app = App { instance, path, auto, game: None, error: None };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}
