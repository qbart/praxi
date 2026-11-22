use praxi::format::{Cue, Reader, StreamParams};
use praxi::{Capabilities, Event, Player, TextureFormat};
use praxi_encoder::{
    AudioTrackConfig, CueConfig, CueStreamConfig, Encoder, EncoderConfig, SampleFormat, Stats, Value, VariantConfig,
};
use std::io::Cursor;
use std::time::Duration;

const W: u32 = 18;
const H: u32 = 10;

/// Six frames: static gradient, a changed square in frame 3, static again.
fn frames() -> Vec<Vec<u8>> {
    let base: Vec<u8> = (0..W * H)
        .flat_map(|i| {
            let (x, y) = (i % W, i / W);
            [(x * 14) as u8, (y * 25) as u8, ((x + y) * 9) as u8, 255]
        })
        .collect();
    let mut changed = base.clone();
    for y in 2..6 {
        for x in 3..7 {
            let i = ((y * W + x) * 4) as usize;
            changed[i..i + 4].copy_from_slice(&[255, 0, 0, 128]);
        }
    }
    vec![base.clone(), base.clone(), base.clone(), changed.clone(), changed.clone(), changed]
}

fn encode(config: EncoderConfig) -> (Vec<u8>, Stats) {
    let mut enc = Encoder::new(Cursor::new(Vec::new()), config.threads(2)).unwrap();
    for f in frames() {
        enc.push_frame(&f).unwrap();
    }
    let (out, stats) = enc.finish().unwrap();
    (out.into_inner(), stats)
}

fn config() -> EncoderConfig {
    EncoderConfig::new(W, H, 10, 1).variant(VariantConfig::bc7()).variant(VariantConfig::astc(6))
}

/// Every frame of `file` for `format`, as reconstructed by the player.
fn play_all(file: Vec<u8>, format: TextureFormat) -> (Vec<Vec<u8>>, Vec<Event>) {
    let mut p = Player::open(Cursor::new(file), &Capabilities::new([format])).unwrap();
    p.play().unwrap();
    let mut out = Vec::new();
    let mut events = Vec::new();
    p.advance(Duration::ZERO).unwrap();
    while !p.is_finished() {
        let f = p.video_frame(0).unwrap();
        if out.len() < 6 && f.time == Duration::from_millis(100 * out.len() as u64) {
            out.push(f.data.to_vec());
        }
        p.advance(Duration::from_millis(50)).unwrap();
        events.extend(std::iter::from_fn(|| p.poll_event()));
    }
    (out, events)
}

