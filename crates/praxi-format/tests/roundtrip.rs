use praxi_format::{
    COLOR_FLAG_SRGB, Compression, Cue, CueParams, Error, Reader, StreamInfo, StreamParams,
    TextureFormat, TimeBase, Value, VideoParams, Writer,
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
    assert!(matches!(Reader::new(Cursor::new(file)), Err(Error::UnsupportedVersion { major: 1, minor: 3 })));

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
