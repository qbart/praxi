//! Times the GPU decoder on the motion track of a file
//! (`docs/features/motion-codec.md#tools`).
//!
//! `cargo run --release -p praxi-wgpu --example motion_timing -- FILE`

use praxi::format::Reader;
use praxi::{Capabilities, Player, TextureFormat};
use std::fs::File;
use std::io::BufReader;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: motion_timing FILE")?;
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(
        &instance, None,
    ))?;
    let info = adapter.get_info();
    println!("adapter: {} ({:?})", info.name, info.backend);
    if !praxi_wgpu::supports_motion(&adapter) {
        return Err("the adapter cannot run the motion decoder".into());
    }
    let required_limits = praxi_wgpu::required_limits(&adapter, wgpu::Limits::default());
    let required_features = adapter.features() & wgpu::Features::TIMESTAMP_QUERY;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features,
        required_limits,
        ..Default::default()
    }))?;
    let caps = Capabilities::new([TextureFormat::Motion]);
    let mut player = Player::open(BufReader::new(File::open(&path)?), &caps)?;
    let track = player.tracks()[0];
    println!("{}x{}", track.params.width, track.params.height);
    // Whether each frame of the track is a keyframe, in decoding order.
    let mut reader = Reader::new(BufReader::new(File::open(&path)?))?;
    let mut is_keyframe = Vec::new();
    while let Some(chunk) = reader.next_chunk()? {
        if chunk.stream == track.stream {
            is_keyframe.push(chunk.keyframe());
        }
    }
    let mut renderer =
        praxi_wgpu::VideoRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb, &player);
    let timed = renderer.set_gpu_timing(&device, true);
    if !timed {
        println!("no timestamp queries: wall times only");
    }
    let (mut keyframes, mut deltas) = (Vec::new(), Vec::new());
    // GPU times of the passes: prediction, residual, color; keyframes, then delta frames.
    let mut passes: [[Vec<f64>; 3]; 2] = Default::default();
    for pass in 0..3 {
        player.seek(Duration::ZERO)?;
        renderer.prepare(&device, &queue, &mut player, 1.0);
        device.poll(wgpu::PollType::wait_indefinitely())?;
        // The first frame is not measured.
        renderer.gpu_times(&device, &queue);
        player.play()?;
        let mut last = player.video_frame(0).map(|f| f.index);
        let mut n = 0;
        while !player.is_finished() {
            // Step to the next frame of the track, whatever its rate.
            player.advance(Duration::from_millis(1))?;
            let frame = player.video_frame(0).map(|f| f.index);
            if frame == last {
                continue;
            }
            last = frame;
            n += 1;
            let keyframe = is_keyframe.get(n).copied().unwrap_or(false);
            let start = Instant::now();
            renderer.prepare(&device, &queue, &mut player, 1.0);
            device.poll(wgpu::PollType::wait_indefinitely())?;
            let ms = start.elapsed().as_secs_f64() * 1e3;
            let gpu = renderer.gpu_times(&device, &queue);
            if pass > 0 {
                if keyframe {
                    keyframes.push(ms)
                } else {
                    deltas.push(ms)
                }
                for t in gpu {
                    let kind = &mut passes[usize::from(!t.keyframe)];
                    let color = t.color.unwrap_or_default();
                    for (times, d) in kind.iter_mut().zip([t.predict, t.residual, color]) {
                        times.push(d.as_secs_f64() * 1e3);
                    }
                }
            }
        }
    }
    println!("wall time of prepare and the wait for the GPU:");
    for (name, times) in [("keyframes", keyframes), ("delta frames", deltas)] {
        print_stats(name, times);
    }
    if timed {
        println!("GPU time per pass:");
        for (kind, stages) in ["keyframes", "delta frames"].iter().zip(passes) {
            for (stage, times) in ["prediction", "residual", "color"].iter().zip(stages) {
                print_stats(&format!("{kind}, {stage}"), times);
            }
        }
    }
    Ok(())
}

fn print_stats(name: &str, mut times: Vec<f64>) {
    if times.is_empty() {
        return;
    }
    times.sort_by(f64::total_cmp);
    let at = |q: usize| times[(times.len() - 1) * q / 100];
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    println!(
        "  {name}: {} frames, mean {mean:.3} ms, median {:.3}, p95 {:.3}, max {:.3}",
        times.len(),
        at(50),
        at(95),
        at(100)
    );
}
