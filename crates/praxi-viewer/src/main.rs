//! `praxi-viewer`: plays a `.praxi` file on the GPU (`docs/features/viewer.md`).

#[cfg(feature = "audio")]
mod audio;
mod gpu;

use clap::Parser;
use gpu::{Rect, Renderer};
use praxi::{Capabilities, Event, Player, TextureFormat, Value};
#[cfg(feature = "audio")]
use praxi::Clock;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;
type FilePlayer = Player<BufReader<File>>;

#[derive(Parser)]
#[command(name = "praxi-viewer", version, about = "Plays a praxi file on the GPU")]
struct Args {
    file: PathBuf,
    /// Only accept this texture format.
    #[arg(long, value_parser = parse_format)]
    format: Option<TextureFormat>,
    /// Start with looping on.
    #[arg(long = "loop")]
    looping: bool,
    /// Open paused.
    #[arg(long)]
    paused: bool,
    /// Start position in seconds.
    #[arg(long, default_value_t = 0.0)]
    at: f64,
    /// Do not open an audio device.
    #[arg(long)]
    no_audio: bool,
    /// Master clock: `audio` follows the audio output while audio plays, `game` the wall clock.
    #[arg(long, value_enum, default_value_t = ClockArg::Audio)]
    clock: ClockArg,
    /// Render the frame at --at to this PNG without a window and exit.
    #[arg(long)]
    screenshot: Option<PathBuf>,
    /// Screenshot size, WxH (default: track 0's size).
    #[arg(long, value_parser = parse_size)]
    size: Option<(u32, u32)>,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ClockArg {
    Audio,
    Game,
}

fn parse_format(s: &str) -> Result<TextureFormat, String> {
    TextureFormat::ALL
        .into_iter()
        .find(|f| f.name().replace(' ', "").eq_ignore_ascii_case(s))
        .ok_or_else(|| format!("unknown format {s:?}"))
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("expected WxH")?;
    match (w.parse(), h.parse()) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err(format!("invalid size {s:?}")),
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = if args.at.is_finite() && args.at >= 0.0 {
        if args.screenshot.is_some() { screenshot(&args) } else { run_window(args) }
    } else {
        Err(format!("invalid time {}", args.at).into())
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn open_player(args: &Args, caps: &Capabilities) -> AnyResult<FilePlayer> {
    let mut player = Player::open(BufReader::new(File::open(&args.file)?), caps)?;
    if player.tracks().is_empty() {
        return Err("the file has no video track this GPU can show".into());
    }
    player.set_looping(args.looping);
    if args.at > 0.0 {
        player.seek(Duration::from_secs_f64(args.at))?;
    }
    Ok(player)
}

fn describe_adapter(adapter: &wgpu::Adapter) {
    let info = adapter.get_info();
    println!("adapter: {} ({:?})", info.name, info.backend);
}

fn describe_tracks(player: &FilePlayer) {
    for (i, t) in player.tracks().iter().enumerate() {
        let p = &t.params;
        let alpha = if p.alpha() { ", alpha" } else { "" };
        println!("video {i}: stream {}, {} {}x{}{alpha}", t.stream, p.format, p.width, p.height);
    }
    for (i, t) in player.audio_tracks().iter().enumerate() {
        let p = &t.params;
        println!("audio {i}: stream {}, {:?}, {} Hz, {} ch, {}", t.stream, p.name, p.sample_rate, p.channels, p.format);
    }
}

fn print_event(at: Duration, event: &Event) {
    let what = match event {
        Event::Cue(cue) => {
            let mut s = format!("cue {} id={} stream={}", cue.kind, cue.id, cue.stream);
            if !cue.duration.is_zero() {
                s += &format!(" duration={:.3}", cue.duration.as_secs_f64());
            }
            for (k, v) in &cue.fields {
                let v = match v {
                    Value::String(v) => format!("{v:?}"),
                    Value::Int(v) => v.to_string(),
                    Value::Float(v) => v.to_string(),
                    Value::Bool(v) => v.to_string(),
                    Value::Bytes(v) => format!("<{} bytes>", v.len()),
                };
                s += &format!(" {k}={v}");
            }
            s
        }
        Event::CueEnd { id, stream } => format!("cue end id={id} stream={stream}"),
        Event::Seeked { to } => format!("seeked to {:.3}", to.as_secs_f64()),
        other => format!("{other:?}").to_lowercase(),
    };
    println!("[{:>8.3}] {what}", at.as_secs_f64());
}

// --- Screenshot -----------------------------------------------------------------------

fn screenshot(args: &Args) -> AnyResult<()> {
    let output = args.screenshot.as_ref().expect("screenshot path");
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(&instance, None))?;
    describe_adapter(&adapter);
    let player = open_player(args, &gpu::capabilities(&adapter, args.format)?)?;
    describe_tracks(&player);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("praxi-viewer"),
        required_features: gpu::required_features(&adapter),
        ..Default::default()
    }))?;

    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(&device, target_format, &player);
    renderer.upload(&queue, &player);
    let video = player.tracks()[0].params;
    let (width, height) = args.size.unwrap_or((video.width, video.height));
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let row = width * 4;
    let padded_row = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screenshot"),
        size: padded_row as u64 * height as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    let rect = Rect::fit(video.width, video.height, width, height);
    renderer.draw(&mut encoder, &target.create_view(&Default::default()), rect, false);
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map screenshot buffer"));
    device.poll(wgpu::PollType::wait_indefinitely())?;

    let mapped = buffer.slice(..).get_mapped_range()?;
    let mut rgba = Vec::with_capacity((row * height) as usize);
    for line in mapped.chunks(padded_row as usize) {
        rgba.extend_from_slice(&line[..row as usize]);
    }
    let mut encoder = png::Encoder::new(File::create(output)?, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    let at = player.video_frame(0).map_or(Duration::ZERO, |f| f.time);
    println!("{}: frame at {:.3} s, {width}x{height}", output.display(), at.as_secs_f64());
    Ok(())
}

