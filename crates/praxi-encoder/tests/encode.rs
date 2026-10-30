use praxi::format::{Cue, Reader, StreamParams};
use praxi::{Capabilities, Event, Player, TextureFormat};
use praxi_encoder::{CueConfig, CueStreamConfig, Encoder, EncoderConfig, Stats, Value, VariantConfig};
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
    p.play();
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
    let subtitles = CueStreamConfig::new("subtitles")
        .language("en")
        .cue(CueConfig::new(0.2, "subtitle").duration(0.15).field("text", "Hi").field("line", 2i64));
    let config = config()
        .keyint(4)
        .skip_threshold(0)
        .cue_with(CueConfig::new(1.0, "end").duration(0.5))
        .cue(0.05, "m1")
        .cue_stream(subtitles);
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
    let cue_names: Vec<(&str, &str, u64)> = streams[2..]
        .iter()
        .map(|s| match &s.params {
            StreamParams::Cues(c) => (c.name.as_str(), c.language.as_str(), c.max_duration),
            p => panic!("not a cue stream: {p:?}"),
        })
        .collect();
    assert_eq!(cue_names, [("cues", "", 500_000), ("subtitles", "en", 150_000)]);

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
    let subtitle = Cue {
        duration: 150_000,
        kind: "subtitle".into(),
        fields: vec![("text".into(), Value::from("Hi")), ("line".into(), Value::Int(2))],
    };
    assert_eq!(
        cues,
        [
            (2, 50_000, Cue::new("m1")),
            (3, 200_000, subtitle),
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
    ];
    for config in bad {
        assert!(Encoder::new(Cursor::new(Vec::new()), config).is_err());
    }
}
