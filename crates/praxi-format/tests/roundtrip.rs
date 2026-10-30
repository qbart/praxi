use praxi_format::{
    COLOR_FLAG_SRGB, Compression, Error, Reader, StreamInfo, StreamParams, TextureFormat, TimeBase,
    VideoParams, Writer,
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
    let streams = vec![video(TextureFormat::Bc7), video(TextureFormat::Astc6x6), StreamInfo::markers(1)];
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    let compressible = vec![7u8; 96];
    let noise: Vec<u8> = (0..32u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    w.write_chunk(2, 0, true, b"start", None).unwrap();
    w.write_chunk(0, 0, true, &compressible, None).unwrap();
    w.write_chunk(1, 0, true, &noise, None).unwrap();
    w.write_chunk(0, 1, false, &noise, Some(Compression::Lz4)).unwrap();
    w.write_chunk(1, 1, false, &compressible, Some(Compression::None)).unwrap();
    w.write_chunk(2, 50_000, true, b"middle", None).unwrap();
    w.write_chunk(0, 2, true, &compressible, None).unwrap();
    w.finish(100_000).unwrap().into_inner()
}

#[test]
fn roundtrip() {
    let mut r = Reader::new(Cursor::new(sample_file())).unwrap();
    assert_eq!(r.header().duration_us, 100_000);
    assert_eq!(r.streams().len(), 3);
    assert_eq!(r.streams()[0].chunk_count, 3);
    assert_eq!(r.streams()[1].chunk_count, 2);
    assert_eq!(r.streams()[2].chunk_count, 2);
    assert_eq!(r.streams()[2].params, StreamParams::Markers);
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
    assert_eq!(chunks[0].1, b"start");
    assert_eq!(chunks[1].1, vec![7u8; 96]);
    assert_eq!(chunks[1].0.compression, Compression::Lz4, "auto picks LZ4 when smaller");
    assert_eq!(chunks[2].0.compression, Compression::None, "auto stores incompressible data");
    assert_eq!(chunks[3].0.compression, Compression::Lz4, "forced LZ4");
    assert_eq!(chunks[4].0.compression, Compression::None, "forced none");
    assert_eq!(chunks[4].1, vec![7u8; 96]);
    assert!(chunks[1].0.keyframe() && !chunks[3].0.keyframe());

    let index = r.read_index().unwrap();
    let keyframes: Vec<(u16, u64)> = index.iter().map(|e| (e.stream, e.pts)).collect();
    assert_eq!(keyframes, [(0, 0), (1, 0), (0, 2)], "index holds video keyframes only");
    r.seek_to(index[2].offset).unwrap();
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
    assert!(matches!(Reader::new(Cursor::new(file)), Err(Error::UnsupportedVersion(1))));
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
    let streams = vec![video(TextureFormat::Bc7), StreamInfo::markers(1)];
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    w.write_chunk(0, 3, true, &[0; 16], None).unwrap();
    assert!(matches!(w.write_chunk(0, 3, false, &[0; 16], None), Err(Error::Invalid(_))));
    assert!(matches!(w.write_chunk(1, 10, true, b"early", None), Err(Error::Invalid(_))));
    assert!(matches!(w.write_chunk(5, 10, true, b"x", None), Err(Error::Invalid(_))));
}