// --- Window ---------------------------------------------------------------------------

fn run_window(args: Args) -> AnyResult<()> {
    let event_loop = EventLoop::new()?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(
        event_loop.owned_display_handle(),
    )));
    let mut app = App { args, instance, viewer: None, error: None };
    event_loop.run_app(&mut app)?;
    if let Some(viewer) = &app.viewer {
        viewer.stats.print();
    }
    match app.error {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

struct App {
    args: Args,
    instance: wgpu::Instance,
    viewer: Option<Viewer>,
    error: Option<String>,
}

impl App {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: impl ToString) {
        self.error = Some(error.to_string());
        event_loop.exit();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.viewer.is_none() {
            match Viewer::new(&self.args, &self.instance, event_loop) {
                Ok(viewer) => self.viewer = Some(viewer),
                Err(e) => self.fail(event_loop, e),
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(viewer) = &mut self.viewer else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => viewer.resize(size.width, size.height),
            WindowEvent::ModifiersChanged(m) => viewer.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => match viewer.key(&event) {
                Ok(true) => event_loop.exit(),
                Ok(false) => {}
                Err(e) => self.fail(event_loop, e),
            },
            WindowEvent::RedrawRequested => {
                if let Err(e) = viewer.redraw() {
                    self.fail(event_loop, e);
                }
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct Stats {
    frames: u64,
    advance: Duration,
    advance_max: Duration,
    upload: Duration,
    upload_max: Duration,
    uploads: u64,
}

impl Stats {
    fn print(&self) {
        if self.frames == 0 {
            return;
        }
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let n = self.frames as u32;
        println!(
            "{} frames presented, {} uploads; advance avg {:.3} ms max {:.3} ms; upload avg {:.3} ms max {:.3} ms",
            self.frames,
            self.uploads,
            ms(self.advance / n),
            ms(self.advance_max),
            ms(self.upload / n),
            ms(self.upload_max)
        );
    }
}

struct Viewer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    player: FilePlayer,
    name: String,
    muted: Arc<AtomicBool>,
    #[cfg(feature = "audio")]
    audio: Option<audio::Stream>,
    /// Whether audio track 0 drives the clock.
    #[cfg(feature = "audio")]
    audio_clock: bool,
    modifiers: ModifiersState,
    last: Instant,
    title_at: Instant,
    stats: Stats,
}

impl Viewer {
    fn new(args: &Args, instance: &wgpu::Instance, event_loop: &ActiveEventLoop) -> AnyResult<Viewer> {
        let name = args.file.file_name().map_or_else(|| args.file.display().to_string(), |n| n.to_string_lossy().into());
        let window = Arc::new(
            event_loop.create_window(Window::default_attributes().with_title(format!("praxi-viewer — {name}")))?,
        );
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(instance, Some(&surface)))?;
        describe_adapter(&adapter);

        #[cfg(feature = "audio")]
        let device_audio = if args.no_audio { None } else { audio::Device::open_default() };
        let caps = gpu::capabilities(&adapter, args.format)?;
        #[cfg(feature = "audio")]
        let caps = match device_audio.as_ref().and_then(|d| d.channels()) {
            Some(channels) => caps.max_audio_channels(channels),
            None => caps,
        };
        let mut player = open_player(args, &caps)?;
        describe_tracks(&player);

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("praxi-viewer"),
            required_features: gpu::required_features(&adapter),
            ..Default::default()
        }))?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("the window surface is not supported by the adapter")?;
        let surface_caps = surface.get_capabilities(&adapter);
        if let Some(&srgb) = surface_caps.formats.iter().find(|f| f.is_srgb()) {
            config.format = srgb;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);
        let renderer = Renderer::new(&device, config.format, &player);

        let video = player.tracks()[0].params;
        let fit = Rect::fit(video.width, video.height, 1280, 800);
        let scale = (fit.w / video.width as f32).min(1.0);
        let _ = window.request_inner_size(winit::dpi::LogicalSize::new(
            (video.width as f32 * scale).round(),
            (video.height as f32 * scale).round(),
        ));

        let muted = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "audio")]
        let audio = device_audio.and_then(|device| start_audio(&device, &player, muted.clone()));
        #[cfg(feature = "audio")]
        let audio_clock = audio.is_some() && args.clock == ClockArg::Audio;
        if !args.paused {
            player.play();
        }
        let now = Instant::now();
        Ok(Viewer {
            window,
            surface,
            device,
            queue,
            config,
            renderer,
            player,
            name,
            muted,
            #[cfg(feature = "audio")]
            audio,
            #[cfg(feature = "audio")]
            audio_clock,
            modifiers: ModifiersState::empty(),
            last: now,
            title_at: now,
            stats: Stats::default(),
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Handles a key; returns true to quit.
    fn key(&mut self, event: &KeyEvent) -> AnyResult<bool> {
        if event.state != ElementState::Pressed {
            return Ok(false);
        }
        let p = &mut self.player;
        let step = Duration::from_secs(if self.modifiers.shift_key() { 5 } else { 1 });
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => return Ok(true),
            Key::Character(c) if c.eq_ignore_ascii_case("q") => return Ok(true),
            Key::Named(NamedKey::Space) if p.is_playing() => p.pause(),
            Key::Named(NamedKey::Space) => p.play(),
            Key::Named(NamedKey::ArrowLeft) => p.seek(p.position().saturating_sub(step))?,
            Key::Named(NamedKey::ArrowRight) => p.seek((p.position() + step).min(p.duration()))?,
            Key::Named(NamedKey::Home) => p.seek(Duration::ZERO)?,
            Key::Character(c) if c.eq_ignore_ascii_case("l") => {
                p.set_looping(!p.looping());
                println!("looping {}", if p.looping() { "on" } else { "off" });
            }
            Key::Character(c) if c.eq_ignore_ascii_case("m") => {
                let muted = !self.muted.fetch_xor(true, Ordering::Relaxed);
                println!("{}", if muted { "muted" } else { "unmuted" });
            }
            _ => {}
        }
        self.drain_events();
        Ok(false)
    }

    fn drain_events(&mut self) {
        while let Some(event) = self.player.poll_event() {
            print_event(self.player.position(), &event);
        }
    }

    fn redraw(&mut self) -> AnyResult<()> {
        let now = Instant::now();
        let dt = now - self.last;
        self.last = now;
        #[cfg(feature = "audio")]
        if let Some(audio) = self.audio.as_ref().filter(|_| self.audio_clock) {
            self.player.set_clock(Clock::Audio { track: 0, latency: audio.latency() });
        }
        self.player.advance(dt)?;
        let advanced = Instant::now();
        self.drain_events();
        let uploads = self.renderer.upload(&self.queue, &self.player);
        let uploaded = Instant::now();

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            _ => {
                self.window.request_redraw();
                return Ok(());
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let video = self.player.tracks()[0].params;
        let rect = Rect::fit(video.width, video.height, self.config.width, self.config.height);
        self.renderer.draw(&mut encoder, &view, rect, true);
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);

        let s = &mut self.stats;
        s.frames += 1;
        s.uploads += uploads as u64;
        s.advance += advanced - now;
        s.advance_max = s.advance_max.max(advanced - now);
        s.upload += uploaded - advanced;
        s.upload_max = s.upload_max.max(uploaded - advanced);
        if now - self.title_at >= Duration::from_millis(250) {
            self.title_at = now;
            self.window.set_title(&self.title());
        }
        self.window.request_redraw();
        Ok(())
    }

    fn title(&self) -> String {
        let p = &self.player;
        let state = if p.is_finished() {
            "finished"
        } else if p.is_playing() {
            "playing"
        } else {
            "paused"
        };
        let looping = if p.looping() { ", loop" } else { "" };
        format!(
            "{} — {:.2} / {:.2} s — {state}{looping} — {}",
            self.name,
            p.position().as_secs_f64(),
            p.duration().as_secs_f64(),
            p.tracks()[0].params.format
        )
    }
}

