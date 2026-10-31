use praxi::format::{AudioParams, COLOR_FLAG_SRGB, SampleFormat, StreamInfo, TimeBase, VideoParams, Writer, adpcm};
use praxi::{AudioOutput, Capabilities, Clock, Player, TextureFormat};
use std::io::Cursor;
use std::time::Duration;

const MS: Duration = Duration::from_millis(1);

/// Audio at 1000 Hz, so one frame is one millisecond.
fn audio_stream(group: u16, name: &str, channels: u16, format: SampleFormat) -> StreamInfo {
    let channel_mask = if channels == 2 { 0x3 } else { 0x4 };
    StreamInfo::audio(
        group,
        AudioParams { sample_rate: 1000, channels, format, channel_mask, preload_us: 200_000, name: name.into() },
    )
}

/// Samples of frames `from..to`: frame `i` holds `i * 10` (s16) or `i / 1000` (f32) in
/// every channel, negated in the second.
fn samples(from: u64, to: u64, channels: u16, format: SampleFormat) -> Vec<u8> {
    let mut out = Vec::new();
    for i in from..to {
        for c in 0..channels {
            let sign = if c == 1 { -1 } else { 1 };
            match format {
                SampleFormat::S16 => out.extend_from_slice(&((i as i16 * 10) * sign).to_le_bytes()),
                SampleFormat::F32 => out.extend_from_slice(&((i as f32 / 1000.0) * sign as f32).to_le_bytes()),
                SampleFormat::Adpcm => unreachable!("see plays_adpcm"),
            }
        }
    }
    out
}

/// 8x4 BC1 video at 10 fps, 5 keyframes, 500 ms. Audio group 0 "music": stereo s16
/// (stream 1) and mono f32 (stream 2), 500 frames. Audio group 1 "voice": mono s16
/// (stream 3), 300 frames. Audio chunks have 100 frames and 200 ms preload.
fn sample_file() -> Vec<u8> {
    let video = VideoParams { width: 8, height: 4, format: TextureFormat::Bc1, color_flags: COLOR_FLAG_SRGB };
    let streams = vec![
        StreamInfo::video(0, TimeBase::new(1, 10), video),
        audio_stream(0, "music", 2, SampleFormat::S16),
        audio_stream(0, "music", 1, SampleFormat::F32),
        audio_stream(1, "voice", 1, SampleFormat::S16),
    ];
    // (decode time in ms, stream, pts), written in decode-time order.
    let mut chunks: Vec<(u64, u16, u64)> = (0..5).map(|n| (n * 100, 0, n)).collect();
    for (stream, frames) in [(1, 500u64), (2, 500), (3, 300)] {
        chunks.extend((0..frames).step_by(100).map(|pts| (pts.saturating_sub(200), stream, pts)));
    }
    chunks.sort_by_key(|&(t, stream, _)| (t, stream != 0));
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    for (_, stream, pts) in chunks {
        match stream {
            0 => w.write_chunk(0, pts, true, &[pts as u8; 16], None).unwrap(),
            1 => w.write_audio(1, pts, &samples(pts, pts + 100, 2, SampleFormat::S16)).unwrap(),
            2 => w.write_audio(2, pts, &samples(pts, pts + 100, 1, SampleFormat::F32)).unwrap(),
            _ => w.write_audio(3, pts, &samples(pts, pts + 100, 1, SampleFormat::S16)).unwrap(),
        }
    }
    w.finish(500_000).unwrap().into_inner()
}

fn open(caps: Capabilities) -> Player<Cursor<Vec<u8>>> {
    Player::open(Cursor::new(sample_file()), &caps).unwrap()
}

fn caps() -> Capabilities {
    Capabilities::new([TextureFormat::Bc1])
}

/// Reads up to `frames` frames of a stereo s16 output and returns their frame numbers,
/// checking the second channel mirrors the first.
fn read_frames(out: &AudioOutput, frames: usize) -> Vec<u64> {
    let mut buf = vec![0.0; frames * 2];
    let n = out.read(&mut buf);
    assert_eq!(n % 2, 0);
    buf[..n]
        .chunks(2)
        .map(|f| {
            assert_eq!(f[0], -f[1]);
            (f[0] * 32768.0 / 10.0).round() as u64
        })
        .collect()
}

fn range(from: u64, to: u64) -> Vec<u64> {
    (from..to).collect()
}

