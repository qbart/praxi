use praxi_format::{
    AudioParams, COLOR_FLAG_SRGB, Compression, Cue, CueParams, Error, Reader, SampleFormat,
    StreamInfo, StreamParams, TextureFormat, TimeBase, Value, VideoParams, Writer,
};
use std::io::Cursor;

fn video(format: TextureFormat) -> StreamInfo {
    StreamInfo::video(
        0,
        TimeBase::new(1, 30),
        VideoParams { width: 10, height: 6, format, color_flags: COLOR_FLAG_SRGB },
    )
}

fn sample_file() -> Vec<u8> {
    let streams = vec![video(TextureFormat::Bc7), video(TextureFormat::Astc6x6), StreamInfo::cues("gameplay")];
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    let compressible = vec![7u8; 96];
    let noise: Vec<u8> = (0..32u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    w.write_cue(2, 0, &Cue::new("start")).unwrap();
    w.write_chunk(0, 0, true, &compressible, None).unwrap();
    w.write_chunk(1, 0, true, &noise, None).unwrap();
    w.write_chunk(0, 1, false, &noise, Some(Compression::Lz4)).unwrap();
    w.write_chunk(1, 1, false, &compressible, Some(Compression::None)).unwrap();
    w.write_cue(2, 50_000, &sample_cue()).unwrap();
    w.write_chunk(0, 2, true, &compressible, None).unwrap();
    w.finish(100_000).unwrap().into_inner()
}

fn sample_cue() -> Cue {
    Cue {
        duration: 40_000,
        kind: "camera.shake".into(),
        fields: vec![
            ("label".into(), "Olá\nmundo".into()),
            ("offset".into(), Value::Int(-3)),
            ("strength".into(), Value::Float(1.5)),
            ("repeat".into(), Value::Bool(true)),
            ("blob".into(), Value::Bytes(vec![0, 255])),
        ],
    }
}

#[test]
fn roundtrip() {
    let mut r = Reader::new(Cursor::new(sample_file())).unwrap();
    assert_eq!(r.header().duration_us, 100_000);
    assert_eq!(r.streams().len(), 3);
    assert_eq!(r.streams()[0].chunk_count, 3);
    assert_eq!(r.streams()[1].chunk_count, 2);
    assert_eq!(r.streams()[2].chunk_count, 2);
    assert_eq!(
        r.streams()[2].params,
        StreamParams::Cues(CueParams { max_duration: 40_000, name: "gameplay".into() })
    );
    let v = r.streams()[1].video_params().unwrap();
    assert_eq!((v.width, v.height, v.format, v.srgb(), v.alpha()), (10, 6, TextureFormat::Astc6x6, true, false));
    assert_eq!((v.blocks_x(), v.blocks_y(), v.frame_size()), (2, 1, 32));

    let mut chunks = Vec::new();
    let mut payload = Vec::new();
    while let Some(c) = r.next_chunk().unwrap() {
        r.read_payload(&c, &mut payload).unwrap();
        chunks.push((c, payload.clone()));
    }
    let order: Vec<(u16, u64)> = chunks.iter().map(|(c, _)| (c.stream, c.pts)).collect();
    assert_eq!(order, [(2, 0), (0, 0), (1, 0), (0, 1), (1, 1), (2, 50_000), (0, 2)]);
    assert_eq!(Cue::decode(&chunks[0].1).unwrap(), Cue::new("start"));
    let sub = Cue::decode(&chunks[5].1).unwrap();
    assert_eq!(sub, sample_cue());
    assert_eq!(sub.get_str("label"), Some("Olá\nmundo"));
    assert_eq!(sub.get("offset"), Some(&Value::Int(-3)));
    assert_eq!(chunks[1].1, vec![7u8; 96]);
    assert_eq!(chunks[1].0.compression, Compression::Lz4, "auto picks LZ4 when smaller");
    assert_eq!(chunks[2].0.compression, Compression::None, "auto stores incompressible data");
    assert_eq!(chunks[3].0.compression, Compression::Lz4, "forced LZ4");
    assert_eq!(chunks[4].0.compression, Compression::None, "forced none");
    assert_eq!(chunks[4].1, vec![7u8; 96]);
    assert!(chunks[1].0.keyframe() && !chunks[3].0.keyframe());

    let index = r.read_index().unwrap();
    let keyframes: Vec<(u16, u64)> = index.iter().map(|e| (e.stream, e.pts)).collect();
    assert_eq!(keyframes, [(2, 0), (0, 0), (1, 0), (2, 50_000), (0, 2)], "video keyframes and cues");
    r.seek_to(index[4].offset).unwrap();
    let c = r.next_chunk().unwrap().unwrap();
    assert_eq!((c.stream, c.pts), (0, 2));
    assert!(r.next_chunk().unwrap().is_none());
}

#[test]
fn rejects_bad_magic_and_version() {
    let mut file = sample_file();
    file[0] = 0;
    assert!(matches!(Reader::new(Cursor::new(file)), Err(Error::BadMagic)));

    let mut file = sample_file();
    file[8] = 1;
    assert!(matches!(Reader::new(Cursor::new(file)), Err(Error::UnsupportedVersion { major: 1, minor: 6 })));

    let mut file = sample_file();
    file[10] = 1;
    assert!(matches!(Reader::new(Cursor::new(file)), Err(Error::UnsupportedVersion { major: 0, minor: 1 })));
}

#[test]
fn rejects_corrupt_lz4() {
    let mut file = sample_file();
    let mut r = Reader::new(Cursor::new(file.clone())).unwrap();
    let mut target = None;
    while let Some(c) = r.next_chunk().unwrap() {
        if c.compression == Compression::Lz4 {
            target = Some(c);
            break;
        }
    }
    let c = target.unwrap();
    let at = (c.offset + praxi_format::CHUNK_HEADER_SIZE) as usize;
    file[at..at + c.stored_size as usize].fill(0xFF);
    let mut r = Reader::new(Cursor::new(file)).unwrap();
    r.seek_to(c.offset).unwrap();
    let c = r.next_chunk().unwrap().unwrap();
    assert!(matches!(r.read_payload(&c, &mut Vec::new()), Err(Error::Corrupt(_))));
}

#[test]
fn writer_enforces_order() {
    let streams = vec![video(TextureFormat::Bc7), StreamInfo::cues("cues")];
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    w.write_chunk(0, 3, true, &[0; 16], None).unwrap();
    assert!(matches!(w.write_chunk(0, 3, false, &[0; 16], None), Err(Error::Invalid(_))));
    assert!(matches!(w.write_cue(1, 10, &Cue::new("early")), Err(Error::Invalid(_))));
    assert!(matches!(w.write_chunk(5, 10, true, b"x", None), Err(Error::Invalid(_))));
    assert!(matches!(w.write_chunk(1, 200_000, true, b"x", None), Err(Error::Invalid(_))), "cue streams take write_cue");
    assert!(matches!(w.write_cue(0, 9, &Cue::new("x")), Err(Error::Invalid(_))), "video streams take write_chunk");
    w.write_cue(1, 200_000, &Cue::new("a")).unwrap();
    w.write_cue(1, 200_000, &Cue::new("b")).unwrap();
    let dup = Cue { fields: vec![("k".into(), Value::Int(1)), ("k".into(), Value::Int(2))], ..Cue::new("c") };
    assert!(matches!(w.write_cue(1, 300_000, &dup), Err(Error::Invalid(_))));
    assert!(matches!(w.write_cue(1, 300_000, &Cue::new("")), Err(Error::Invalid(_))));

    let long_name = StreamInfo::cues("x".repeat(33));
    assert!(matches!(Writer::new(Cursor::new(Vec::new()), vec![long_name]), Err(Error::Invalid(_))));
}

#[test]
fn rejects_bad_cue_payloads() {
    let mut good = Vec::new();
    sample_cue().encode(&mut good).unwrap();
    for cut in 0..good.len() {
        assert!(Cue::decode(&good[..cut]).is_err(), "truncated at {cut}");
    }
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(Cue::decode(&trailing).is_err());
    let mut bad_type = Vec::new();
    Cue { fields: vec![("k".into(), Value::Bool(true))], ..Cue::new("t") }.encode(&mut bad_type).unwrap();
    let ty_at = bad_type.len() - 2;
    bad_type[ty_at] = 9;
    assert!(Cue::decode(&bad_type).is_err());
}

fn audio(name: &str) -> StreamInfo {
    StreamInfo::audio(
        3,
        AudioParams {
            sample_rate: 1000,
            channels: 2,
            format: SampleFormat::S16,
            channel_mask: 0x3,
            preload_us: 200_000,
            name: name.into(),
        },
    )
}

#[test]
fn audio_roundtrip_and_decode_time_order() {
    let streams = vec![video(TextureFormat::Bc7), audio("music")];
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    let samples = |frames: usize| vec![1u8; frames * 4];
    w.write_audio(1, 0, &samples(100)).unwrap();
    w.write_audio(1, 100, &samples(100)).unwrap();
    w.write_audio(1, 200, &samples(100)).unwrap();
    w.write_chunk(0, 0, true, &[0; 32], None).unwrap();
    // Due at 300 - 200 = 100 ms, before the video frame at 133 ms.
    w.write_audio(1, 300, &samples(100)).unwrap();
    w.write_chunk(0, 4, true, &[0; 32], None).unwrap();
    assert!(matches!(w.write_audio(1, 350, &samples(10)), Err(Error::Invalid(_))), "overlap");
    assert!(matches!(w.write_audio(1, 400, &samples(10)[..6]), Err(Error::Invalid(_))), "partial frame");
    assert!(matches!(w.write_audio(1, 400, &[]), Err(Error::Invalid(_))), "empty");
    assert!(matches!(w.write_chunk(1, 400, true, &samples(10), None), Err(Error::Invalid(_))));
    assert!(matches!(w.write_audio(0, 400, &samples(10)), Err(Error::Invalid(_))));
    // A gap is allowed; due at 600 - 200 = 400 ms.
    w.write_audio(1, 600, &samples(50)).unwrap();
    assert!(matches!(w.write_chunk(0, 10, true, &[0; 32], None), Err(Error::Invalid(_))), "frame at 333 ms after 400 ms");
    let file = w.finish(650_000).unwrap().into_inner();

    let mut r = Reader::new(Cursor::new(file)).unwrap();
    assert_eq!(r.streams()[1], {
        let mut s = audio("music");
        s.chunk_count = 5;
        s
    });
    assert_eq!(r.streams()[1].time_base, TimeBase::new(1, 1000));
    assert_eq!(r.streams()[1].decode_time_us(300), 100_000);
    assert_eq!(r.streams()[1].decode_time_us(100), 0);
    assert_eq!(r.streams()[0].decode_time_us(4), 133_333);
    let index = r.read_index().unwrap();
    assert_eq!(index.iter().filter(|e| e.stream == 1).map(|e| e.pts).collect::<Vec<_>>(), [0, 100, 200, 300, 600]);
    let mut payload = Vec::new();
    let mut order = Vec::new();
    while let Some(c) = r.next_chunk().unwrap() {
        r.read_payload(&c, &mut payload).unwrap();
        if c.stream == 1 {
            assert!(c.keyframe());
            assert_eq!(payload, samples(c.raw_size as usize / 4));
        }
        order.push((c.stream, c.pts));
    }
    assert_eq!(order, [(1, 0), (1, 100), (1, 200), (0, 0), (1, 300), (0, 4), (1, 600)]);
}

#[test]
fn rejects_bad_audio_streams() {
    let bad = |f: fn(&mut StreamInfo)| {
        let mut s = audio("music");
        f(&mut s);
        matches!(Writer::new(Cursor::new(Vec::new()), vec![s]), Err(Error::Invalid(_)))
    };
    assert!(bad(|s| s.time_base = TimeBase::new(1, 48_000)));
    assert!(bad(|s| if let StreamParams::Audio(a) = &mut s.params { a.channel_mask = 0x7 }));
    assert!(bad(|s| if let StreamParams::Audio(a) = &mut s.params { a.name = "x".repeat(33) }));
    assert!(bad(|s| if let StreamParams::Audio(a) = &mut s.params { a.channels = 0 }));

    // Unknown sample format in the file.
    let mut w = Writer::new(Cursor::new(Vec::new()), vec![audio("music")]).unwrap();
    w.write_audio(0, 0, &[0; 8]).unwrap();
    let mut file = w.finish(2_000).unwrap().into_inner();
    file[48 + 22] = 9;
    assert!(matches!(Reader::new(Cursor::new(file)), Err(Error::Corrupt(_))));
}

/// BC7 blocks whose bytes 0, 6 and 7 are the same everywhere and whose other bytes are
/// noise, like real encoder output.
fn structured_blocks(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761) | 1;
    let mut out = Vec::new();
    for _ in 0..n {
        for k in 0..16 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            out.push(match k {
                0 => 0x40,
                6 | 7 => 0,
                _ => x as u8,
            });
        }
    }
    out
}