#[test]
fn writes_streams_cues_and_deltas() {
    let gameplay = CueStreamConfig::new("gameplay")
        .cue(CueConfig::new(0.2, "camera.shake").duration(0.15).field("axis", "x").field("level", 2i64));
    let config = config()
        .keyint(4)
        .skip_threshold(0)
        .cue_with(CueConfig::new(1.0, "end").duration(0.5))
        .cue(0.05, "m1")
        .cue_stream(gameplay);
    let (file, stats) = encode(config);
    assert_eq!(stats.frames, 6);
    assert_eq!(stats.duration_us, 1_500_000, "raised to the last cue end");

    let mut r = Reader::new(Cursor::new(file)).unwrap();
    let streams = r.streams().to_vec();
    assert_eq!(streams.len(), 4);
    let bc7 = streams[0].video_params().unwrap();
    assert_eq!((bc7.format, bc7.width, bc7.height, bc7.block_count()), (TextureFormat::Bc7, W, H, 5 * 3));
    assert!(bc7.srgb() && !bc7.alpha());
    assert_eq!(streams[1].video_params().unwrap().format, TextureFormat::Astc6x6);
    let counts: Vec<u32> = streams.iter().map(|s| s.chunk_count).collect();
    assert_eq!(counts, [6, 6, 2, 1]);
    let cue_names: Vec<(&str, u64)> = streams[2..]
        .iter()
        .map(|s| match &s.params {
            StreamParams::Cues(c) => (c.name.as_str(), c.max_duration),
            p => panic!("not a cue stream: {p:?}"),
        })
        .collect();
    assert_eq!(cue_names, [("cues", 500_000), ("gameplay", 150_000)]);

    let mut chunks = Vec::new();
    let mut payload = Vec::new();
    while let Some(c) = r.next_chunk().unwrap() {
        r.read_payload(&c, &mut payload).unwrap();
        chunks.push((c, payload.clone()));
    }
    let bc7_chunks: Vec<_> = chunks.iter().filter(|(c, _)| c.stream == 0).collect();
    let keys: Vec<bool> = bc7_chunks.iter().map(|(c, _)| c.keyframe()).collect();
    assert_eq!(keys, [true, false, false, false, true, false]);
    for n in [1, 2, 5] {
        assert_eq!(bc7_chunks[n].1, vec![0u8; bc7.mask_size()], "static frame {n} skips every block");
    }
    let changed: u32 = bc7_chunks[3].1[..bc7.mask_size()].iter().map(|b| b.count_ones()).sum();
    assert!(changed > 0 && changed < 15, "frame 3 updates only the changed square ({changed} blocks)");

    let cues: Vec<(u16, u64, Cue)> =
        chunks.iter().filter(|(c, _)| c.stream >= 2).map(|(c, p)| (c.stream, c.pts, Cue::decode(p).unwrap())).collect();
    let shake = Cue {
        duration: 150_000,
        kind: "camera.shake".into(),
        fields: vec![("axis".into(), Value::from("x")), ("level".into(), Value::Int(2))],
    };
    assert_eq!(
        cues,
        [
            (2, 50_000, Cue::new("m1")),
            (3, 200_000, shake),
            (2, 1_000_000, Cue { duration: 500_000, ..Cue::new("end") }),
        ],
        "cues sorted by time across streams"
    );
    assert!(stats.variants[0].skipped_blocks > 0);
}

#[test]
fn player_reconstructs_keyframe_encoding() {
    for format in [TextureFormat::Bc7, TextureFormat::Astc6x6] {
        let (deltas, events) = play_all(encode(config().keyint(4).skip_threshold(0).cue(0.25, "m")).0, format);
        let (keys, _) = play_all(encode(config().keyint(1)).0, format);
        assert_eq!(deltas.len(), 6);
        assert_eq!(deltas, keys, "{format}: delta reconstruction matches all-keyframe encoding");
        assert!(events.iter().any(|e| matches!(e, Event::Cue(c) if c.kind == "m" && c.time == Duration::from_millis(250))));
    }
}

#[test]
fn rejects_bad_config() {
    let bad = [
        EncoderConfig::new(0, 4, 30, 1).variant(VariantConfig::bc7()),
        EncoderConfig::new(4, 4, 30, 1),
        EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::astc(5)),
        EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::bc7()).cue(-1.0, "a"),
        EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::bc7()).cue(1.0, ""),
        EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::bc7()).cue_with(CueConfig::new(1.0, "a").duration(f64::NAN)),
        EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::bc7()).cue_stream(CueStreamConfig::new("x".repeat(40)).cue(CueConfig::new(0.0, "a"))),
        EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::bc7()).plane_min_saving(-1.0),
        EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::bc7()).plane_min_saving(f64::NAN),
        EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::bc7()).plane_min_saving(101.0),
    ];
    for config in bad {
        assert!(Encoder::new(Cursor::new(Vec::new()), config).is_err());
    }
}

#[test]
fn decodes_frames_close_to_source() {
    let source = frames();
    // The synthetic gradient is steep (14-25 levels per pixel), hard for block formats; the
    // threshold catches decode bugs (channel order, block placement), not codec quality.
    for format in [TextureFormat::Bc7, TextureFormat::Astc4x4, TextureFormat::Astc6x6] {
        let variant = match format {
            TextureFormat::Bc7 => VariantConfig::bc7(),
            TextureFormat::Astc4x4 => VariantConfig::astc(4),
            _ => VariantConfig::astc(6),
        };
        let (file, _) = encode(EncoderConfig::new(W, H, 10, 1).variant(variant));
        let mut p = Player::open(Cursor::new(file), &Capabilities::new([format])).unwrap();
        p.seek(Duration::from_millis(300)).unwrap();
        let params = p.tracks()[0].params;
        let rgba = praxi_encoder::decode_frame(&params, p.video_frame(0).unwrap().data).unwrap();
        assert_eq!(rgba.len(), (W * H * 4) as usize, "{format}: padding cropped");
        let se: f64 = rgba.iter().zip(&source[3]).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum();
        let psnr = 10.0 * (255.0f64.powi(2) / (se / rgba.len() as f64)).log10();
        assert!(psnr > 25.0, "{format}: PSNR {psnr:.1} dB");
    }
}

