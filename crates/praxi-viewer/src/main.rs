//! `praxi-viewer`: plays a `.praxi` file on the GPU (`docs/features/viewer.md`).

#[cfg(feature = "audio")]
use praxi_audio as audio;
mod checker;

use checker::Checker;
use clap::Parser;
#[cfg(feature = "audio")]
use praxi::Clock;
use praxi::{Capabilities, CueTrack, Event, Player, TextureFormat, Value};
use praxi_wgpu::{Rect, VideoRenderer};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;
type FilePlayer = Player<BufReader<File>>;

#[derive(Parser)]
#[command(
    name = "praxi-viewer",
    version,
    about = "Plays a praxi file on the GPU"
)]
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
    /// Play the cues of this cue sheet in place of the file's, reloading it when it changes (repeatable).
    #[arg(long = "cues", value_name = "SHEET")]
    cues: Vec<PathBuf>,
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
    TextureFormat::parse(s).ok_or_else(|| format!("unknown format {s:?}"))
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
        if args.screenshot.is_some() {
            screenshot(&args)
        } else {
            run_window(args)
        }
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
        println!(
            "video {i}: stream {}, {} {}x{}{alpha}",
            t.stream, p.format, p.width, p.height
        );
    }
    for (i, t) in player.audio_tracks().iter().enumerate() {
        let p = &t.params;
        println!(
            "audio {i}: stream {}, {:?}, {} Hz, {} ch, {}",
            t.stream, p.name, p.sample_rate, p.channels, p.format
        );
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

/// The `--cues` sheets (`docs/features/cue-preview.md`).
struct CueSheets {
    paths: Vec<PathBuf>,
    /// Modification times when last read.
    modified: Vec<Option<SystemTime>>,
}

impl CueSheets {
    fn new(paths: Vec<PathBuf>) -> CueSheets {
        let modified = vec![None; paths.len()];
        CueSheets { paths, modified }
    }

    fn modified_now(&self) -> Vec<Option<SystemTime>> {
        self.paths
            .iter()
            .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
            .collect()
    }

    /// Whether a sheet that exists now changed since it was last read. A sheet missing for
    /// a moment, as editors replace files, is not a change.
    fn changed(&self) -> bool {
        self.modified_now()
            .iter()
            .zip(&self.modified)
            .any(|(now, then)| now.is_some() && now != then)
    }

    /// Reads every sheet and hands their cues to `player`; on error the player keeps its cues.
    fn load(&mut self, player: &mut FilePlayer) -> Result<(), String> {
        // Taken before reading, so a save during the read triggers another load.
        self.modified = self.modified_now();
        let mut tracks: Vec<CueTrack> = Vec::new();
        for path in &self.paths {
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            tracks.extend(
                praxi::format::cue_sheet::parse(&text)
                    .map_err(|e| format!("{}: {e}", path.display()))?,
            );
        }
        let cues: usize = tracks.iter().map(|t| t.cues.len()).sum();
        let duration = player.duration().as_micros() as u64;
        let late = tracks
            .iter()
            .flat_map(|t| &t.cues)
            .filter(|c| c.time_us.saturating_add(c.cue.duration) > duration)
            .count();
        let names: Vec<String> = self.paths.iter().map(|p| p.display().to_string()).collect();
        let mut summary = format!(
            "cues: {} streams, {cues} cues from {}",
            tracks.len(),
            names.join(", ")
        );
        if late > 0 {
            summary +=
                &format!("; {late} past the end of the file (import would extend its duration)");
        }
        println!("{summary}");
        player.replace_cues(tracks);
        Ok(())
    }
}

/// A render pass that clears `view` to transparent black.
fn begin_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("praxi-viewer"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    })
}

// --- Screenshot -----------------------------------------------------------------------

fn screenshot(args: &Args) -> AnyResult<()> {
    let output = args.screenshot.as_ref().expect("screenshot path");
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(
        &instance, None,
    ))?;
    describe_adapter(&adapter);
    let mut player = open_player(args, &praxi_wgpu::capabilities(&adapter, args.format)?)?;
    describe_tracks(&player);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("praxi-viewer"),
        required_features: praxi_wgpu::required_features(&adapter),
        required_limits: praxi_wgpu::required_limits(&adapter, wgpu::Limits::default()),
        ..Default::default()
    }))?;

    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = VideoRenderer::new(&device, target_format, &player);
    renderer.prepare(&device, &queue, &mut player, 1.0);
    let video = player.tracks()[0].params;
    let (width, height) = args.size.unwrap_or((video.width, video.height));
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
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
    {
        let view = target.create_view(&Default::default());
        let mut pass = begin_pass(&mut encoder, &view);
        renderer.draw_replace(&mut pass, rect);
    }
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
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map screenshot buffer"));
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
    println!(
        "{}: frame at {:.3} s, {width}x{height}",
        output.display(),
        at.as_secs_f64()
    );
    Ok(())
}

// --- Window ---------------------------------------------------------------------------