#[test]
fn selects_audio_variants_by_channels() {
    let p = open(caps());
    let streams: Vec<u16> = p.audio_tracks().iter().map(|t| t.stream).collect();
    assert_eq!(streams, [1, 3]);
    let music = &p.audio_tracks()[0];
    assert_eq!((music.group, music.params.name.as_str(), music.params.channels), (0, "music", 2));
    let out = p.audio_output(0).unwrap();
    assert_eq!((out.sample_rate(), out.channels(), out.channel_mask()), (1000, 2, 0x3));
    assert!(p.audio_output(2).is_none());

    assert_eq!(open(caps().max_audio_channels(1)).audio_tracks()[0].stream, 2);
    assert_eq!(open(caps().max_audio_channels(0)).audio_tracks()[0].stream, 2, "fewest channels when none fit");
    assert_eq!(open(caps().max_audio_channels(8)).audio_tracks()[0].stream, 1);
}

#[test]
fn queues_preload_ahead_of_the_clock() {
    let mut p = open(caps());
    let out = p.audio_output(0).unwrap();
    assert_eq!(out.read(&mut [0.0; 64]), 0, "nothing before play");

    p.play().unwrap();
    assert_eq!(read_frames(&out, 1000), range(0, 300), "play queues the chunks due at 0: 300 ms");
    assert_eq!(out.read(&mut [0.0; 64]), 0);
    p.advance(100 * MS).unwrap();
    assert_eq!(read_frames(&out, 1000), range(300, 400));

    let mut buf = [0.0; 3];
    assert_eq!(out.read(&mut buf), 0, "reads whole frames only");
}

#[test]
fn converts_f32_and_mono() {
    let mut p = open(caps().max_audio_channels(1));
    let out = p.audio_output(0).unwrap();
    p.play().unwrap();
    let mut buf = [0.0; 4];
    assert_eq!(out.read(&mut buf), 4);
    assert_eq!(buf, [0.0, 0.001, 0.002, 0.003]);
}

#[test]
fn plays_adpcm() {
    // 1 s of a mono tone at 1000 Hz in ADPCM chunks of 100 frames, with a 1 fps video.
    let tone: Vec<i16> = (0..1000).map(|i| (10000.0 * (i as f64 * 0.05).sin()) as i16).collect();
    let video = VideoParams { width: 8, height: 4, format: TextureFormat::Bc1, color_flags: COLOR_FLAG_SRGB };
    let streams = vec![StreamInfo::video(0, TimeBase::new(1, 1), video), audio_stream(0, "music", 1, SampleFormat::Adpcm)];
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    w.write_chunk(0, 0, true, &[0; 16], None).unwrap();
    let mut encoder = adpcm::Encoder::new(1);
    for (n, part) in tone.chunks(100).enumerate() {
        let mut payload = Vec::new();
        encoder.encode(part, &mut payload);
        w.write_audio(1, n as u64 * 100, &payload).unwrap();
    }
    let file = w.finish(1_000_000).unwrap().into_inner();
    let mut p = Player::open(Cursor::new(file), &caps()).unwrap();
    let out = p.audio_output(0).unwrap();
    let close = |got: &[f32], from: usize| {
        for (i, &s) in got.iter().enumerate() {
            let want = tone[from + i] as f32 / 32768.0;
            assert!((s - want).abs() < 0.01, "frame {}: {s} vs {want}", from + i);
        }
    };

    p.play().unwrap();
    let mut buf = vec![0.0; 1000];
    let n = out.read(&mut buf);
    assert_eq!(n, 300, "chunks due at 0: 200 ms preload, whole chunks");
    close(&buf[..n], 0);

    p.seek(Duration::from_millis(250)).unwrap();
    let n = out.read(&mut buf);
    assert_eq!(n, 250, "from the target to the chunks due at 250 ms");
    close(&buf[..n], 250);
}

#[test]
fn pause_stops_reads_and_keeps_samples() {
    let mut p = open(caps());
    let out = p.audio_output(0).unwrap();
    p.play().unwrap();
    assert_eq!(read_frames(&out, 10), range(0, 10));
    p.pause();
    assert_eq!(out.read(&mut [0.0; 64]), 0);
    p.play().unwrap();
    assert_eq!(read_frames(&out, 10), range(10, 20));
}

#[test]
fn late_reads_skip_to_the_clock() {
    let mut p = open(caps());
    let out = p.audio_output(0).unwrap();
    p.play().unwrap();
    p.advance(50 * MS).unwrap();
    assert_eq!(read_frames(&out, 5), range(0, 5), "50 ms behind is within the tolerance");
    p.advance(200 * MS).unwrap();
    assert_eq!(read_frames(&out, 5), range(250, 255), "245 ms behind skips");

    p.set_audio_sync_tolerance(Duration::from_secs(1));
    p.advance(150 * MS).unwrap();
    assert_eq!(read_frames(&out, 5), range(255, 260));
}

