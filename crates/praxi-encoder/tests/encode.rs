use praxi::format::Reader;
use praxi::{Capabilities, Event, Player, TextureFormat};
use praxi_encoder::{Encoder, EncoderConfig, Stats, VariantConfig};
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
fn writes_streams_markers_and_deltas() {
    let (file, stats) = encode(config().keyint(4).skip_threshold(0).marker(0.05, "m1").marker(1.0, "end"));
    assert_eq!(stats.frames, 6);
    assert_eq!(stats.duration_us, 1_000_000, "raised to the last marker");

    let mut r = Reader::new(Cursor::new(file)).unwrap();
    let streams = r.streams().to_vec();
    assert_eq!(streams.len(), 3);
    let bc7 = streams[0].video_params().unwrap();
    assert_eq!((bc7.format, bc7.width, bc7.height, bc7.block_count()), (TextureFormat::Bc7, W, H, 5 * 3));
    assert!(bc7.srgb() && !bc7.alpha());
    assert_eq!(streams[1].video_params().unwrap().format, TextureFormat::Astc6x6);
    assert_eq!([streams[0].chunk_count, streams[1].chunk_count, streams[2].chunk_count], [6, 6, 2]);

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

    let markers: Vec<(u64, &[u8])> = chunks.iter().filter(|(c, _)| c.stream == 2).map(|(c, p)| (c.pts, p.as_slice())).collect();
    assert_eq!(markers, [(50_000, b"m1".as_slice()), (1_000_000, b"end".as_slice())]);
    assert!(stats.variants[0].skipped_blocks > 0);
}

#[test]
fn player_reconstructs_keyframe_encoding() {
    for format in [TextureFormat::Bc7, TextureFormat::Astc6x6] {
        let (deltas, events) = play_all(encode(config().keyint(4).skip_threshold(0).marker(0.25, "m")).0, format);
        let (keys, _) = play_all(encode(config().keyint(1)).0, format);
        assert_eq!(deltas.len(), 6);
        assert_eq!(deltas, keys, "{format}: delta reconstruction matches all-keyframe encoding");
        assert!(events.contains(&Event::Marker { name: "m".into(), time: Duration::from_millis(250) }));
    }
}

#[test]
fn rejects_bad_config() {
    let bad = [
        EncoderConfig::new(0, 4, 30, 1).variant(VariantConfig::bc7()),
        EncoderConfig::new(4, 4, 30, 1),
        EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::astc(5)),
        EncoderConfig::new(4, 4, 30, 1).variant(VariantConfig::bc7()).marker(1.0, "a").marker(1.0, "b"),
    ];
    for config in bad {
        assert!(Encoder::new(Cursor::new(Vec::new()), config).is_err());
    }
}
