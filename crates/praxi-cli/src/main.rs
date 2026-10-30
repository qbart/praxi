//! `praxi` command line tool (`docs/features/encoder.md`).

use clap::{Parser, Subcommand, ValueEnum};
use praxi_encoder::{CueConfig, Encoder, EncoderConfig, Quality, VariantConfig};
use praxi_format::{Cue, Reader, StreamParams, Value};
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
    /// Cue as TIME[+DURATION]=TYPE[:KEY=VALUE[;KEY=VALUE...]], times in seconds (repeatable).
    #[arg(long = "cue", value_parser = parse_cue)]
    cues: Vec<CueConfig>,
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

/// Parses `TIME[+DURATION]=TYPE[:KEY=VALUE[;KEY=VALUE...]]` (`docs/features/cues.md`).
fn parse_cue(s: &str) -> Result<CueConfig, String> {
    let (times, rest) = s.split_once('=').ok_or("expected TIME[+DURATION]=TYPE")?;
    let seconds = |t: &str| t.trim().parse::<f64>().map_err(|e| format!("bad time {t:?}: {e}"));
    let (time, duration) = match times.split_once('+') {
        Some((t, d)) => (seconds(t)?, seconds(d)?),
        None => (seconds(times)?, 0.0),
    };
    let (kind, fields) = rest.split_once(':').unwrap_or((rest, ""));
    if kind.is_empty() {
        return Err("empty cue type".into());
    }
    let mut cue = CueConfig::new(time, kind).duration(duration);
    for field in fields.split(';').filter(|f| !f.is_empty()) {
        let (key, value) = field.split_once('=').ok_or_else(|| format!("expected KEY=VALUE, got {field:?}"))?;
        cue = cue.field(key, value);
    }
    Ok(cue)
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
    for cue in args.cues {
        config = config.cue_with(cue);
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
    let mut ffmpeg = Command::new("ffmpeg");
    ffmpeg.args(["-v", "error", "-i"]).arg(&args.input);
    if let Some(n) = args.frames {
        ffmpeg.args(["-frames:v", &n.to_string()]);
    }
    let mut ffmpeg = ffmpeg
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    let mut stdout = ffmpeg.stdout.take().ok_or("no ffmpeg stdout")?;
    let mut frame = vec![0u8; p.width as usize * p.height as usize * 4];
    let started = Instant::now();
    let mut count = 0u64;
    while read_frame(&mut stdout, &mut frame)? {
        encoder.push_frame(&frame)?;
        count += 1;
        eprint!("\rframe {count}");
    }
    drop(stdout);
    let status = ffmpeg.wait()?;
    if !status.success() {
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
            StreamParams::Cues(c) => format!(
                "cues {:?}{}  max duration {}",
                c.name,
                if c.language.is_empty() { String::new() } else { format!(" ({})", c.language) },
                c.max_duration
            ),
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
            let is_cue = r.streams()[c.stream as usize].cue_params().is_some();
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
            if is_cue {
                r.read_payload(&c, &mut payload)?;
                print!("  {}", describe_cue(&Cue::decode(&payload)?));
            }
            println!();
        }
    }
    Ok(())
}

fn describe_cue(cue: &Cue) -> String {
    let mut out = format!("{} +{}", cue.kind, cue.duration);
    for (key, value) in &cue.fields {
        let value = match value {
            Value::String(s) => format!("{s:?}"),
            Value::Int(v) => v.to_string(),
            Value::Float(v) => v.to_string(),
            Value::Bool(v) => v.to_string(),
            Value::Bytes(b) => format!("<{} bytes>", b.len()),
        };
        out.push_str(&format!(" {key}={value}"));
    }
    out
}
