//! `praxi` command line tool (`docs/features/encoder.md`).

use clap::{Parser, Subcommand, ValueEnum};
use praxi_encoder::{AudioTrackConfig, Chroma, CueConfig, Encoder, EncoderConfig, Quality, SampleFormat, VariantConfig};
use praxi::{Capabilities, Player};
use praxi_format::{Cue, Reader, StreamParams, TextureFormat, Value};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::Instant;

mod bench;
mod cues;

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
    /// Decode one frame on the CPU and write it as a PNG.
    Frame(FrameArgs),
    /// Measure size, quality and decode time of an encode (docs/features/benchmarks.md).
    Bench(bench::BenchArgs),
    /// Edit cues through cue sheets (docs/features/cue-editing.md).
    Cues {
        #[command(subcommand)]
        command: cues::CuesCmd,
    },
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
    #[command(flatten)]
    video: VideoOptions,
    /// Cue as TIME[+DURATION]=TYPE[:KEY=VALUE[;KEY=VALUE...]], times in seconds (repeatable).
    #[arg(long = "cue", value_parser = parse_cue)]
    cues: Vec<CueConfig>,
    /// Leave audio out.
    #[arg(long)]
    no_audio: bool,
    /// Audio variant in an ffmpeg channel layout, e.g. stereo or 5.1 (repeatable; default: source layout).
    #[arg(long = "audio-layout")]
    audio_layouts: Vec<String>,
    /// Resample audio to this rate in Hz (default: the source rate; for opus 48000 unless
    /// the source rate is an Opus rate).
    #[arg(long)]
    audio_rate: Option<u32>,
    #[arg(long, value_enum, default_value_t = SampleFormatArg::S16)]
    audio_format: SampleFormatArg,
    /// Opus bitrate of each audio variant in kbps (default: 64 per channel).
    #[arg(long)]
    audio_bitrate: Option<u32>,
    /// Audio track name.
    #[arg(long, default_value = "main")]
    audio_name: String,
}

/// Video encoding options shared by `encode` and `bench`.
#[derive(clap::Args)]
struct VideoOptions {
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
    /// Use byte planes only when they save at least this many percent (0-100).
    #[arg(long, default_value_t = praxi_format::DEFAULT_PLANE_MIN_SAVING)]
    plane_min_saving: f64,
    /// Resize to WxH pixels; one side may be -1 to keep the aspect ratio.
    #[arg(long, value_parser = parse_scale, allow_hyphen_values = true)]
    scale: Option<(i64, i64)>,
    /// Resample to N or N/D frames per second.
    #[arg(long, value_parser = parse_fps)]
    fps: Option<(u32, u32)>,
    /// Encode only the first N frames (after --fps).
    #[arg(long)]
    frames: Option<u64>,
    /// Quantizer of the motion variant (0-51; lower is larger and better).
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u8).range(0..=51))]
    qp: u8,
    /// Chroma resolution of the motion variant.
    #[arg(long, value_enum, default_value_t = ChromaArg::C420)]
    chroma: ChromaArg,
    /// Slices per frame of the motion variant.
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=255))]
    slices: u32,
}

#[derive(Clone, Copy, ValueEnum)]
enum ChromaArg {
    #[value(name = "420")]
    C420,
    #[value(name = "444")]
    C444,
}

#[derive(Clone, Copy, ValueEnum)]
enum SampleFormatArg {
    S16,
    F32,
    Adpcm,
    Opus,
}

#[derive(clap::Args)]
struct FrameArgs {
    file: PathBuf,
    #[arg(short, long)]
    output: PathBuf,
    /// Time of the frame in seconds.
    #[arg(long, default_value_t = 0.0)]
    at: f64,
    /// Variant to decode (default: each group's first variant).
    #[arg(long, value_enum)]
    format: Option<FormatArg>,
    /// Track (group) index.
    #[arg(long, default_value_t = 0)]
    track: usize,
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Bc1,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
    Astc4x4,
    Astc5x5,
    Astc6x6,
    Astc8x8,
    Motion,
}