#[test]
fn bc1_is_opaque_and_half_of_bc7() {
    let source = frames();
    for quality in [praxi_encoder::Quality::Fast, praxi_encoder::Quality::Thorough] {
        let config = EncoderConfig::new(W, H, 10, 1).variant(VariantConfig::bc1().quality(quality)).alpha(true);
        let (file, stats) = encode(config.skip_threshold(0));
        let bc7 = encode(EncoderConfig::new(W, H, 10, 1).variant(VariantConfig::bc7()).skip_threshold(0)).1;
        assert_eq!(stats.variants[0].format, Some(TextureFormat::Bc1));
        let mut p = Player::open(Cursor::new(file), &Capabilities::new([TextureFormat::Bc1])).unwrap();
        let params = p.tracks()[0].params;
        assert!(!params.alpha(), "BC1 streams are opaque even with alpha in the source");
        assert_eq!(params.frame_size() * 2, praxi::VideoParams { format: TextureFormat::Bc7, ..params }.frame_size());
        assert!(stats.variants[0].bytes < bc7.variants[0].bytes, "{} >= {}", stats.variants[0].bytes, bc7.variants[0].bytes);
        p.seek(Duration::from_millis(300)).unwrap();
        let data = p.video_frame(0).unwrap().data.to_vec();
        for block in data.as_chunks::<8>().0 {
            let (c0, c1) = (u16::from_le_bytes([block[0], block[1]]), u16::from_le_bytes([block[2], block[3]]));
            assert!(c0 > c1, "four-color mode");
        }
        let rgba = praxi_encoder::decode_frame(&params, &data).unwrap();
        assert!(rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 255), "decodes opaque");
        let se: f64 = rgba
            .as_chunks::<4>().0.iter()
            .zip(source[3].as_chunks::<4>().0)
            .flat_map(|(a, b)| (0..3).map(move |c| (a[c] as f64 - b[c] as f64).powi(2)))
            .sum();
        let psnr = 10.0 * (255.0f64.powi(2) / (se / (W * H * 3) as f64)).log10();
        assert!(psnr > 25.0, "{quality:?}: PSNR {psnr:.1} dB");
    }
}

#[test]
fn decode_covers_every_block_format() {
    // A zero BC4 / BC5 block is red (and green) 0; BC4 and BC5 come out as R00A and RG0A.
    for (format, bytes, pixel) in [
        (TextureFormat::Bc3, 16, None),
        (TextureFormat::Bc4, 8, Some([0, 0, 0, 255])),
        (TextureFormat::Bc5, 16, Some([0, 0, 0, 255])),
    ] {
        let params = praxi::VideoParams::new(4, 4, format, 0);
        let rgba = praxi_encoder::decode_frame(&params, &vec![0; bytes]).expect("decodes");
        assert_eq!(rgba.len(), 4 * 4 * 4, "{format}");
        if let Some(px) = pixel {
            assert!(rgba.as_chunks::<4>().0.iter().all(|p| *p == px), "{format}: {:?}", &rgba[..4]);
        }
    }
    let params = praxi::VideoParams::new(4, 4, TextureFormat::Bc7, 0);
    assert!(praxi_encoder::decode_frame(&params, &[0; 8]).is_err(), "wrong data size");
}

