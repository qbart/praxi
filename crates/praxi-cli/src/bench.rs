//! `praxi bench`: size, quality and decode time per variant (`docs/features/benchmarks.md`).

use crate::{AnyResult, VideoOptions, probe, pump_frames, spawn_video, value_name, video_config};
use praxi::{Capabilities, Player};
use praxi_encoder::Encoder;
use praxi_format::{Reader, TextureFormat, motion};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `version` of the JSON results (`docs/spec/BENCH.md`).
const VERSION: u32 = 1;
const PSNR_CAP: f64 = 100.0;
/// Slack for limits, so that a change of exactly the limit passes despite rounding.
const EPSILON: f64 = 1e-9;

#[derive(clap::Args)]
pub struct BenchArgs {
    input: PathBuf,
    #[command(flatten)]
    video: VideoOptions,
    /// Playbacks per variant for decode timing.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..))]
    passes: u32,
    /// Write the results as JSON.
    #[arg(long)]
    json: Option<PathBuf>,
    /// Compare with earlier results and fail on regressions.
    #[arg(long)]
    baseline: Option<PathBuf>,
    /// Allowed growth of a variant's bytes over the baseline, in percent.
    #[arg(long, default_value_t = 1.0)]
    max_size_increase: f64,
    /// Allowed PSNR drop below the baseline, in dB.
    #[arg(long, default_value_t = 0.1)]
    max_psnr_drop: f64,
    /// Keep the encoded file here.
    #[arg(long)]
    keep: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Results {
    pub version: u32,
    pub input: String,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub frames: u64,
    pub settings: Settings,
    pub encode_seconds: f64,
    pub variants: Vec<VariantResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub quality: String,
    pub rdo: f64,
    pub keyint: u32,
    pub skip_threshold: u8,
    #[serde(default)]
    pub plane_min_saving: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariantResult {
    pub format: String,
    pub bytes: u64,
    pub mb_per_s: f64,
    pub skipped_pct: f64,
    pub keyframes: u64,
    pub psnr_db: f64,
    pub psnr_min_db: f64,
    pub decode_ms: DecodeMs,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DecodeMs {
    pub mean: f64,
    pub p95: f64,
    pub max: f64,
}

/// Deletes the encoded file unless it was asked to be kept.
struct Encoded {
    path: PathBuf,
    keep: bool,
}

impl Drop for Encoded {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn bench(args: BenchArgs) -> AnyResult<()> {
    let baseline = args.baseline.as_deref().map(read_results).transpose()?;
    let p = probe(&args.input)?;
    let frame_bytes = p.width as usize * p.height as usize * 4;
    let encoded = Encoded {
        path: args
            .keep
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join(format!("praxi-bench-{}.praxi", std::process::id()))),
        keep: args.keep.is_some(),
    };

    eprintln!("encoding {} ({}x{} @ {}/{} fps)", args.input.display(), p.width, p.height, p.fps_num, p.fps_den);
    let started = Instant::now();
    let mut encoder = Encoder::new(BufWriter::new(File::create(&encoded.path)?), video_config(&p, &args.video))?;
    pump_frames(spawn_video(&args.input, args.video.frames)?, frame_bytes, |frame| {
        encoder.push_frame(frame)?;
        Ok(())
    })?;
    let (_, stats) = encoder.finish()?;
    let encode_seconds = started.elapsed().as_secs_f64();
    let frame_us = |i: u64| (i as u128 * p.fps_den as u128 * 1_000_000 / p.fps_num as u128) as u64;
    let formats: Vec<TextureFormat> = args.video.variants.iter().map(|v| v.format()).collect();

    eprintln!("timing decode, {} passes", args.passes);
    let mut decode = Vec::new();
    for &format in &formats {
        let mut times = Vec::new();
        for _ in 0..args.passes {
            if format == TextureFormat::Motion {
                // Entropy decoding only: reconstruction runs on the GPU.
                let mut m = MotionPlayback::open(&encoded.path)?;
                for _ in 0..stats.frames {
                    let start = Instant::now();
                    m.next()?;
                    times.push(start.elapsed());
                }
                continue;
            }
            let mut player = open_variant(&encoded.path, format)?;
            for i in 0..stats.frames {
                let dt = frame_us(i) - frame_us(i.saturating_sub(1));
                let start = Instant::now();
                // `play()` decodes the first frame.
                if i == 0 {
                    player.play()?;
                } else {
                    player.advance(Duration::from_micros(dt))?;
                }
                times.push(start.elapsed());
                check_frame(&player, i, frame_us(i))?;
            }
        }
        decode.push(decode_ms(&mut times));
    }

    eprintln!("measuring quality");
    let mut players = Vec::new();
    for &format in &formats {
        if format == TextureFormat::Motion {
            players.push(Playback::Motion(Box::new(MotionPlayback::open(&encoded.path)?)));
            continue;
        }
        let mut player = open_variant(&encoded.path, format)?;
        player.play()?;
        players.push(Playback::Blocks(Box::new(player)));
    }
    let mut quality: Vec<(f64, f64)> = vec![(0.0, PSNR_CAP); formats.len()]; // (summed MSE, worst frame PSNR)
    let channels = if p.alpha { 4 } else { 3 };
    let mut i = 0;
    let compared = pump_frames(spawn_video(&args.input, args.video.frames)?, frame_bytes, |source| {
        let dt = frame_us(i) - frame_us(i.saturating_sub(1));
        let mses = std::thread::scope(|scope| {
            let jobs: Vec<_> = players
                .iter_mut()
                .map(|player| {
                    scope.spawn(move || {
                        let mut compare = || -> AnyResult<f64> {
                            let player = match player {
                                Playback::Motion(m) => {
                                    m.next()?;
                                    return Ok(mse(source, m.rgba(), channels));
                                }
                                Playback::Blocks(player) => player,
                            };
                            player.advance(Duration::from_micros(dt))?;
                            check_frame(player, i, frame_us(i))?;
                            let params = player.tracks()[0].params;
                            let frame = player.video_frame(0).ok_or("no frame")?;
                            let rgba = praxi_encoder::decode_frame(&params, frame.data)?;
                            Ok(mse(source, &rgba, channels))
                        };
                        compare().map_err(|e| e.to_string())
                    })
                })
                .collect();
            jobs.into_iter().map(|j| j.join().expect("quality thread")).collect::<Vec<_>>()
        });
        for (q, m) in quality.iter_mut().zip(mses) {
            let m = m?;
            q.0 += m;
            q.1 = q.1.min(psnr(m));
        }
        i += 1;
        Ok(())
    })?;
    if compared != stats.frames {
        return Err(format!("ffmpeg decoded {compared} frames the second time, {} the first", stats.frames).into());
    }

    let secs = (stats.duration_us as f64 / 1e6).max(1e-9);
    let variants = args
        .video
        .variants
        .iter()
        .zip(&stats.variants)
        .zip(quality.iter().zip(&decode))
        .map(|((&arg, v), (&(mse_sum, worst), &decode_ms))| VariantResult {
            format: value_name(arg),
            bytes: v.bytes,
            mb_per_s: v.bytes as f64 / 1e6 / secs,
            skipped_pct: 100.0 * v.skipped_blocks as f64 / v.blocks.max(1) as f64,
            keyframes: v.keyframes,
            psnr_db: psnr(mse_sum / stats.frames.max(1) as f64),
            psnr_min_db: worst,
            decode_ms,
        })
        .collect();
    let results = Results {
        version: VERSION,
        input: args.input.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        width: p.width,
        height: p.height,
        fps_num: p.fps_num,
        fps_den: p.fps_den,
        frames: stats.frames,
        settings: Settings {
            quality: value_name(args.video.quality),
            rdo: args.video.rdo as f64,
            keyint: args.video.keyint,
            skip_threshold: args.video.skip_threshold,
            plane_min_saving: args.video.plane_min_saving,
        },
        encode_seconds,
        variants,
    };
    print_results(&results);

    let regressions = match &baseline {
        Some(baseline) => compare(baseline, &results, args.max_size_increase, args.max_psnr_drop)?,
        None => Vec::new(),
    };
    if let Some(path) = &args.json {
        std::fs::write(path, serde_json::to_string_pretty(&results)? + "\n")?;
    }
    for r in &regressions {
        println!("regression: {r}");
    }
    if regressions.is_empty() { Ok(()) } else { Err(format!("{} regression(s)", regressions.len()).into()) }
}

enum Playback {
    Blocks(Box<Player<BufReader<File>>>),
    Motion(Box<MotionPlayback>),
}

/// Plays the motion stream of a file frame by frame: entropy decoding, and on request the
/// reference reconstruction.
struct MotionPlayback {
    reader: Reader<BufReader<File>>,
    stream: u16,
    decoder: motion::Decoder,
    frame: motion::CodedFrame,
    payload: Vec<u8>,
    planes: Vec<u8>,
    reference: Vec<u8>,
    rgba: Vec<u8>,
}

impl MotionPlayback {
    fn open(path: &Path) -> AnyResult<MotionPlayback> {
        let reader = Reader::new(BufReader::new(File::open(path)?))?;
        let (stream, params) = reader
            .streams()
            .iter()
            .enumerate()
            .find_map(|(i, s)| s.video_params().filter(|v| v.format == TextureFormat::Motion).map(|v| (i as u16, *v)))
            .ok_or("the encoded file has no motion track")?;
        let decoder = motion::Decoder::new(&params);
        let size = decoder.layout().size;
        Ok(MotionPlayback {
            reader,
            stream,
            decoder,
            frame: motion::CodedFrame::default(),
            payload: Vec::new(),
            planes: vec![0; size],
            reference: vec![0; size],
            rgba: Vec::new(),
        })
    }

    /// Reads and entropy-decodes the next frame.
    fn next(&mut self) -> AnyResult<()> {
        loop {
            let chunk = self.reader.next_chunk()?.ok_or("motion stream ended early")?;
            if chunk.stream != self.stream {
                continue;
            }
            self.reader.read_payload(&chunk, &mut self.payload)?;
            self.decoder.decode(chunk.keyframe(), &self.payload, &mut self.frame)?;
            return Ok(());
        }
    }

    /// The last decoded frame as RGBA.
    fn rgba(&mut self) -> &[u8] {
        let layout = self.decoder.layout();
        std::mem::swap(&mut self.planes, &mut self.reference);
        motion::reconstruct(layout, &self.frame, &self.reference, &mut self.planes);
        motion::to_rgba(layout, &self.planes, &mut self.rgba);
        &self.rgba
    }
}

fn open_variant(path: &Path, format: TextureFormat) -> AnyResult<Player<BufReader<File>>> {
    let player = Player::open(BufReader::new(File::open(path)?), &Capabilities::new([format]))?;
    match player.tracks().first() {
        Some(t) if t.params.format == format => Ok(player),
        _ => Err(format!("the encoded file has no {format} track").into()),
    }
}

/// Fails unless the player shows frame `i`, decoded at its time.
fn check_frame<R: std::io::Read + std::io::Seek>(player: &Player<R>, i: u64, at_us: u64) -> AnyResult<()> {
    match player.video_frame(0) {
        Some(f) if f.time == Duration::from_micros(at_us) => Ok(()),
        _ => Err(format!("frame {i} was not decoded at {at_us} us").into()),
    }
}

/// Mean squared error over the first `channels` channels of two RGBA images.
fn mse(a: &[u8], b: &[u8], channels: usize) -> f64 {
    let mut sum = 0u64;
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for c in 0..channels {
            let d = pa[c] as i64 - pb[c] as i64;
            sum += (d * d) as u64;
        }
    }
    sum as f64 / (a.len() / 4 * channels).max(1) as f64
}

fn psnr(mse: f64) -> f64 {
    if mse <= 0.0 { PSNR_CAP } else { (10.0 * (255.0f64 * 255.0 / mse).log10()).min(PSNR_CAP) }
}

fn decode_ms(times: &mut [Duration]) -> DecodeMs {
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    if times.is_empty() {
        return DecodeMs { mean: 0.0, p95: 0.0, max: 0.0 };
    }
    let rank = (times.len() as f64 * 0.95).ceil() as usize;
    DecodeMs {
        mean: ms(times.iter().sum::<Duration>()) / times.len() as f64,
        p95: ms(times[rank.clamp(1, times.len()) - 1]),
        max: ms(times[times.len() - 1]),
    }
}

fn print_results(r: &Results) {
    println!(
        "{} frames of {}x{} @ {}/{} fps, encoded in {:.1} s",
        r.frames, r.width, r.height, r.fps_num, r.fps_den, r.encode_seconds
    );
    println!(
        "{:<9} {:>8} {:>8} {:>9} {:>8} {:>8}   decode ms mean / p95 / max",
        "variant", "MB/s", "skipped", "keyframes", "PSNR dB", "min dB"
    );
    for v in &r.variants {
        println!(
            "{:<9} {:>8.2} {:>7.1}% {:>9} {:>8.2} {:>8.2}   {:.3} / {:.3} / {:.3}",
            v.format,
            v.mb_per_s,
            v.skipped_pct,
            v.keyframes,
            v.psnr_db,
            v.psnr_min_db,
            v.decode_ms.mean,
            v.decode_ms.p95,
            v.decode_ms.max
        );
    }
}

pub fn read_results(path: &Path) -> AnyResult<Results> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    match value.get("version").and_then(|v| v.as_u64()) {
        Some(v) if v == VERSION as u64 => Ok(serde_json::from_value(value)?),
        v => Err(format!("{}: unsupported results version {v:?}", path.display()).into()),
    }
}

/// Checks `new` against `baseline`; returns one message per regression. Fails when the
/// two runs are not comparable.
pub fn compare(baseline: &Results, new: &Results, max_size_increase: f64, max_psnr_drop: f64) -> AnyResult<Vec<String>> {
    let setup = |r: &Results| (r.input.clone(), r.width, r.height, r.fps_num, r.fps_den, r.frames, r.settings.clone());
    if setup(baseline) != setup(new) {
        return Err(format!(
            "baseline is not comparable: it has {:?}, this run {:?}",
            setup(baseline),
            setup(new)
        )
        .into());
    }
    println!("{:<9} {:>9} {:>9} {:>12}", "baseline", "bytes", "PSNR dB", "decode mean");
    let mut regressions = Vec::new();
    for b in &baseline.variants {
        let Some(n) = new.variants.iter().find(|n| n.format == b.format) else {
            regressions.push(format!("{} is missing", b.format));
            continue;
        };
        let size = 100.0 * (n.bytes as f64 / b.bytes.max(1) as f64 - 1.0);
        let psnr = n.psnr_db - b.psnr_db;
        let decode = n.decode_ms.mean / b.decode_ms.mean.max(1e-9);
        println!("{:<9} {:>+8.2}% {:>+9.3} {:>11.2}x", b.format, size, psnr, decode);
        if size > max_size_increase + EPSILON {
            regressions.push(format!(
                "{} grew {size:.2}% ({} -> {} bytes, limit {max_size_increase}%)",
                b.format, b.bytes, n.bytes
            ));
        }
        if -psnr > max_psnr_drop + EPSILON {
            regressions.push(format!(
                "{} PSNR dropped {:.3} dB ({:.3} -> {:.3}, limit {max_psnr_drop})",
                b.format, -psnr, b.psnr_db, n.psnr_db
            ));
        }
    }
    for n in new.variants.iter().filter(|n| !baseline.variants.iter().any(|b| b.format == n.format)) {
        println!("{:<9} new, not in the baseline", n.format);
    }
    Ok(regressions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn results() -> Results {
        Results {
            version: VERSION,
            input: "in.mp4".into(),
            width: 64,
            height: 32,
            fps_num: 30,
            fps_den: 1,
            frames: 10,
            settings: Settings { quality: "fast".into(), rdo: 0.0, keyint: 60, skip_threshold: 3, plane_min_saving: 8.0 },
            encode_seconds: 1.0,
            variants: vec![VariantResult {
                format: "bc7".into(),
                bytes: 1000,
                mb_per_s: 0.003,
                skipped_pct: 10.0,
                keyframes: 1,
                psnr_db: 40.0,
                psnr_min_db: 38.0,
                decode_ms: DecodeMs { mean: 0.5, p95: 0.6, max: 0.9 },
            }],
        }
    }

    #[test]
    fn json_roundtrip_ignores_unknown_fields() {
        let r = results();
        let mut value = serde_json::to_value(&r).unwrap();
        value["extra"] = serde_json::json!(1);
        let path = std::env::temp_dir().join(format!("praxi-bench-test-{}.json", std::process::id()));
        std::fs::write(&path, value.to_string()).unwrap();
        assert_eq!(read_results(&path).unwrap(), r);
        value["version"] = serde_json::json!(2);
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(read_results(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn compare_flags_size_and_psnr_regressions() {
        let base = results();
        let mut new = results();
        new.variants[0].bytes = 1010; // +1.0% is allowed
        new.variants[0].psnr_db = 39.9; // -0.1 dB is allowed
        new.variants[0].decode_ms.mean = 5.0; // timing never fails
        assert!(compare(&base, &new, 1.0, 0.1).unwrap().is_empty());

        new.variants[0].bytes = 1011;
        new.variants[0].psnr_db = 39.8;
        let r = compare(&base, &new, 1.0, 0.1).unwrap();
        assert_eq!(r.len(), 2, "{r:?}");

        new.variants[0].format = "astc6x6".into();
        assert_eq!(compare(&base, &new, 1.0, 0.1).unwrap(), ["bc7 is missing"]);

        new.frames = 11;
        assert!(compare(&base, &new, 1.0, 0.1).is_err(), "different frame count");
    }

    #[test]
    fn metrics() {
        assert_eq!(psnr(0.0), PSNR_CAP);
        assert!((psnr(1.0) - 48.13).abs() < 0.01);
        let a = [10, 20, 30, 255, 0, 0, 0, 0];
        let b = [12, 20, 30, 0, 0, 0, 0, 0];
        assert_eq!(mse(&a, &b, 3), 4.0 / 6.0);
        assert_eq!(mse(&a, &b, 4), (4.0 + 255.0 * 255.0) / 8.0);
        let mut times: Vec<Duration> = (1..=20).map(Duration::from_millis).collect();
        let d = decode_ms(&mut times);
        assert_eq!((d.mean, d.p95, d.max), (10.5, 19.0, 20.0));
    }
}