impl FormatArg {
    fn format(self) -> TextureFormat {
        match self {
            FormatArg::Bc1 => TextureFormat::Bc1,
            FormatArg::Bc3 => TextureFormat::Bc3,
            FormatArg::Bc4 => TextureFormat::Bc4,
            FormatArg::Bc5 => TextureFormat::Bc5,
            FormatArg::Bc7 => TextureFormat::Bc7,
            FormatArg::Astc4x4 => TextureFormat::Astc4x4,
            FormatArg::Astc5x5 => TextureFormat::Astc5x5,
            FormatArg::Astc6x6 => TextureFormat::Astc6x6,
            FormatArg::Astc8x8 => TextureFormat::Astc8x8,
            FormatArg::Motion => TextureFormat::Motion,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum VariantArg {
    Bc7,
    Bc1,
    Astc4x4,
    Astc6x6,
    Astc8x8,
    Motion,
}

impl VariantArg {
    fn format(self) -> TextureFormat {
        match self {
            VariantArg::Bc7 => TextureFormat::Bc7,
            VariantArg::Bc1 => TextureFormat::Bc1,
            VariantArg::Astc4x4 => TextureFormat::Astc4x4,
            VariantArg::Astc6x6 => TextureFormat::Astc6x6,
            VariantArg::Astc8x8 => TextureFormat::Astc8x8,
            VariantArg::Motion => TextureFormat::Motion,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum QualityArg {
    Fast,
    Medium,
    Thorough,
}

/// The name of a clap value, as typed on the command line.
fn value_name(v: impl ValueEnum) -> String {
    v.to_possible_value().map_or_else(String::new, |p| p.get_name().to_string())
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
        cue = cue.field(key, parse_value(value)?);
    }
    Ok(cue)
}

/// A field value typed on the command line: bool, int, float, or string (quoted to force
/// one) (`docs/features/cues.md#values-on-the-command-line`).
fn parse_value(s: &str) -> Result<Value, String> {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        return Ok(Value::String(s[1..s.len() - 1].to_string()));
    }
    match s {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().map(Value::Int).map_err(|_| format!("integer {s} out of range"));
    }
    let numeric = s.bytes().any(|b| b.is_ascii_digit()) && s.bytes().all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b));
    Ok(match s.parse::<f64>() {
        Ok(v) if numeric => Value::Float(v),
        _ => Value::String(s.to_string()),
    })
}

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Encode(args) => encode(args),
        Cmd::Frame(args) => frame(args),
        Cmd::Bench(args) => bench::bench(args),
        Cmd::Cues { command } => cues::run(command),
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
    /// ffmpeg filters producing these frames from the input (`--fps`, `--scale`).
    filters: Option<String>,
}

/// `WxH`, each side a positive size or `-1` (from the other side and the aspect ratio).
fn parse_scale(s: &str) -> Result<(i64, i64), String> {
    let (w, h) = s.split_once('x').ok_or("expected WxH")?;
    let side = |v: &str| match v.parse::<i64>() {
        Ok(n) if n > 0 || n == -1 => Ok(n),
        _ => Err(format!("invalid size {s:?}")),
    };
    match (side(w)?, side(h)?) {
        (-1, -1) => Err("only one side of --scale can be -1".into()),
        size => Ok(size),
    }
}

/// `N` or `N/D` frames per second.
fn parse_fps(s: &str) -> Result<(u32, u32), String> {
    let (n, d) = s.split_once('/').unwrap_or((s, "1"));
    match (n.parse(), d.parse()) {
        (Ok(n), Ok(d)) if n > 0 && d > 0 => Ok((n, d)),
        _ => Err(format!("invalid frame rate {s:?}")),
    }
}

/// The size `--scale` gives a `from` sized source; a `-1` side keeps the aspect ratio.
fn scaled_size(from: (u32, u32), scale: (i64, i64)) -> (u32, u32) {
    let follow = |other: i64, a: u32, b: u32| ((other as f64 * b as f64 / a as f64).round() as u32).max(1);
    match scale {
        (-1, h) => (follow(h, from.1, from.0), h as u32),
        (w, -1) => (w as u32, follow(w, from.0, from.1)),
        (w, h) => (w as u32, h as u32),
    }
}

/// The frames the video options turn the input into: its probe with `--scale` and `--fps`
/// applied (`docs/features/encoder.md#cli`).
fn source(input: &Path, o: &VideoOptions) -> AnyResult<Probe> {
    let mut p = probe(input)?;
    let mut filters = Vec::new();
    if let Some((num, den)) = o.fps {
        (p.fps_num, p.fps_den) = (num, den);
        filters.push(format!("fps={num}/{den}"));
    }
    if let Some(scale) = o.scale {
        (p.width, p.height) = scaled_size((p.width, p.height), scale);
        filters.push(format!("scale={}:{}:flags=lanczos", p.width, p.height));
    }
    p.filters = (!filters.is_empty()).then(|| filters.join(","));
    Ok(p)
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
        filters: None,
    })
}