/// `seconds` of interleaved audio at 8 kHz: a different sine per channel, past full scale
/// at the peaks of channel 0 to exercise clamping.
fn tone(channels: u16, seconds: f64) -> Vec<f32> {
    let frames = (8000.0 * seconds) as usize;
    (0..frames)
        .flat_map(|i| (0..channels).map(move |c| (i as f32 * 0.01 * (c + 1) as f32).sin() * if c == 0 { 1.2 } else { 0.5 }))
        .collect()
}

/// Plays `file` to the end, reading every audio track as it decodes.
fn play_audio(file: Vec<u8>, caps: Capabilities) -> (Player<Cursor<Vec<u8>>>, Vec<Vec<f32>>) {
    let mut p = Player::open(Cursor::new(file), &caps).unwrap();
    let outputs: Vec<_> = (0..p.audio_tracks().len()).map(|t| p.audio_output(t).unwrap()).collect();
    let mut heard = vec![Vec::new(); outputs.len()];
    p.play().unwrap();
    while !p.is_finished() {
        for (out, h) in outputs.iter().zip(&mut heard) {
            let mut buf = [0.0; 4096];
            let n = out.read(&mut buf);
            h.extend_from_slice(&buf[..n]);
        }
        p.advance(Duration::from_millis(50)).unwrap();
    }
    (p, heard)
}

#[test]
fn encodes_audio_tracks() {
    let config = config()
        .audio(AudioTrackConfig::from_samples("music", 8000, 2, tone(2, 1.0)).channel_mask(0x3))
        .audio(AudioTrackConfig::from_samples("voice", 8000, 1, tone(1, 0.3)).group(0).format(SampleFormat::F32))
        .audio(AudioTrackConfig::from_samples("music", 8000, 1, tone(1, 1.0)).format(SampleFormat::F32))
        .cue(0.2, "m");
    let (file, stats) = encode(config);
    assert_eq!(stats.duration_us, 1_000_000, "audio outlasts the 600 ms of video");
    let audio: Vec<_> = stats.audio.iter().map(|a| (a.name.as_str(), a.channels, a.frames, a.chunks)).collect();
    assert_eq!(audio, [("music", 2, 8000, 10), ("voice", 1, 2400, 3), ("music", 1, 8000, 10)]);

    let r = Reader::new(Cursor::new(file.clone())).unwrap();
    let kinds: Vec<_> = r
        .streams()
        .iter()
        .map(|s| match &s.params {
            StreamParams::Video(_) => ("video".to_string(), s.group),
            StreamParams::Audio(a) => (format!("{} {}ch {}", a.name, a.channels, a.format), s.group),
            StreamParams::Cues(c) => (c.name.clone(), s.group),
            StreamParams::Unknown(_) => ("?".to_string(), s.group),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("video".to_string(), 0),
            ("video".to_string(), 0),
            ("music 2ch s16".to_string(), 1),
            ("voice 1ch f32".to_string(), 0),
            ("music 1ch f32".to_string(), 1),
            ("cues".to_string(), 0),
        ],
        "same names share a group; explicit group 0 pushes music to group 1"
    );
    let a = r.streams()[2].audio_params().unwrap();
    assert_eq!((a.sample_rate, a.channel_mask, a.preload_us), (8000, 0x3, 250_000));

    let (p, heard) = play_audio(file.clone(), Capabilities::new([TextureFormat::Bc7]));
    let names: Vec<_> = p.audio_tracks().iter().map(|t| (t.params.name.as_str(), t.stream)).collect();
    assert_eq!(names, [("voice", 3), ("music", 2)], "audio tracks in group order");
    assert_eq!(heard[0], tone(1, 0.3), "f32 is stored exactly");
    let source = tone(2, 1.0);
    assert_eq!(heard[1].len(), source.len());
    for (got, want) in heard[1].iter().zip(&source) {
        // s16 tops out at 32767 / 32768.
        assert!((got - want.clamp(-1.0, 32767.0 / 32768.0)).abs() <= 0.5 / 32768.0 + 1e-6, "s16 {got} vs {want}");
    }

    let (_, heard) = play_audio(file, Capabilities::new([TextureFormat::Bc7]).max_audio_channels(1));
    assert_eq!(heard[1], tone(1, 1.0), "mono variant");
}

