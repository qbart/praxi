//! Decoding on a worker thread, with the next video prerolled for a seamless cut
//! (`docs/features/engine-integration.md`).
//!
//! ```sh
//! cargo run --release -p praxi --example decode_thread -- intro.praxi [next.praxi]
//! ```
//!
//! The game thread sends each frame's `dt` to the worker and, while the worker decodes,
//! handles what the worker produced for the previous frame: frames to upload (in recycled
//! buffers) and events. Decoding never runs on the game thread.

use praxi::{Capabilities, Event, Player, TextureFormat};
use std::fs::File;
use std::io::BufReader;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_micros(16_667); // 60 Hz

type FilePlayer = Player<BufReader<File>>;

enum ToWorker {
    Advance(Duration),
    Quit,
}

enum ToGame {
    /// A new frame of video `video`'s track 0.
    Frame { video: usize, index: u64, data: Vec<u8> },
    Event { video: usize, event: Event },
    /// Everything for one `Advance` was sent.
    Advanced,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() || paths.len() > 2 {
        return Err("usage: decode_thread FILE [NEXT]".into());
    }
    let caps = Capabilities::new(TextureFormat::ALL);
    let players = paths
        .iter()
        .map(|p| Ok(Player::open(BufReader::new(File::open(p)?), &caps)?))
        .collect::<Result<Vec<FilePlayer>, Box<dyn std::error::Error>>>()?;

    let (to_worker, commands) = channel();
    let (results, from_worker) = channel();
    let (recycle, free_buffers) = channel::<Vec<u8>>();
    let worker = thread::spawn(move || decode(players, commands, results, free_buffers));

    // Game state: the frame index each video's texture holds, and which video is on screen.
    let mut textures: Vec<Option<u64>> = vec![None; paths.len()];
    let mut showing = 0;
    let (mut frames, mut waited) = (0u32, Duration::ZERO);
    let mut frames_without_picture = 0;
    let mut done = false;
    let mut last = Instant::now();
    to_worker.send(ToWorker::Advance(Duration::ZERO))?;
    while !done {
        let now = Instant::now();
        let dt = now - last;
        last = now;
        // Start decoding this frame, then handle the previous frame's results meanwhile.
        to_worker.send(ToWorker::Advance(dt))?;
        loop {
            let t = Instant::now();
            let message = from_worker.recv()?;
            waited += t.elapsed();
            match message {
                ToGame::Frame { video, index, data } => {
                    // A real engine uploads `data` to video `video`'s texture here.
                    textures[video] = Some(index);
                    let _ = recycle.send(data);
                }
                ToGame::Event { video, event } => {
                    println!("[frame {frames:5}] video {video}: {event:?}");
                    if event == Event::Finished {
                        if video + 1 < paths.len() {
                            // The next video's first frame is already uploaded: cut now.
                            showing = video + 1;
                        } else {
                            done = true;
                        }
                    }
                }
                ToGame::Advanced => break,
            }
        }
        if textures[showing].is_none() {
            frames_without_picture += 1;
        }
        frames += 1;
        thread::sleep(FRAME.saturating_sub(now.elapsed()));
    }
    to_worker.send(ToWorker::Quit)?;
    worker.join().expect("worker thread")?;

    println!("{frames} game frames, {frames_without_picture} without a picture on screen");
    // Near zero: the previous frame's decode finished during this frame's sleep.
    println!("game thread waited for the worker {:.3} ms per frame", waited.as_secs_f64() * 1e3 / frames as f64);
    Ok(())
}

/// The worker: owns the players, advances the current one and sends what changed.
fn decode(
    mut players: Vec<FilePlayer>,
    commands: Receiver<ToWorker>,
    results: Sender<ToGame>,
    free_buffers: Receiver<Vec<u8>>,
) -> Result<(), String> {
    let mut sent: Vec<Option<u64>> = vec![None; players.len()];
    let send_frame = |player: &FilePlayer, video: usize, sent: &mut Vec<Option<u64>>| {
        let Some(frame) = player.video_frame(0) else { return };
        if sent[video] == Some(frame.index) {
            return;
        }
        sent[video] = Some(frame.index);
        let mut data = free_buffers.try_recv().unwrap_or_default();
        data.clear();
        data.extend_from_slice(frame.data);
        let _ = results.send(ToGame::Frame { video, index: frame.index, data });
    };

    // Preroll every video: seeking decodes its first frame, which goes to the game thread
    // now, long before it is shown.
    for (video, player) in players.iter_mut().enumerate() {
        player.seek(Duration::ZERO).map_err(|e| e.to_string())?;
        while player.poll_event().is_some() {}
        send_frame(player, video, &mut sent);
    }
    let mut current = 0;
    players[0].play().map_err(|e| e.to_string())?;

    let started = Instant::now();
    let mut decode_time = Duration::ZERO;
    while let Ok(ToWorker::Advance(dt)) = commands.recv() {
        let t = Instant::now();
        let player = &mut players[current];
        player.advance(dt).map_err(|e| e.to_string())?;
        let mut finished = false;
        while let Some(event) = player.poll_event() {
            finished |= event == Event::Finished;
            let _ = results.send(ToGame::Event { video: current, event });
        }
        send_frame(player, current, &mut sent);
        if finished && current + 1 < players.len() {
            current += 1;
            players[current].play().map_err(|e| e.to_string())?;
        }
        decode_time += t.elapsed();
        let _ = results.send(ToGame::Advanced);
    }
    println!(
        "worker: {:.1}% busy decoding",
        100.0 * decode_time.as_secs_f64() / started.elapsed().as_secs_f64()
    );
    Ok(())
}
