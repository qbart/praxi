//! Runs the `praxi` binary end to end on a video ffmpeg generates
//! (`docs/features/testing.md`). Skips without `ffmpeg` and `ffprobe`.

use praxi_format::{Reader, TextureFormat};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Skips a test that cannot run here, or fails it when `PRAXI_TEST_STRICT` is set.
fn skip(why: &str) {
    assert!(std::env::var_os("PRAXI_TEST_STRICT").is_none(), "PRAXI_TEST_STRICT: {why}");
    eprintln!("skipped: {why}");
}

fn has_ffmpeg() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|t| Command::new(t).arg("-version").output().is_ok_and(|o| o.status.success()))
}

/// A fresh directory for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("praxi-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs `praxi` with `args`, failing the test with its output if it fails; returns stdout.
fn praxi(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_praxi")).args(args).output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    assert!(out.status.success(), "praxi {args:?} failed:\n{}{}", text(&out.stdout), text(&out.stderr));
    text(&out.stdout)
}

/// One second of a 64x48 test pattern at 30 fps with a 48 kHz stereo tone.
fn source(dir: &Path) -> PathBuf {
    let path = dir.join("source.mkv");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=30:duration=1"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1", "-ac", "2"])
        .args(["-c:v", "ffv1", "-c:a", "pcm_s16le"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not generate the source");
    path
}

struct Summary {
    /// Format, width, height and frame count of every video stream.
    video: Vec<(TextureFormat, u32, u32, usize)>,
    /// Frame rate of the first video stream as (frames, seconds).
    time_base: (u32, u32),
    /// Seconds of audio per audio stream.
    audio: Vec<f64>,
    cue_streams: Vec<String>,
}

fn summary(path: &Path) -> Summary {
    let mut reader = Reader::new(BufReader::new(File::open(path).unwrap())).unwrap();
    let streams = reader.streams().to_vec();
    let mut frames = vec![0usize; streams.len()];
    let mut samples = vec![0u64; streams.len()];
    let mut payload = Vec::new();
    while let Some(chunk) = reader.next_chunk().unwrap() {
        let s = &streams[chunk.stream as usize];
        frames[chunk.stream as usize] += 1;
        if let Some(a) = s.audio_params() {
            reader.read_payload(&chunk, &mut payload).unwrap();
            samples[chunk.stream as usize] += a.payload_frames(&payload).unwrap();
        }
    }
    let first = streams.iter().find(|s| s.video_params().is_some()).unwrap();
    Summary {
        video: streams
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.video_params().map(|v| (v.format, v.width, v.height, frames[i])))
            .collect(),
        time_base: (first.time_base.den, first.time_base.num),
        audio: streams
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.audio_params().map(|a| samples[i] as f64 / a.sample_rate as f64))
            .collect(),
        cue_streams: streams.iter().filter_map(|s| s.cue_params().map(|c| c.name.clone())).collect(),
    }
}