#[test]
fn encodes_adpcm_audio() {
    let config = config().audio(AudioTrackConfig::from_samples("music", 8000, 2, tone(2, 1.0)).format(SampleFormat::Adpcm));
    let (file, stats) = encode(config);
    // 10 chunks of 800 frames: 4 + 8 + 9 * 2 * 50 bytes each, unless LZ4 makes them smaller.
    assert!(stats.audio[0].bytes <= 10 * (24 + 912), "{} bytes", stats.audio[0].bytes);
    let (_, heard) = play_audio(file, Capabilities::new([TextureFormat::Bc7]));
    let source: Vec<f32> = tone(2, 1.0).iter().map(|s| s.clamp(-1.0, 1.0)).collect();
    assert_eq!(heard[0].len(), source.len());
    let signal: f32 = source.iter().map(|s| s * s).sum();
    let noise: f32 = heard[0].iter().zip(&source).map(|(a, b)| (a - b) * (a - b)).sum();
    let snr = 10.0 * (signal / noise).log10();
    assert!(snr > 40.0, "SNR {snr} dB");
}

#[test]
fn rejects_bad_audio_config() {
    let base = || EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::bc7());
    let bad = [
        base().audio(AudioTrackConfig::from_samples("a", 0, 2, vec![])),
        base().audio(AudioTrackConfig::from_samples("a", 8000, 0, vec![])),
        base().audio(AudioTrackConfig::from_samples("a", 8000, 2, vec![]).channel_mask(0x7)),
        base().audio(AudioTrackConfig::from_samples("x".repeat(40), 8000, 2, vec![])),
        base().audio(AudioTrackConfig::from_samples("a", 8000, 2, vec![])).audio_chunk(Duration::ZERO),
    ];
    for config in bad {
        assert!(Encoder::new(Cursor::new(Vec::new()), config).is_err());
    }
}

/// A textured 72x40 frame shifted right by `shift` pixels, with alpha.
fn moving_frame(shift: usize) -> Vec<u8> {
    let (w, h) = (72, 40);
    (0..w * h)
        .flat_map(|i| {
            let (x, y) = ((i % w + 1000 - shift) % 1000, i / w);
            let v = ((x * 7 + y * 3) % 64 * 3 + (x / 8 + y / 8) % 2 * 40) as u8;
            // Detail in luma, smooth color, as in camera footage (4:2:0 keeps it).
            [v / 2 + (i % w) as u8, v / 2 + 40, v / 2 + (y * 3) as u8, (y * 6) as u8]
        })
        .collect()
}

#[test]
fn motion_frames_decode_close_to_the_source() {
    use praxi::format::motion;
    let frames: Vec<Vec<u8>> = (0..7).map(|i| moving_frame(i * 3)).collect();
    for chroma in [praxi_encoder::Chroma::C420, praxi_encoder::Chroma::C444] {
        let config = EncoderConfig::new(72, 40, 10, 1)
            .variant(VariantConfig::motion().qp(10).slices(2).chroma(chroma))
            .keyint(4)
            .alpha(true)
            .threads(3);
        let mut enc = Encoder::new(Cursor::new(Vec::new()), config).unwrap();
        for f in &frames {
            enc.push_frame(f).unwrap();
        }
        let (out, stats) = enc.finish().unwrap();
        let m = stats.variants[0].motion.unwrap();
        assert!(m.inter + m.skip > 0, "delta frames use motion: {m:?}");

        let mut reader = Reader::new(Cursor::new(out.into_inner())).unwrap();
        let params = *reader.streams()[0].video_params().unwrap();
        assert_eq!(params.motion.chroma, chroma);
        let mut decoder = motion::Decoder::new(&params);
        let layout = decoder.layout().clone();
        let (mut planes, mut reference) = (vec![0; layout.size], vec![0; layout.size]);
        let (mut payload, mut coded, mut rgba) = (Vec::new(), motion::CodedFrame::default(), Vec::new());
        let mut keyframes = Vec::new();
        while let Some(chunk) = reader.next_chunk().unwrap() {
            reader.read_payload(&chunk, &mut payload).unwrap();
            decoder.decode(chunk.keyframe(), &payload, &mut coded).unwrap();
            std::mem::swap(&mut planes, &mut reference);
            motion::reconstruct(&layout, &coded, &reference, &mut planes);
            motion::to_rgba(&layout, &planes, &mut rgba);
            let source = &frames[chunk.pts as usize];
            let mse = rgba.iter().zip(source).map(|(&a, &b)| (a as f64 - b as f64).powi(2)).sum::<f64>() / rgba.len() as f64;
            let psnr = 10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10();
            assert!(psnr > 44.0, "{chroma:?} frame {}: {psnr:.1} dB", chunk.pts);
            keyframes.push(chunk.keyframe());
        }
        assert_eq!(keyframes.len(), 7);
        assert!(keyframes[0] && keyframes[4], "{keyframes:?}");
    }
}

