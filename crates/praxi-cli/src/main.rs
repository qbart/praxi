//! `praxi` command line tool (`docs/features/encoder.md`).

use clap::{Parser, Subcommand, ValueEnum};
use praxi_encoder::{Encoder, EncoderConfig, Quality, VariantConfig};
use praxi_format::{Reader, StreamParams};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

#[derive(Parser)]
#[command(name = "praxi", version, about = "praxi video codec for games")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Encode a video (anything ffmpeg decodes) into a .praxi file.
    Encode(EncodeArgs),
    /// Print the header and stream table of a .praxi file.
    Info {
        file: PathBuf,
        /// Also list every chunk.
        #[arg(long)]
        chunks: bool,
    },
}

#[derive(clap::Args)]
struct EncodeArgs {
    input: PathBuf,
    #[arg(short, long)]
    output: PathBuf,
    /// Texture format variants to write (repeatable).
    #[arg(long = "variant", value_enum, default_values_t = [VariantArg::Bc7])]
    variants: Vec<VariantArg>,
    #[arg(long, value_enum, default_value_t = QualityArg::Fast)]
    quality: QualityArg,
    /// UASTC RDO strength for bc7 (0 = off).
    #[arg(long, default_value_t = 0.0)]
    rdo: f32,
    /// Frames between keyframes.
    #[arg(long, default_value_t = 60)]
    keyint: u32,
    /// Temporal skip threshold.
    #[arg(long, default_value_t = 3)]
    skip_threshold: u8,
    /// Marker as TIME=NAME, TIME in seconds (repeatable).
    #[arg(long = "marker", value_parser = parse_marker)]
    markers: Vec<(f64, String)>,
    /// Encode only the first N frames.
    #[arg(long)]
    frames: Option<u64>,
}

#[derive(Clone, Copy, ValueEnum)]
enum VariantArg {
    Bc7,
    Astc4x4,
    Astc6x6,
    Astc8x8,
}

#[derive(Clone, Copy, ValueEnum)]
enum QualityArg {
    Fast,
    Medium,
    Thorough,
}

fn parse_marker(s: &str) -> Result<(f64, String), String> {
    let (time, name) = s.split_once('=').ok_or("expected TIME=NAME")?;
    let time: f64 = time.trim().parse().map_err(|e| format!("bad time {time:?}: {e}"))?;
    if name.is_empty() {
        return Err("empty marker name".into());
    }
    Ok((time, name.to_string()))
}

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Encode(args) => encode(args),
        Cmd::Info { file, chunks } => info(&file, chunks),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Probe {
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    alpha: bool,
}

fn probe(input: &Path) -> AnyResult<Probe> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,r_frame_rate,pix_fmt", "-of", "default=noprint_wrappers=1"])
        .arg(input)
        .output()
        .map_err(|e| format!("cannot run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim()).into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .map(str::trim)
            .ok_or_else(|| format!("ffprobe did not report {key}"))
    };
    let (num, den) = field("r_frame_rate")?.split_once('/').ok_or("bad r_frame_rate")?;
    let pix_fmt = field("pix_fmt")?;
    Ok(Probe {
        width: field("width")?.parse()?,
        height: field("height")?.parse()?,
        fps_num: num.parse()?,
        fps_den: den.parse()?,
        alpha: pix_fmt_has_alpha(pix_fmt),
    })
}

fn pix_fmt_has_alpha(pix_fmt: &str) -> bool {
    ["yuva", "gbrap", "rgba", "bgra", "argb", "abgr", "ya8", "ya16", "pal8"].iter().any(|p| pix_fmt.starts_with(p))
}