struct AudioProbe {
    sample_rate: u32,
    channels: u16,
    layout: String,
}

/// Probes the first audio stream; `None` when the input has none.
fn probe_audio(input: &Path) -> AnyResult<Option<AudioProbe>> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=sample_rate,channels,channel_layout", "-of", "default=noprint_wrappers=1"])
        .arg(input)
        .output()
        .map_err(|e| format!("cannot run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim()).into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('='))).map(str::trim);
    let (Some(rate), Some(channels)) = (field("sample_rate"), field("channels")) else {
        return Ok(None);
    };
    Ok(Some(AudioProbe {
        sample_rate: rate.parse()?,
        channels: channels.parse()?,
        layout: field("channel_layout").unwrap_or("").to_string(),
    }))
}

/// WAVE speaker bits, in bit order, by ffmpeg channel name.
const SPEAKERS: [&str; 18] =
    ["FL", "FR", "FC", "LFE", "BL", "BR", "FLC", "FRC", "BC", "SL", "SR", "TC", "TFL", "TFC", "TFR", "TBL", "TBC", "TBR"];

/// ffmpeg's standard layouts made of WAVE speakers (`ffmpeg -layouts`).
const LAYOUTS: [(&str, &str); 25] = [
    ("mono", "FC"),
    ("stereo", "FL+FR"),
    ("2.1", "FL+FR+LFE"),
    ("3.0", "FL+FR+FC"),
    ("3.0(back)", "FL+FR+BC"),
    ("4.0", "FL+FR+FC+BC"),
    ("quad", "FL+FR+BL+BR"),
    ("quad(side)", "FL+FR+SL+SR"),
    ("3.1", "FL+FR+FC+LFE"),
    ("5.0", "FL+FR+FC+BL+BR"),
    ("5.0(side)", "FL+FR+FC+SL+SR"),
    ("4.1", "FL+FR+FC+LFE+BC"),
    ("5.1", "FL+FR+FC+LFE+BL+BR"),
    ("5.1(side)", "FL+FR+FC+LFE+SL+SR"),
    ("6.0", "FL+FR+FC+BC+SL+SR"),
    ("6.0(front)", "FL+FR+FLC+FRC+SL+SR"),
    ("3.1.2", "FL+FR+FC+LFE+TFL+TFR"),
    ("hexagonal", "FL+FR+FC+BL+BR+BC"),
    ("6.1", "FL+FR+FC+LFE+BC+SL+SR"),
    ("6.1(back)", "FL+FR+FC+LFE+BL+BR+BC"),
    ("6.1(front)", "FL+FR+LFE+FLC+FRC+SL+SR"),
    ("7.0", "FL+FR+FC+BL+BR+SL+SR"),
    ("7.0(front)", "FL+FR+FC+FLC+FRC+SL+SR"),
    ("7.1", "FL+FR+FC+LFE+BL+BR+SL+SR"),
    ("7.1(wide)", "FL+FR+FC+LFE+BL+BR+FLC+FRC"),
];

/// Channel mask and channel count of a standard ffmpeg layout.
fn layout_mask(name: &str) -> Option<(u32, u16)> {
    let (_, speakers) = LAYOUTS.iter().find(|(n, _)| *n == name)?;
    let mut mask = 0u32;
    for s in speakers.split('+') {
        mask |= 1 << SPEAKERS.iter().position(|n| *n == s)?;
    }
    Some((mask, mask.count_ones() as u16))
}

/// Starts an ffmpeg process decoding the first audio stream to `f32le` on its stdout.
fn spawn_audio(args: &EncodeArgs, layout: Option<&str>, seconds: Option<f64>, resample: Option<u32>) -> AnyResult<Child> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-i"]).arg(&args.input).args(["-map", "0:a:0", "-vn"]);
    if let Some(s) = seconds {
        cmd.args(["-t", &format!("{s:.6}")]);
    }
    if let Some(l) = layout {
        cmd.args(["-af", &format!("aformat=channel_layouts={l}")]);
    }
    if let Some(r) = resample {
        cmd.args(["-ar", &r.to_string()]);
    }
    let child = cmd
        .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    Ok(child)
}

fn pix_fmt_has_alpha(pix_fmt: &str) -> bool {
    ["yuva", "gbrap", "rgba", "bgra", "argb", "abgr", "ya8", "ya16", "pal8"].iter().any(|p| pix_fmt.starts_with(p))
}