#[test]
fn player_queues_motion_frames() {
    use praxi::format::motion;
    let config = EncoderConfig::new(72, 40, 10, 1)
        .variant(VariantConfig::motion().qp(20).slices(2))
        .keyint(4)
        .threads(2);
    let mut enc = Encoder::new(Cursor::new(Vec::new()), config).unwrap();
    for i in 0..10 {
        enc.push_frame(&moving_frame(i * 3)).unwrap();
    }
    let bytes = enc.finish().unwrap().0.into_inner();

    let mut reader = Reader::new(Cursor::new(bytes.clone())).unwrap();
    let params = *reader.streams()[0].video_params().unwrap();
    let mut decoder = motion::Decoder::new(&params);
    let mut expected = Vec::new();
    let mut payload = Vec::new();
    while let Some(chunk) = reader.next_chunk().unwrap() {
        reader.read_payload(&chunk, &mut payload).unwrap();
        let mut coded = motion::CodedFrame::default();
        decoder.decode(chunk.keyframe(), &payload, &mut coded).unwrap();
        expected.push(coded);
    }

    // Decoding frames `from..=to` queues them from the last keyframe among them on.
    let queued = |from: usize, to: usize| {
        let start = (from..=to).rev().find(|&i| expected[i].keyframe).unwrap_or(from);
        &expected[start..=to]
    };
    assert!(expected.iter().filter(|f| f.keyframe).count() < 6, "most frames are delta frames");

    let step = |p: &mut Player<_>, frames: u64| p.advance(Duration::from_millis(100 * frames)).unwrap();
    // One thread decodes every frame when due; with three, frames start decoding one
    // `advance` early.
    for threads in [1, 3] {
        let mut p = Player::open(Cursor::new(bytes.clone()), &Capabilities::new([TextureFormat::Motion])).unwrap();
        assert_eq!(p.tracks()[0].params.format, TextureFormat::Motion);
        p.set_decode_threads(threads);
        p.play().unwrap();
        let f = p.video_frame(0).unwrap();
        assert!(f.data.is_empty());
        assert_eq!(p.take_coded_frames(0), queued(0, 0));
        assert!(p.take_coded_frames(0).is_empty(), "taking empties the queue");
        let mut at = 0;
        for frames in [1, 1, 1, 1, 2, 1, 2] {
            step(&mut p, frames as u64);
            let want = queued(at + 1, at + frames);
            assert_eq!(p.take_coded_frames(0), want, "{threads} threads, frames {}..={}", at + 1, at + frames);
            at += frames;
        }
        p.seek(Duration::from_millis(700)).unwrap();
        // Seeking decodes from the keyframe before the target.
        assert_eq!(p.take_coded_frames(0), queued(0, 7));
        // A seek onto the frame being decoded ahead (keyframe 4) uses it; one elsewhere
        // cancels it.
        for (to, expect) in [(400, queued(0, 4)), (0, queued(0, 0))] {
            p.seek(Duration::from_millis(250)).unwrap();
            p.play().unwrap();
            step(&mut p, 1);
            p.take_coded_frames(0);
            p.seek(Duration::from_millis(to)).unwrap();
            assert_eq!(p.take_coded_frames(0), expect, "{threads} threads, seek to {to}");
        }
        p.play().unwrap();
        step(&mut p, 4);
        assert_eq!(p.take_coded_frames(0), queued(1, 4));
        assert!(p.take_coded_frames(1).is_empty(), "no such track");
    }
}