#[test]
fn seek_refills_from_the_target() {
    let mut p = open(caps());
    let out = p.audio_output(0).unwrap();
    p.seek(330 * MS).unwrap();
    assert_eq!(out.read(&mut [0.0; 64]), 0, "stopped");
    p.play().unwrap();
    assert_eq!(read_frames(&out, 1000), range(330, 500), "decoded up to the end of the audio");

    p.seek(120 * MS).unwrap();
    assert_eq!(read_frames(&out, 1000), range(120, 400), "decoded up to 120 + 200 ms, whole chunks");
}

#[test]
fn loops_seamlessly_with_reads_ahead() {
    let mut p = open(caps());
    let out = p.audio_output(0).unwrap();
    p.set_looping(true);
    p.play().unwrap();
    // The reader keeps 50 ms ahead of the clock, like an audio callback filling a buffer.
    let mut heard = read_frames(&out, 50);
    for _ in 0..120 {
        p.advance(10 * MS).unwrap();
        let got = read_frames(&out, 10);
        assert_eq!(got.len(), 10, "underrun at {:?} after {} frames", p.position(), heard.len());
        heard.extend(got);
    }
    let expected: Vec<u64> = (0..500).cycle().take(heard.len()).collect();
    assert_eq!(heard, expected);
}

#[test]
fn finished_and_disabled_tracks_return_nothing() {
    let mut p = open(caps());
    let voice = p.audio_output(1).unwrap();
    p.set_audio_track_enabled(1, false);
    p.play().unwrap();
    assert_eq!(voice.read(&mut [0.0; 64]), 0);

    p.advance(150 * MS).unwrap();
    p.set_audio_track_enabled(1, true);
    assert_eq!(voice.read(&mut [0.0; 64]), 0, "nothing queued yet");
    p.advance(50 * MS).unwrap();
    // Voice chunk 300 is due at 100 ms and passed; the next ones are past the end of the voice.
    assert_eq!(voice.read(&mut [0.0; 64]), 0);

    let music = p.audio_output(0).unwrap();
    p.advance(Duration::from_secs(1)).unwrap();
    assert!(p.is_finished());
    assert_eq!(music.read(&mut [0.0; 64]), 0);
    p.play().unwrap();
    assert_eq!(read_frames(&music, 3), range(0, 3), "play after finish starts over");
}

#[test]
fn outputs_work_from_another_thread() {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<AudioOutput>();
    let mut p = open(caps());
    let out = p.audio_output(0).unwrap();
    p.play().unwrap();
    let frames = std::thread::spawn(move || read_frames(&out, 100)).join().unwrap();
    assert_eq!(frames, range(0, 100));
}

/// `seconds` of 8x4 BC1 video at 1 fps and one mono f32 track at 1000 Hz whose frame `i`
/// holds `i / 1000`, in 100-frame chunks with 200 ms preload.
fn long_file(seconds: u64) -> Vec<u8> {
    let video = VideoParams { width: 8, height: 4, format: TextureFormat::Bc1, color_flags: COLOR_FLAG_SRGB };
    let streams = vec![StreamInfo::video(0, TimeBase::new(1, 1), video), audio_stream(0, "music", 1, SampleFormat::F32)];
    let mut chunks: Vec<(u64, u16, u64)> = (0..seconds).map(|n| (n * 1000, 0, n)).collect();
    chunks.extend((0..seconds * 1000).step_by(100).map(|pts| (pts.saturating_sub(200), 1, pts)));
    chunks.sort_by_key(|&(t, stream, _)| (t, stream != 0));
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    for (_, stream, pts) in chunks {
        match stream {
            0 => w.write_chunk(0, pts, true, &[pts as u8; 16], None).unwrap(),
            _ => w.write_audio(1, pts, &samples(pts, pts + 100, 1, SampleFormat::F32)).unwrap(),
        }
    }
    w.finish(seconds * 1_000_000).unwrap().into_inner()
}

/// An audio device whose clock runs at `speed` times the game clock. Its callback reads
/// 10-frame (10 ms) buffers one buffer ahead of what it has played, so its output latency
/// is 10 ms.
struct Device {
    out: AudioOutput,
    speed: f64,
    /// Milliseconds played by the device.
    played: f64,
    /// Frame numbers returned by reads.
    frames: Vec<u64>,
    reads: u64,
}

impl Device {
    const LATENCY: Duration = Duration::from_millis(10);

    fn new(out: AudioOutput, speed: f64) -> Device {
        Device { out, speed, played: 0.0, frames: Vec::new(), reads: 0 }
    }

    /// Plays `dt` of game time.
    fn run(&mut self, dt: Duration) {
        self.played += dt.as_secs_f64() * 1000.0 * self.speed;
        while (self.reads as f64) * 10.0 < self.played + 10.0 {
            let mut buf = [0.0; 10];
            let n = self.out.read(&mut buf);
            self.frames.extend(buf[..n].iter().map(|v| (v * 1000.0).round() as u64));
            self.reads += 1;
        }
    }