fn encode(args: EncodeArgs) -> AnyResult<()> {
    let p = probe(&args.input)?;
    let quality = match args.quality {
        QualityArg::Fast => Quality::Fast,
        QualityArg::Medium => Quality::Medium,
        QualityArg::Thorough => Quality::Thorough,
    };
    let mut config = EncoderConfig::new(p.width, p.height, p.fps_num, p.fps_den)
        .keyint(args.keyint)
        .skip_threshold(args.skip_threshold)
        .alpha(p.alpha);
    for v in &args.variants {
        let variant = match v {
            VariantArg::Bc7 => VariantConfig::bc7().rdo(args.rdo),
            VariantArg::Astc4x4 => VariantConfig::astc(4),
            VariantArg::Astc6x6 => VariantConfig::astc(6),
            VariantArg::Astc8x8 => VariantConfig::astc(8),
        };
        config = config.variant(variant.quality(quality));
    }
    for (time, name) in &args.markers {
        config = config.marker(*time, name.clone());
    }
    eprintln!(
        "{}: {}x{} @ {}/{} fps{}",
        args.input.display(),
        p.width,
        p.height,
        p.fps_num,
        p.fps_den,
        if p.alpha { ", alpha" } else { "" }
    );

    let out = BufWriter::new(File::create(&args.output)?);
    let mut encoder = Encoder::new(out, config)?;
    let mut ffmpeg = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&args.input)
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    let mut stdout = ffmpeg.stdout.take().ok_or("no ffmpeg stdout")?;
    let mut frame = vec![0u8; p.width as usize * p.height as usize * 4];
    let started = Instant::now();
    let mut count = 0u64;
    while args.frames.is_none_or(|n| count < n) {
        if !read_frame(&mut stdout, &mut frame)? {
            break;
        }
        encoder.push_frame(&frame)?;
        count += 1;
        eprint!("\rframe {count}");
    }
    drop(stdout);
    let status = ffmpeg.wait()?;
    if args.frames.is_none() && !status.success() {
        return Err(format!("ffmpeg exited with {status}").into());
    }
    let (_, stats) = encoder.finish()?;
    eprintln!("\rencoded {} frames in {:.1} s", stats.frames, started.elapsed().as_secs_f64());

    let secs = (stats.duration_us as f64 / 1e6).max(1e-9);
    println!("{:<10} {:>12} {:>9} {:>9} {:>9}", "variant", "bytes", "MB/s", "skipped", "keyframes");
    for v in &stats.variants {
        let name = v.format.map_or("?", |f| f.name());
        let skipped = 100.0 * v.skipped_blocks as f64 / v.blocks.max(1) as f64;
        println!("{:<10} {:>12} {:>9.2} {:>8.1}% {:>9}", name, v.bytes, v.bytes as f64 / 1e6 / secs, skipped, v.keyframes);
    }
    Ok(())
}

/// Reads one whole frame; `false` on a clean end of stream.
fn read_frame(r: &mut impl Read, buf: &mut [u8]) -> AnyResult<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 if filled == 0 => return Ok(false),
            0 => return Err("ffmpeg output ended mid-frame".into()),
            n => filled += n,
        }
    }
    Ok(true)
}

fn info(path: &Path, chunks: bool) -> AnyResult<()> {
    let mut r = Reader::new(BufReader::new(File::open(path)?))?;
    let h = *r.header();
    println!("version   {}.{}", h.version_major, h.version_minor);
    println!("duration  {:.3} s", h.duration_us as f64 / 1e6);
    println!("index     {} entries at {}", h.index_count, h.index_offset);
    for (i, s) in r.streams().iter().enumerate() {
        let desc = match &s.params {
            StreamParams::Video(v) => format!(
                "video {} {}x{}{}{}",
                v.format,
                v.width,
                v.height,
                if v.srgb() { " sRGB" } else { "" },
                if v.alpha() { " alpha" } else { "" }
            ),
            StreamParams::Markers => "markers".to_string(),
            StreamParams::Unknown(kind) => format!("unknown kind {kind}"),
        };
        println!(
            "stream {i}  group {}  time base {}/{}  {} chunks  {desc}",
            s.group, s.time_base.num, s.time_base.den, s.chunk_count
        );
    }
    if chunks {
        let mut payload = Vec::new();
        while let Some(c) = r.next_chunk()? {
            let marker = r.streams()[c.stream as usize].params == StreamParams::Markers;
            print!(
                "{:>10}  stream {}  pts {:>8}  {}  {:?}  {} -> {}",
                c.offset,
                c.stream,
                c.pts,
                if c.keyframe() { "key  " } else { "delta" },
                c.compression,
                c.stored_size,
                c.raw_size
            );
            if marker {
                r.read_payload(&c, &mut payload)?;
                print!("  {:?}", String::from_utf8_lossy(&payload));
            }
            println!();
        }
    }
    Ok(())
}