#[test]
fn cues_of_motion_files_are_replaced_without_touching_video() {
    use praxi::format::{CueTrack, TimedCue};
    let config = EncoderConfig::new(72, 40, 10, 1)
        .variant(VariantConfig::motion().qp(20).slices(2))
        .keyint(4)
        .threads(2)
        .cue(0.15, "old");
    let mut enc = Encoder::new(Cursor::new(Vec::new()), config).unwrap();
    for i in 0..6 {
        enc.push_frame(&moving_frame(i * 3)).unwrap();
    }
    let bytes = enc.finish().unwrap().0.into_inner();
    let tracks = [CueTrack {
        name: "gameplay".into(),
        cues: vec![
            TimedCue { time_us: 250_000, cue: Cue { duration: 100_000, ..Cue::new("new") } },
            TimedCue { time_us: 0, cue: Cue::new("first") },
        ],
    }];
    let edited = praxi::format::replace_cues(Reader::new(Cursor::new(bytes.clone())).unwrap(), Cursor::new(Vec::new()), &tracks)
        .unwrap()
        .into_inner();

    let video = |bytes: &[u8]| {
        let mut reader = Reader::new(Cursor::new(bytes.to_vec())).unwrap();
        let mut chunks = Vec::new();
        while let Some(chunk) = reader.next_chunk().unwrap() {
            if chunk.stream == 0 {
                let mut payload = Vec::new();
                reader.read_payload(&chunk, &mut payload).unwrap();
                chunks.push((chunk.pts, chunk.keyframe(), payload));
            }
        }
        (*reader.streams()[0].video_params().unwrap(), chunks)
    };
    assert_eq!(video(&edited), video(&bytes), "video chunks are copied as they are");
    assert_eq!(praxi::format::read_cues(&mut Reader::new(Cursor::new(edited.clone())).unwrap()).unwrap()[0].cues[0].cue.kind, "first");

    let mut p = Player::open(Cursor::new(edited), &Capabilities::new([TextureFormat::Motion])).unwrap();
    p.play().unwrap();
    let mut events = Vec::new();
    while !p.is_finished() {
        p.advance(Duration::from_millis(50)).unwrap();
        p.take_coded_frames(0);
        events.extend(std::iter::from_fn(|| p.poll_event()));
    }
    let cues: Vec<(&str, Duration)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Cue(c) => Some((c.kind.as_str(), c.time)),
            _ => None,
        })
        .collect();
    assert_eq!(cues, [("first", Duration::ZERO), ("new", Duration::from_millis(250))]);
    assert!(events.iter().any(|e| matches!(e, Event::CueEnd { .. })), "the cue with a duration ends");
}

/// 48 kHz stereo: 440 Hz left, 660 Hz right.
fn stereo_48k(seconds: f64) -> Vec<f32> {
    let frames = (48000.0 * seconds) as usize;
    (0..frames)
        .flat_map(|i| {
            let t = i as f32 / 48000.0;
            [0.5 * (std::f32::consts::TAU * 440.0 * t).sin(), 0.3 * (std::f32::consts::TAU * 660.0 * t).sin()]
        })
        .collect()
}

fn snr(got: &[f32], want: &[f32]) -> f32 {
    assert_eq!(got.len(), want.len());
    let signal: f32 = want.iter().map(|s| s * s).sum();
    let noise: f32 = got.iter().zip(want).map(|(a, b)| (a - b) * (a - b)).sum();
    10.0 * (signal / noise.max(1e-20)).log10()
}