    /// Heard position in milliseconds: one past the last frame read, minus the latency.
    fn heard_ms(&self) -> f64 {
        self.frames.last().map_or(0.0, |&f| f as f64 + 1.0 - 10.0)
    }
}

/// Plays `steps` game frames of 16 ms with the device; returns the positions after each.
fn play_with(p: &mut Player<Cursor<Vec<u8>>>, device: &mut Device, steps: usize) -> Vec<f64> {
    let dt = 16 * MS;
    (0..steps)
        .map(|_| {
            device.run(dt);
            p.advance(dt).unwrap();
            p.position().as_secs_f64() * 1000.0
        })
        .collect()
}

fn contiguous(frames: &[u64]) -> bool {
    frames.windows(2).all(|w| w[1] == w[0] + 1)
}

#[test]
fn audio_clock_follows_device_drift() {
    for speed in [1.01, 0.99] {
        let mut p = Player::open(Cursor::new(long_file(20)), &caps()).unwrap();
        p.set_clock(Clock::Audio { track: 0, latency: Device::LATENCY });
        let mut device = Device::new(p.audio_output(0).unwrap(), speed);
        p.play().unwrap();
        let positions = play_with(&mut p, &mut device, 625); // 10 s of game time
        assert!(positions.windows(2).all(|w| w[1] >= w[0]), "speed {speed}: the clock never goes back");
        assert_eq!(device.frames[0], 0);
        assert!(contiguous(&device.frames), "speed {speed}: no skipped or repeated audio");
        let last = *positions.last().unwrap();
        assert!((last - device.heard_ms()).abs() < 10.0, "speed {speed}: clock {last} ms, heard {} ms", device.heard_ms());
        assert!((last - 10_000.0 * speed).abs() < 20.0, "speed {speed}: clock {last} ms follows the device");
    }

    // With the game clock, a slow device falls behind and its reads skip forward.
    let mut p = Player::open(Cursor::new(long_file(20)), &caps()).unwrap();
    let mut device = Device::new(p.audio_output(0).unwrap(), 0.99);
    p.play().unwrap();
    let positions = play_with(&mut p, &mut device, 625);
    assert_eq!(*positions.last().unwrap(), 10_000.0);
    assert!(!contiguous(&device.frames));
}

#[test]
fn audio_clock_runs_on_dt_while_audio_stalls() {
    let mut p = Player::open(Cursor::new(long_file(20)), &caps()).unwrap();
    p.set_clock(Clock::Audio { track: 0, latency: Device::LATENCY });
    let mut device = Device::new(p.audio_output(0).unwrap(), 1.0);
    p.play().unwrap();
    play_with(&mut p, &mut device, 60); // 960 ms
    let before = p.position().as_secs_f64() * 1000.0;
    // The engine stops reading for 480 ms.
    for _ in 0..30 {
        p.advance(16 * MS).unwrap();
    }
    let stalled = p.position().as_secs_f64() * 1000.0;
    assert!(stalled - before > 400.0, "the clock went on: {before} -> {stalled} ms");
    // Reads resume behind the clock and skip forward to it, then the clock follows them.
    let resumed_at = device.frames.len();
    device.played += 480.0;
    play_with(&mut p, &mut device, 60);
    let first = device.frames[resumed_at];
    assert!(first as f64 >= stalled - 1.0, "first read after the stall at frame {first}, clock {stalled} ms");
    assert!(contiguous(&device.frames[resumed_at..]));
    let last = p.position().as_secs_f64() * 1000.0;
    assert!((last - device.heard_ms()).abs() < 15.0, "clock {last} ms, heard {} ms", device.heard_ms());
}

#[test]
fn audio_clock_jumps_to_audio_ahead() {
    let mut p = Player::open(Cursor::new(long_file(20)), &caps()).unwrap();
    p.set_clock(Clock::Audio { track: 0, latency: Duration::ZERO });
    let out = p.audio_output(0).unwrap();
    p.play().unwrap();
    let mut buf = [0.0; 150];
    assert_eq!(out.read(&mut buf), 150);
    p.advance(16 * MS).unwrap();
    assert_eq!(p.position(), 150 * MS, "150 ms heard, 134 ms ahead of the clock");
}

#[test]
fn audio_clock_without_master_runs_on_dt() {
    for clock in [Clock::Audio { track: 0, latency: Duration::ZERO }, Clock::Audio { track: 7, latency: Duration::ZERO }] {
        let mut p = Player::open(Cursor::new(long_file(20)), &caps()).unwrap();
        p.set_clock(clock);
        assert_eq!(p.clock(), clock);
        p.set_audio_track_enabled(0, false);
        p.play().unwrap();
        for _ in 0..100 {
            p.advance(16 * MS).unwrap();
        }
        assert_eq!(p.position(), 1600 * MS, "{clock:?}");
    }
}