#[test]
fn encodes_inspects_and_edits_cues() {
    if !has_ffmpeg() {
        return skip("ffmpeg or ffprobe not found");
    }
    let dir = temp_dir("encode");
    let input = source(&dir);
    let file = dir.join("out.praxi");
    let (input_s, file_s) = (input.to_str().unwrap(), file.to_str().unwrap());
    praxi(&[
        "encode", input_s, "-o", file_s, "--variant", "bc1", "--variant", "bc7", "--variant", "motion", "--scale", "32x-1",
        "--fps", "15", "--keyint", "5", "--cue", "0.1=a", "--cue", "0.2+0.3=b:k=v",
    ]);
    let s = summary(&file);
    assert_eq!(
        s.video,
        [(TextureFormat::Bc1, 32, 24, 15), (TextureFormat::Bc7, 32, 24, 15), (TextureFormat::Motion, 32, 24, 15)]
    );
    assert_eq!(s.time_base, (15, 1));
    assert_eq!(s.audio.len(), 1);
    assert!((s.audio[0] - 1.0).abs() < 0.05, "audio lasts {} s", s.audio[0]);
    assert_eq!(s.cue_streams, ["cues"]);

    // With --frames, the audio follows the output frame rate.
    let short = dir.join("short.praxi");
    praxi(&["encode", input_s, "-o", short.to_str().unwrap(), "--variant", "bc1", "--fps", "15", "--frames", "6"]);
    let s = summary(&short);
    assert_eq!(s.video, [(TextureFormat::Bc1, 64, 48, 6)]);
    assert!((s.audio[0] - 0.4).abs() < 0.05, "audio lasts {} s", s.audio[0]);

    // Opus: stereo and a mono variant at a lower bitrate, exact length.
    let opus = dir.join("opus.praxi");
    praxi(&[
        "encode", input_s, "-o", opus.to_str().unwrap(), "--variant", "bc1", "--audio-format", "opus", "--audio-layout",
        "stereo", "--audio-layout", "mono", "--audio-bitrate", "32",
    ]);
    let s = summary(&opus);
    assert_eq!(s.audio, [1.0, 1.0], "opus keeps the exact length");
    let info = praxi(&["info", opus.to_str().unwrap()]);
    assert!(info.contains("opus"), "{info}");

    let info = praxi(&["info", file_s]);
    assert!(info.contains("BC1 32x24"), "{info}");
    assert!(info.contains("motion 32x24"), "{info}");

    for format in ["bc1", "bc7"] {
        let png = dir.join(format!("{format}.png"));
        praxi(&["frame", file_s, "--format", format, "--at", "0.5", "-o", png.to_str().unwrap()]);
        assert_eq!(&std::fs::read(&png).unwrap()[..4], b"\x89PNG");
    }

    // Cues: export, add, remove, SRT, import.
    let sheet = praxi(&["cues", "export", file_s]);
    assert!(sheet.contains("\"type\": \"b\""), "{sheet}");
    praxi(&["cues", "add", file_s, "--cue", "0.7=c:label=Here;x=0.25;y=1;n=\"3\"", "--stream", "gameplay"]);
    let added = praxi(&["cues", "export", file_s]);
    assert!(added.contains("\"x\": 0.25") && added.contains("\"y\": 1,") && added.contains("\"n\": \"3\""), "typed values: {added}");
    praxi(&["cues", "remove", file_s, "--type", "a"]);
    let cues = |path: &Path| {
        let mut reader = Reader::new(BufReader::new(File::open(path).unwrap())).unwrap();
        praxi_format::read_cues(&mut reader)
            .unwrap()
            .into_iter()
            .map(|t| (t.name, t.cues.into_iter().map(|c| (c.time_us, c.cue.kind)).collect::<Vec<_>>()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        cues(&file),
        [("cues".to_string(), vec![(200_000, "b".to_string())]), ("gameplay".to_string(), vec![(700_000, "c".to_string())])]
    );
    let srt = dir.join("subs.srt");
    std::fs::write(&srt, "1\n00:00:00,100 --> 00:00:00,600\nHello.\n").unwrap();
    let subs = dir.join("subs.json");
    praxi(&["cues", "srt", srt.to_str().unwrap(), "-o", subs.to_str().unwrap()]);
    let edited = dir.join("edited.praxi");
    praxi(&["cues", "import", file_s, subs.to_str().unwrap(), "-o", edited.to_str().unwrap()]);
    assert_eq!(cues(&edited), [("subtitles".to_string(), vec![(100_000, "subtitle".to_string())])]);
    assert_eq!(summary(&edited).video, summary(&file).video, "video is kept");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn benches_to_json() {
    if !has_ffmpeg() {
        return skip("ffmpeg or ffprobe not found");
    }
    let dir = temp_dir("bench");
    let input = source(&dir);
    let json = dir.join("bench.json");
    praxi(&[
        "bench", input.to_str().unwrap(), "--frames", "4", "--variant", "bc1", "--variant", "motion", "--passes", "1", "--json",
        json.to_str().unwrap(),
    ]);
    let result: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    let variants = result["variants"].as_array().unwrap();
    assert_eq!(variants.len(), 2, "{result}");
    // The small test pattern is sharp: about 25 dB; broken decoding is far below.
    assert!(variants.iter().all(|v| v["psnr_db"].as_f64().unwrap() > 20.0), "{result}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `praxi encode` on a 10-bit clip tagged PQ / BT.2020, `praxi info`, and `praxi frame` to
/// EXR and PNG (`docs/features/hdr.md#tests`).
#[test]
fn encodes_hdr_sources() {
    if !has_ffmpeg() {
        return skip("ffmpeg and ffprobe not found");
    }
    let dir = temp_dir("hdr");
    let source = dir.join("pq.mkv");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=10:duration=0.5"])
        .args(["-vf", "format=yuv444p10le", "-c:v", "ffv1"])
        .args(["-color_trc", "smpte2084", "-color_primaries", "bt2020", "-colorspace", "bt2020nc"])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not generate the source");
    let out = dir.join("pq.praxi");
    let (src, out_s) = (source.to_str().unwrap(), out.to_str().unwrap());
    let stdout = praxi(&["encode", src, "-o", out_s, "--variant", "bc6h", "--variant", "astc6x6-hdr", "--variant", "bc7"]);
    assert!(stdout.contains("maxCLL"), "{stdout}");
    let s = summary(&out);
    assert_eq!(
        s.video,
        [(TextureFormat::Bc6h, 64, 48, 5), (TextureFormat::Astc6x6Hdr, 64, 48, 5), (TextureFormat::Bc7, 64, 48, 5)]
    );
    let reader = Reader::new(BufReader::new(File::open(&out).unwrap())).unwrap();
    let v = reader.streams()[0].video_params().unwrap();
    assert!(v.hdr() && v.color.reference_white == 203 && v.color.max_cll > 203, "{:?}", v.color);
    let info = praxi(&["info", out_s]);
    assert!(info.contains("video BC6H 64x48 HDR bt709 white 203 nits maxCLL"), "{info}");

    let exr = dir.join("frame.exr");
    praxi(&["frame", out_s, "--at", "0.2", "-o", exr.to_str().unwrap()]);
    assert!(std::fs::read(&exr).unwrap().starts_with(&[0x76, 0x2f, 0x31, 0x01]), "OpenEXR magic");
    let png = dir.join("frame.png");
    praxi(&["frame", out_s, "--format", "astc6x6-hdr", "--tonemap-white", "2", "-o", png.to_str().unwrap()]);
    assert!(std::fs::read(&png).unwrap().starts_with(b"\x89PNG"));

    // --scale would clamp linear float sources in ffmpeg.
    let failed = Command::new(env!("CARGO_BIN_EXE_praxi"))
        .args(["encode", src, "-o", out_s, "--variant", "bc6h", "--input-transfer", "linear", "--scale", "32x-1"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("--scale"));
    let _ = std::fs::remove_dir_all(&dir);
}
