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
fn decode_rejects_unsupported_formats() {
    let params = praxi::VideoParams { width: 4, height: 4, format: TextureFormat::Bc3, color_flags: 0 };
    assert!(praxi_encoder::decode_frame(&params, &[0; 16]).is_err());
    let params = praxi::VideoParams { format: TextureFormat::Bc7, ..params };
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