fn run_window(args: Args) -> AnyResult<()> {
    let event_loop = EventLoop::new()?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
        Box::new(event_loop.owned_display_handle()),
    ));
    let mut app = App {
        args,
        instance,
        viewer: None,
        error: None,
    };
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
        let Some(viewer) = &mut self.viewer else {
            return;
        };
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
    renderer: VideoRenderer,
    checker: Checker,
    player: FilePlayer,
    name: String,
    muted: Arc<AtomicBool>,
    sheets: Option<CueSheets>,
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
    fn new(
        args: &Args,
        instance: &wgpu::Instance,
        event_loop: &ActiveEventLoop,
    ) -> AnyResult<Viewer> {
        let name = args.file.file_name().map_or_else(
            || args.file.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        let window = Arc::new(event_loop.create_window(
            Window::default_attributes().with_title(format!("praxi-viewer — {name}")),
        )?);
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(
            instance,
            Some(&surface),
        ))?;
        describe_adapter(&adapter);

        #[cfg(feature = "audio")]
        let device_audio = if args.no_audio {
            None
        } else {
            audio::Device::open_default()
        };
        let caps = praxi_wgpu::capabilities(&adapter, args.format)?;
        #[cfg(feature = "audio")]
        let caps = match device_audio.as_ref().and_then(|d| d.channels()) {
            Some(channels) => caps.max_audio_channels(channels),
            None => caps,
        };
        let mut player = open_player(args, &caps)?;
        describe_tracks(&player);
        let mut sheets = (!args.cues.is_empty()).then(|| CueSheets::new(args.cues.clone()));
        if let Some(sheets) = &mut sheets {
            sheets.load(&mut player)?;
        }

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("praxi-viewer"),
                required_features: praxi_wgpu::required_features(&adapter),
                required_limits: praxi_wgpu::required_limits(&adapter, wgpu::Limits::default()),
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
        let renderer = VideoRenderer::new(&device, config.format, &player);
        let checker = Checker::new(&device, config.format);

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
            player.play()?;
        }
        let now = Instant::now();
        Ok(Viewer {
            window,
            surface,
            device,
            queue,
            config,
            renderer,
            checker,
            player,
            name,
            muted,
            sheets,
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
            Key::Named(NamedKey::Space) => p.play()?,
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
            Key::Character(c) if c.eq_ignore_ascii_case("r") => self.reload_cues(),
            _ => {}
        }
        self.drain_events();
        Ok(false)
    }

    /// Reloads the `--cues` sheets, printing errors.
    fn reload_cues(&mut self) {
        if let Some(sheets) = &mut self.sheets
            && let Err(e) = sheets.load(&mut self.player)
        {
            println!("cues: {e}; keeping the previous cues");
        }
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
            self.player.set_clock(Clock::Audio {
                track: 0,
                latency: audio.latency(),
            });
        }
        self.player.advance(dt)?;
        let advanced = Instant::now();
        self.drain_events();
        let uploads = self
            .renderer
            .prepare(&self.device, &self.queue, &mut self.player, 1.0);
        let uploaded = Instant::now();

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
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
        let rect = Rect::fit(
            video.width,
            video.height,
            self.config.width,
            self.config.height,
        );
        {
            let mut pass = begin_pass(&mut encoder, &view);
            self.checker.draw(&mut pass, rect);
            self.renderer.draw(&mut pass, rect);
        }
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
            if self.sheets.as_ref().is_some_and(CueSheets::changed) {
                self.reload_cues();
                self.drain_events();
            }
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

/// Starts the audio stream for every audio track, converted to the device's format where
/// needed, or prints why not.
#[cfg(feature = "audio")]
fn start_audio(
    device: &audio::Device,
    player: &FilePlayer,
    muted: Arc<AtomicBool>,
) -> Option<audio::Stream> {
    let tracks = player.audio_tracks();
    let outputs: Vec<_> = (0..tracks.len())
        .filter_map(|i| player.audio_output(i))
        .collect();
    if outputs.is_empty() {
        return None;
    }
    match device.start(outputs, muted) {
        Ok(stream) => {
            let (rate, channels) = stream.config();
            println!("audio device: {}, {rate} Hz, {channels} ch", device.name());
            for (i, t) in tracks.iter().enumerate() {
                if (t.params.sample_rate, t.params.channels) != (rate, channels) {
                    println!(
                        "audio {i}: converted from {} Hz {} ch",
                        t.params.sample_rate, t.params.channels
                    );
                }
            }
            Some(stream)
        }
        Err(e) => {
            println!(
                "audio device: {}: {e}; playing without sound",
                device.name()
            );
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
        assert_eq!(parse_format("motion"), Ok(TextureFormat::Motion));
        assert!(parse_format("etc2").is_err());
        assert_eq!(parse_size("640x480"), Ok((640, 480)));
        assert!(parse_size("640").is_err());
        assert!(parse_size("0x480").is_err());
    }
}