/// The encoder configuration for the video options, without audio and cues.
fn video_config(p: &Probe, o: &VideoOptions) -> EncoderConfig {
    let quality = match o.quality {
        QualityArg::Fast => Quality::Fast,
        QualityArg::Medium => Quality::Medium,
        QualityArg::Thorough => Quality::Thorough,
    };
    let mut config = EncoderConfig::new(p.width, p.height, p.fps_num, p.fps_den)
        .keyint(o.keyint)
        .skip_threshold(o.skip_threshold)
        .plane_min_saving(o.plane_min_saving)
        .alpha(p.alpha);
    for v in &o.variants {
        let variant = match v {
            VariantArg::Bc7 => VariantConfig::bc7().rdo(o.rdo),
            VariantArg::Bc1 => VariantConfig::bc1(),
            VariantArg::Astc4x4 => VariantConfig::astc(4),
            VariantArg::Astc6x6 => VariantConfig::astc(6),
            VariantArg::Astc8x8 => VariantConfig::astc(8),
            VariantArg::Motion => VariantConfig::motion().qp(o.qp).slices(o.slices).chroma(match o.chroma {
                ChromaArg::C420 => Chroma::C420,
                ChromaArg::C444 => Chroma::C444,
            }),
        };
        config = config.variant(variant.quality(quality));
    }
    config
}

/// Starts an ffmpeg process decoding the video to the RGBA frames of `source` on its stdout.
fn spawn_video(input: &Path, source: &Probe, frames: Option<u64>) -> AnyResult<Child> {
    let mut ffmpeg = Command::new("ffmpeg");
    ffmpeg.args(["-v", "error", "-nostdin", "-i"]).arg(input);
    if let Some(filters) = &source.filters {
        ffmpeg.args(["-vf", filters]);
    }
    if let Some(n) = frames {
        ffmpeg.args(["-frames:v", &n.to_string()]);
    }
    let child = ffmpeg
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    Ok(child)
}

/// Feeds every frame of `ffmpeg`'s stdout to `push`, then waits for ffmpeg. Returns the frame count.
fn pump_frames(mut ffmpeg: Child, frame_bytes: usize, mut push: impl FnMut(&[u8]) -> AnyResult<()>) -> AnyResult<u64> {
    let mut stdout = ffmpeg.stdout.take().ok_or("no ffmpeg stdout")?;
    let mut frame = vec![0u8; frame_bytes];
    let mut count = 0u64;
    while read_frame(&mut stdout, &mut frame)? {
        push(&frame)?;
        count += 1;
    }
    drop(stdout);
    let status = ffmpeg.wait()?;
    if !status.success() {
        return Err(format!("ffmpeg exited with {status}").into());
    }
    Ok(count)
}