/// Starts the audio stream for every track matching audio track 0, or prints why not.
#[cfg(feature = "audio")]
fn start_audio(device: &audio::Device, player: &FilePlayer, muted: Arc<AtomicBool>) -> Option<audio::Stream> {
    let tracks = player.audio_tracks();
    let first = &tracks.first()?.params;
    let mut outputs = Vec::new();
    for (i, t) in tracks.iter().enumerate() {
        if t.params.sample_rate == first.sample_rate && t.params.channels == first.channels {
            outputs.extend(player.audio_output(i));
        } else {
            println!(
                "audio {i}: skipped, {} Hz {} ch differs from audio 0 ({} Hz {} ch)",
                t.params.sample_rate, t.params.channels, first.sample_rate, first.channels
            );
        }
    }
    match device.start(outputs, muted) {
        Ok(stream) => {
            println!("audio device: {}, {} Hz, {} ch", device.name(), first.sample_rate, first.channels);
            Some(stream)
        }
        Err(e) => {
            println!("audio device: {}: {e}; playing without sound", device.name());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arguments() {
        assert_eq!(parse_format("astc6x6"), Ok(TextureFormat::Astc6x6));
        assert_eq!(parse_format("BC7"), Ok(TextureFormat::Bc7));
        assert!(parse_format("etc2").is_err());
        assert_eq!(parse_size("640x480"), Ok((640, 480)));
        assert!(parse_size("640").is_err());
        assert!(parse_size("0x480").is_err());
    }
}