/// Plays track 0 of `file` from `from`, reading everything decoded at each 50 ms step,
/// for `steps` steps or to the end.
fn play_48k(file: Vec<u8>, from: Duration, looping: bool, steps: usize) -> Vec<f32> {
    let mut p = Player::open(Cursor::new(file), &Capabilities::new([TextureFormat::Bc7])).unwrap();
    let out = p.audio_output(0).unwrap();
    p.set_looping(looping);
    p.seek(from).unwrap();
    p.play().unwrap();
    let mut heard = Vec::new();
    let mut buf = vec![0.0; 96000];
    for _ in 0..steps {
        if p.is_finished() {
            break;
        }
        let n = out.read(&mut buf);
        heard.extend_from_slice(&buf[..n]);
        p.advance(Duration::from_millis(50)).unwrap();
    }
    heard
}

fn opus_file(seconds: f64) -> Vec<u8> {
    let track = AudioTrackConfig::from_samples("music", 48000, 2, stereo_48k(seconds)).channel_mask(0x3).format(SampleFormat::Opus);
    encode(config().audio(track)).0
}

#[test]
fn encodes_opus_audio() {
    let tracks = config()
        .audio(AudioTrackConfig::from_samples("music", 48000, 2, stereo_48k(1.0)).channel_mask(0x3).format(SampleFormat::Opus))
        .audio(AudioTrackConfig::from_samples("voice", 16000, 1, vec![0.25; 16000 / 3]).format(SampleFormat::Opus).bitrate(24000));
    let (file, stats) = encode(tracks);
    assert_eq!(stats.duration_us, 1_000_000);
    let audio: Vec<_> = stats.audio.iter().map(|a| (a.channels, a.frames)).collect();
    assert_eq!(audio, [(2, 48000), (1, 5333)]);
    // 128 kbps for 1 s, with the last chunk holding the encoder's delay.
    assert_eq!(stats.audio[0].chunks, 11);
    assert!((14_000..20_000).contains(&stats.audio[0].bytes), "{} bytes", stats.audio[0].bytes);

    let r = Reader::new(Cursor::new(file.clone())).unwrap();
    assert_eq!(r.streams()[2].audio_params().unwrap().format, SampleFormat::Opus);
    let heard = play_48k(file.clone(), Duration::ZERO, false, 100);
    let source = stereo_48k(1.0);
    assert_eq!(heard.len(), source.len(), "exact length: the encoder delay is dropped");
    // Lined up with the source frame for frame; the first packet starts from silence.
    let s = snr(&heard[960 * 2..], &source[960 * 2..]);
    assert!(s > 20.0, "SNR {s} dB");
    let (_, heard) = play_audio(file, Capabilities::new([TextureFormat::Bc7]));
    assert_eq!(heard[1].len(), 5333);

    let bad = AudioTrackConfig::from_samples("a", 44100, 2, vec![0.0; 100]).format(SampleFormat::Opus);
    assert!(Encoder::new(Cursor::new(Vec::new()), config().audio(bad)).is_err(), "44.1 kHz is not an Opus rate");
}

#[test]
fn opus_seeks_with_preroll() {
    let file = opus_file(1.0);
    let straight = play_48k(file.clone(), Duration::ZERO, false, 100);
    for at_ms in [0, 130, 550, 900] {
        let heard = play_48k(file.clone(), Duration::from_millis(at_ms), false, 100);
        let from = at_ms as usize * 48 * 2;
        assert_eq!(heard.len(), straight.len() - from, "seek to {at_ms} ms");
        // The pre-roll makes the first samples after the seek match a decode from the start.
        let s = snr(&heard[..9600], &straight[from..from + 9600]);
        assert!(s > 50.0, "seek to {at_ms} ms: SNR {s} dB against a straight decode");
    }
}

#[test]
fn opus_loops_seamlessly() {
    let file = opus_file(1.0);
    let once = play_48k(file.clone(), Duration::ZERO, false, 100);
    let heard = play_48k(file, Duration::ZERO, true, 40);
    let pass = once.len();
    assert!(heard.len() > pass + 9600);
    // Both sides of the loop point decode as in a single pass.
    let s = snr(&heard[pass - 9600..pass], &once[pass - 9600..]);
    assert!(s > 60.0, "end of the first pass: SNR {s} dB");
    let s = snr(&heard[pass..pass + 9600], &once[..9600]);
    assert!(s > 60.0, "start of the second pass: SNR {s} dB");
}