fn chunk_offsets(file: &[u8]) -> Vec<praxi_format::ChunkHeader> {
    let mut r = Reader::new(Cursor::new(file.to_vec())).unwrap();
    let mut chunks = Vec::new();
    while let Some(c) = r.next_chunk().unwrap() {
        chunks.push(c);
    }
    chunks
}

#[test]
fn byte_planes() {
    // 64x32 BC7: 16x8 = 128 blocks, mask of 16 bytes.
    let params = VideoParams { width: 64, height: 32, format: TextureFormat::Bc7, color_flags: COLOR_FLAG_SRGB };
    let streams = vec![StreamInfo::video(0, TimeBase::new(1, 30), params), StreamInfo::cues("c")];
    let key = structured_blocks(128, 1);
    let mut delta = vec![0u8; 16];
    delta[0] = 0b1000_0101;
    delta[15] = 0x80;
    delta.extend(structured_blocks(4, 2));
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    w.write_cue(1, 0, &Cue::new("start")).unwrap();
    w.write_chunk(0, 0, true, &key, None).unwrap();
    w.write_chunk(0, 1, false, &delta, Some(Compression::Planes)).unwrap();
    let err = w.write_chunk(0, 2, false, &delta[..30], Some(Compression::Planes));
    assert!(matches!(err, Err(Error::Invalid(_))), "not whole blocks after the mask: {err:?}");
    let file = w.finish(100_000).unwrap().into_inner();

    // The saving threshold: planes save about 13% here (three constant planes as LZ4, minus
    // 85 bytes of section headers).
    let auto_compression = |percent: f64| {
        let streams = vec![StreamInfo::video(0, TimeBase::new(1, 30), params)];
        let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
        w.set_plane_min_saving(percent);
        w.write_chunk(0, 0, true, &key, None).unwrap();
        let file = w.finish(100_000).unwrap().into_inner();
        let mut r = Reader::new(Cursor::new(file)).unwrap();
        r.next_chunk().unwrap().unwrap().compression
    };
    assert_eq!(auto_compression(0.0), Compression::Planes);
    assert_eq!(auto_compression(10.0), Compression::Planes);
    assert_eq!(auto_compression(15.0), Compression::None, "saving below the threshold");
    assert_eq!(auto_compression(100.0), Compression::None, "never byte planes");

    let mut r = Reader::new(Cursor::new(file.clone())).unwrap();
    let mut payload = Vec::new();
    r.next_chunk().unwrap().unwrap();
    let c = r.next_chunk().unwrap().unwrap();
    assert_eq!(c.compression, Compression::Planes, "auto picks byte planes for structured blocks");
    assert!(c.stored_size < c.raw_size);
    r.read_payload(&c, &mut payload).unwrap();
    assert_eq!(payload, key);
    let c = r.next_chunk().unwrap().unwrap();
    assert_eq!(c.compression, Compression::Planes, "forced");
    r.read_payload(&c, &mut payload).unwrap();
    assert_eq!(payload, delta, "the mask stays in front");

    let chunks = chunk_offsets(&file);
    // Byte planes in a cue stream.
    let mut bad = file.clone();
    bad[chunks[0].offset as usize + 3] = 2;
    assert!(matches!(chunk_offsets_result(&bad), Err(Error::Corrupt(_))));
    // A raw size that is not the mask plus whole blocks.
    let mut bad = file.clone();
    let at = chunks[2].offset as usize + 8;
    bad[at..at + 4].copy_from_slice(&(delta.len() as u32 + 1).to_le_bytes());
    assert!(matches!(chunk_offsets_result(&bad), Err(Error::Corrupt(_))));
}

fn chunk_offsets_result(file: &[u8]) -> Result<(), Error> {
    let mut r = Reader::new(Cursor::new(file.to_vec()))?;
    while r.next_chunk()?.is_some() {}
    Ok(())
}
