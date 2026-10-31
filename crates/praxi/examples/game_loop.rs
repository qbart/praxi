//! A game loop playing a praxi file, with stand-ins for the GPU and the audio device
//! (`docs/features/engine-integration.md`).
//!
//! ```sh
//! cargo run --release -p praxi --example game_loop -- intro.praxi
//! ```

use praxi::{AudioOutput, Capabilities, Event, Player, TextureFormat};
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_micros(16_667); // 60 Hz

/// Stands in for a GPU texture: remembers which frame it holds.
#[derive(Default)]
struct Texture {
    index: Option<u64>,
    uploads: u64,
    bytes: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: game_loop FILE")?;

    // What the GPU samples, most preferred first. A real engine asks its GPU; this one
    // pretends to support everything, BC before ASTC like a desktop GPU.
    let mut formats = TextureFormat::ALL.to_vec();
    formats.sort_by_key(|f| f.name().starts_with("ASTC"));
    let caps = Capabilities::new(formats).max_audio_channels(2);
    let mut player = Player::open(BufReader::new(File::open(&path)?), &caps)?;

    for (i, t) in player.tracks().iter().enumerate() {
        let p = &t.params;
        println!("video {i}: {} {}x{} (stream {})", p.format, p.width, p.height, t.stream);
    }
    let mut textures: Vec<Texture> = player.tracks().iter().map(|_| Texture::default()).collect();

    // The audio device pulls samples from its own thread, 10 ms at a time.
    let stop = Arc::new(AtomicBool::new(false));
    let audio_frames = Arc::new(AtomicU64::new(0));
    let device = player.audio_output(0).map(|out| {
        println!("audio 0: {} Hz, {} channels", out.sample_rate(), out.channels());
        let (stop, frames) = (stop.clone(), audio_frames.clone());
        thread::spawn(move || audio_device(out, &stop, &frames))
    });

    player.play()?;
    let start = Instant::now();
    let (mut frames, mut advance_total, mut advance_max) = (0u32, Duration::ZERO, Duration::ZERO);
    let mut last = start;
    while !player.is_finished() {
        let now = Instant::now();
        let dt = now - last;
        last = now;

        // 1. Decode what became due.
        player.advance(dt)?;
        let spent = now.elapsed();
        advance_total += spent;
        advance_max = advance_max.max(spent);

        // 2. React to events.
        while let Some(event) = player.poll_event() {
            let at = player.position().as_secs_f64();
            match event {
                Event::Cue(cue) => {
                    let fields: Vec<String> = cue.fields.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
                    println!("[{at:7.3}] cue {} {}", cue.kind, fields.join(" "));
                }
                Event::CueEnd { id, .. } => println!("[{at:7.3}] cue {id} ended"),
                other => println!("[{at:7.3}] {other:?}"),
            }
        }

        // 3. Upload frames that changed. A real engine calls its texture upload here
        //    (`queue.write_texture`, `glCompressedTexSubImage2D`, ...) with `frame.data`.
        for (track, texture) in textures.iter_mut().enumerate() {
            if let Some(frame) = player.video_frame(track)
                && texture.index != Some(frame.index)
            {
                texture.index = Some(frame.index);
                texture.uploads += 1;
                texture.bytes += frame.data.len() as u64;
            }
        }

        // 4. Render, then wait for the next frame.
        frames += 1;
        thread::sleep(FRAME.saturating_sub(now.elapsed()));
    }

    stop.store(true, Ordering::Relaxed);
    if let Some(device) = device {
        device.join().expect("audio thread");
    }
    let elapsed = start.elapsed().as_secs_f64();
    println!("{frames} game frames in {elapsed:.2} s");
    for (i, t) in textures.iter().enumerate() {
        println!("video {i}: {} uploads, {:.1} MB, {} game frames without upload", t.uploads, t.bytes as f64 / 1e6, frames as u64 - t.uploads);
    }
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    println!("advance: {:.3} ms average, {:.3} ms max", ms(advance_total / frames.max(1)), ms(advance_max));
    if let Some(out) = player.audio_output(0) {
        let read = audio_frames.load(Ordering::Relaxed) as f64 / out.sample_rate() as f64;
        println!("audio: {read:.2} s read of {:.2} s", player.duration().as_secs_f64());
    }
    Ok(())
}

/// Reads 10 ms buffers in real time until `stop`, counting the frames it got.
fn audio_device(out: AudioOutput, stop: &AtomicBool, frames: &AtomicU64) {
    let frames_per_buffer = out.sample_rate() as usize / 100;
    let mut buf = vec![0.0f32; frames_per_buffer * out.channels()];
    let start = Instant::now();
    let mut buffers = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let n = out.read(&mut buf);
        buf[n..].fill(0.0); // a device plays silence for what is missing
        frames.fetch_add((n / out.channels()) as u64, Ordering::Relaxed);
        buffers += 1;
        thread::sleep((start + Duration::from_millis(10) * buffers).saturating_duration_since(Instant::now()));
    }
}
