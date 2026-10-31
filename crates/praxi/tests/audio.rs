use praxi::format::{AudioParams, COLOR_FLAG_SRGB, SampleFormat, StreamInfo, TimeBase, VideoParams, Writer};
use praxi::{AudioOutput, Capabilities, Player, TextureFormat};
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

    p.play();
    assert_eq!(out.read(&mut [0.0; 64]), 0, "nothing decoded before the first advance");
    p.advance(Duration::ZERO).unwrap();
    assert_eq!(read_frames(&out, 1000), range(0, 300), "chunks due at 0 cover 300 ms");
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
    p.play();
    p.advance(Duration::ZERO).unwrap();
    let mut buf = [0.0; 4];
    assert_eq!(out.read(&mut buf), 4);
    assert_eq!(buf, [0.0, 0.001, 0.002, 0.003]);
}

#[test]
fn pause_stops_reads_and_keeps_samples() {
    let mut p = open(caps());
    let out = p.audio_output(0).unwrap();
    p.play();
    p.advance(Duration::ZERO).unwrap();
    assert_eq!(read_frames(&out, 10), range(0, 10));
    p.pause();
    assert_eq!(out.read(&mut [0.0; 64]), 0);
    p.play();
    assert_eq!(read_frames(&out, 10), range(10, 20));
}

#[test]
fn late_reads_skip_to_the_clock() {
    let mut p = open(caps());
    let out = p.audio_output(0).unwrap();
    p.play();
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
    p.play();
    assert_eq!(read_frames(&out, 1000), range(330, 500), "decoded up to the end of the audio");

    p.seek(120 * MS).unwrap();
    assert_eq!(read_frames(&out, 1000), range(120, 400), "decoded up to 120 + 200 ms, whole chunks");
}

#[test]
fn loops_seamlessly_with_reads_ahead() {
    let mut p = open(caps());
    let out = p.audio_output(0).unwrap();
    p.set_looping(true);
    p.play();
    p.advance(Duration::ZERO).unwrap();
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
    p.play();
    p.advance(Duration::ZERO).unwrap();
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
    p.play();
    p.advance(Duration::ZERO).unwrap();
    assert_eq!(read_frames(&music, 3), range(0, 3), "play after finish starts over");
}

#[test]
fn outputs_work_from_another_thread() {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<AudioOutput>();
    let mut p = open(caps());
    let out = p.audio_output(0).unwrap();
    p.play();
    p.advance(Duration::ZERO).unwrap();
    let frames = std::thread::spawn(move || read_frames(&out, 100)).join().unwrap();
    assert_eq!(frames, range(0, 100));
}