fn encode(args: EncodeArgs) -> AnyResult<()> {
    let p = source(&args.input, &args.video)?;
    let mut config = video_config(&p, &args.video);
    for cue in &args.cues {
        config = config.cue_with(cue.clone());
    }
    let mut children = Vec::new();
    let audio = if args.no_audio { None } else { probe_audio(&args.input)? };
    if let Some(a) = &audio {
        let seconds = args.video.frames.map(|n| n as f64 * p.fps_den as f64 / p.fps_num as f64);
        let format = match args.audio_format {
            SampleFormatArg::S16 => SampleFormat::S16,
            SampleFormatArg::F32 => SampleFormat::F32,
            SampleFormatArg::Adpcm => SampleFormat::Adpcm,
            SampleFormatArg::Opus => SampleFormat::Opus,
        };
        let opus_rate = |r: u32| [8000, 12000, 16000, 24000, 48000].contains(&r);
        let rate = match args.audio_rate {
            Some(r) => r,
            None if format == SampleFormat::Opus && !opus_rate(a.sample_rate) => 48000,
            None => a.sample_rate,
        };
        if format == SampleFormat::Opus && !opus_rate(rate) {
            return Err(format!("--audio-rate {rate}: opus takes 8000, 12000, 16000, 24000 or 48000").into());
        }
        let resample = (rate != a.sample_rate).then_some(rate);
        let variants: Vec<(Option<&str>, u32, u16)> = if args.audio_layouts.is_empty() {
            let mask = layout_mask(&a.layout).filter(|(_, n)| *n == a.channels).map_or(0, |(m, _)| m);
            vec![(None, mask, a.channels)]
        } else {
            let mut v = Vec::new();
            for l in &args.audio_layouts {
                let (mask, channels) = layout_mask(l).ok_or_else(|| format!("unknown audio layout {l:?}, see ffmpeg -layouts"))?;
                v.push((Some(l.as_str()), mask, channels));
            }
            v
        };
        for (layout, mask, channels) in variants {
            let mut child = spawn_audio(&args, layout, seconds, resample)?;
            let stdout = child.stdout.take().ok_or("no ffmpeg stdout")?;
            children.push(child);
            let mut track = AudioTrackConfig::from_reader(args.audio_name.clone(), rate, channels, stdout)
                .channel_mask(mask)
                .format(format);
            if let Some(kbps) = args.audio_bitrate {
                track = track.bitrate(kbps.saturating_mul(1000));
            }
            config = config.audio(track);
            eprintln!("audio: {rate} Hz, {channels} channels {}", layout.unwrap_or(&a.layout));
        }
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
    let ffmpeg = spawn_video(&args.input, &p, args.video.frames)?;
    let started = Instant::now();
    let mut count = 0u64;
    pump_frames(ffmpeg, p.width as usize * p.height as usize * 4, |frame| {
        encoder.push_frame(frame)?;
        count += 1;
        eprint!("\rframe {count}");
        Ok(())
    })?;
    let (_, stats) = encoder.finish()?;
    for mut child in children {
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("ffmpeg (audio) exited with {status}").into());
        }
    }
    eprintln!("\rencoded {} frames in {:.1} s", stats.frames, started.elapsed().as_secs_f64());

    let secs = (stats.duration_us as f64 / 1e6).max(1e-9);
    println!("{:<10} {:>12} {:>9} {:>9} {:>9}", "variant", "bytes", "MB/s", "skipped", "keyframes");
    for v in &stats.variants {
        let name = v.format.map_or("?", |f| f.name());
        let skipped = 100.0 * v.skipped_blocks as f64 / v.blocks.max(1) as f64;
        println!("{:<10} {:>12} {:>9.2} {:>8.1}% {:>9}", name, v.bytes, v.bytes as f64 / 1e6 / secs, skipped, v.keyframes);
        if let Some(m) = v.motion {
            let total = (m.intra + m.inter + m.split + m.skip).max(1) as f64;
            println!(
                "{:<10} macroblocks {:.1}% intra, {:.1}% inter, {:.1}% split, {:.1}% skip; {} scene cuts",
                "",
                100.0 * m.intra as f64 / total,
                100.0 * m.inter as f64 / total,
                100.0 * m.split as f64 / total,
                100.0 * m.skip as f64 / total,
                m.scene_cuts
            );
        }
    }
    for a in &stats.audio {
        let name = format!("audio {}ch", a.channels);
        println!("{:<10} {:>12} {:>9.2}  {:?}, {} frames", name, a.bytes, a.bytes as f64 / 1e6 / secs, a.name, a.frames);
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

fn frame(args: FrameArgs) -> AnyResult<()> {
    if !args.at.is_finite() || args.at < 0.0 {
        return Err(format!("invalid time {}", args.at).into());
    }
    let caps = match args.format {
        Some(f) => Capabilities::new([f.format()]),
        None => {
            // Each group's first variant in stream order.
            let reader = Reader::new(BufReader::new(File::open(&args.file)?))?;
            let mut first: Vec<(u16, TextureFormat)> = Vec::new();
            for s in reader.streams() {
                if let Some(v) = s.video_params()
                    && !first.iter().any(|(g, _)| *g == s.group)
                {
                    first.push((s.group, v.format));
                }
            }
            Capabilities::new(first.into_iter().map(|(_, f)| f))
        }
    };
    let mut player = Player::open(BufReader::new(File::open(&args.file)?), &caps)?;
    let tracks = player.tracks().len();
    if args.track >= tracks {
        return Err(format!("track {} out of range, the file has {tracks}", args.track).into());
    }
    player.seek(std::time::Duration::from_secs_f64(args.at))?;
    let params = player.tracks()[args.track].params;
    let frame = player.video_frame(args.track).ok_or("no frame at that time")?;
    let time = frame.time;
    let rgba = if params.format == TextureFormat::Motion {
        // Seeking queued the frames from the preceding keyframe: rebuild them on the CPU.
        use praxi::format::motion;
        let layout = motion::Layout::new(&params);
        let (mut planes, mut reference) = (vec![0; layout.size], vec![0; layout.size]);
        for coded in player.take_coded_frames(args.track) {
            std::mem::swap(&mut planes, &mut reference);
            motion::reconstruct(&layout, coded, &reference, &mut planes);
        }
        let mut rgba = Vec::new();
        motion::to_rgba(&layout, &planes, &mut rgba);
        rgba
    } else {
        praxi_encoder::decode_frame(&params, frame.data)?
    };
    let png = ktx::lodepng::encode(&rgba, params.width, params.height, &mut ktx::lodepng::State::new())
        .map_err(|code| format!("PNG encoding failed: {}", ktx::lodepng::lodepng_error_text(code)))?;
    std::fs::write(&args.output, png)?;
    println!(
        "{}: frame at {:.3} s, {} {}x{}",
        args.output.display(),
        time.as_secs_f64(),
        params.format,
        params.width,
        params.height
    );
    Ok(())
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
            StreamParams::Audio(a) => format!(
                "audio {:?} {} Hz {} ch mask {:#x} {} preload {} ms",
                a.name,
                a.sample_rate,
                a.channels,
                a.channel_mask,
                a.format,
                a.preload_us / 1000
            ),
            StreamParams::Cues(c) => format!("cues {:?}  max duration {}", c.name, c.max_duration),
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
            let audio = r.streams()[c.stream as usize].audio_params().cloned();
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
            if let Some(a) = audio {
                r.read_payload(&c, &mut payload)?;
                let frames = a.payload_frames(&payload).ok_or("invalid audio payload")?;
                print!("  {frames} frames");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_cue_values() {
        let v = |s| parse_value(s).unwrap();
        assert_eq!(v("true"), Value::Bool(true));
        assert_eq!(v("false"), Value::Bool(false));
        assert_eq!(v("2"), Value::Int(2));
        assert_eq!(v("-1"), Value::Int(-1));
        assert_eq!(v("+7"), Value::Int(7));
        assert_eq!(v("0.25"), Value::Float(0.25));
        assert_eq!(v(".5"), Value::Float(0.5));
        assert_eq!(v("-3.5"), Value::Float(-3.5));
        assert_eq!(v("1e-3"), Value::Float(0.001));
        assert_eq!(v("99999999999999999999.0"), Value::Float(1e20));
        for s in ["red", "", "1.2.3", "2024-01-01", "inf", "nan", "-", "e", "1x", "True", "\"", "0x10"] {
            assert_eq!(v(s), Value::String(s.into()), "{s:?}");
        }
        assert_eq!(v("\"42\""), Value::String("42".into()));
        assert_eq!(v("\"true\""), Value::String("true".into()));
        assert_eq!(v("\"a\"b\""), Value::String("a\"b".into()));
        assert_eq!(v("\"\""), Value::String(String::new()));
        assert!(parse_value("99999999999999999999").is_err());

        let cue = parse_cue("5.5=marker:label=Look here;x=0.5;y=0;power=2;code=\"7\"").unwrap();
        let fields: Vec<_> = cue.fields.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        assert_eq!(
            fields,
            [
                ("label", Value::String("Look here".into())),
                ("x", Value::Float(0.5)),
                ("y", Value::Int(0)),
                ("power", Value::Int(2)),
                ("code", Value::String("7".into())),
            ]
        );
        let cue = Cue { duration: 0, kind: cue.kind, fields: cue.fields };
        assert_eq!(cue.position(), Some((0.5, 0.0)));
    }

    #[test]
    fn parses_scale_and_fps() {
        assert_eq!(parse_scale("1280x720"), Ok((1280, 720)));
        assert_eq!(parse_scale("1280x-1"), Ok((1280, -1)));
        assert_eq!(parse_scale("-1x540"), Ok((-1, 540)));
        for bad in ["-1x-1", "0x720", "1280", "1280x-2", "axb"] {
            assert!(parse_scale(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_fps("30"), Ok((30, 1)));
        assert_eq!(parse_fps("30000/1001"), Ok((30000, 1001)));
        for bad in ["0", "30/0", "x", "-30"] {
            assert!(parse_fps(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn scaled_sizes_keep_the_aspect_ratio() {
        assert_eq!(scaled_size((1920, 1080), (1280, -1)), (1280, 720));
        assert_eq!(scaled_size((1920, 1080), (-1, 540)), (960, 540));
        assert_eq!(scaled_size((1920, 1080), (1000, -1)), (1000, 563));
        assert_eq!(scaled_size((1920, 1080), (1, -1)), (1, 1));
        assert_eq!(scaled_size((1920, 1080), (640, 640)), (640, 640));
    }
}
