use praxi::format::{COLOR_FLAG_SRGB, StreamInfo, TimeBase, VideoParams, Writer};
use praxi::{Capabilities, Error, Event, Player, TextureFormat};
use std::io::Cursor;
use std::time::Duration;

const MS: Duration = Duration::from_millis(1);

/// Expected BC1 frame `n`: two 8-byte blocks.
fn bc1_frame(n: u8) -> Vec<u8> {
    let (a, b) = match n {
        0 => (0, 10),
        1 => (1, 10),
        2 => (1, 12),
        3 => (3, 13),
        _ => (4, 14),
    };
    [vec![a; 8], vec![b; 8]].concat()
}

fn astc_frame(n: u8) -> Vec<u8> {
    vec![200 + n; 32]
}

/// 8x4 video at 10 fps, 5 frames, group 0 in BC1 (keyframes 0 and 3) and ASTC 4x4 (all
/// keyframes), plus markers "a" at 150 ms and "b" at 420 ms.
fn sample_file() -> Vec<u8> {
    let params = |format| VideoParams { width: 8, height: 4, format, color_flags: COLOR_FLAG_SRGB };
    let tb = TimeBase::new(1, 10);
    let streams = vec![
        StreamInfo::video(0, tb, params(TextureFormat::Bc1)),
        StreamInfo::video(0, tb, params(TextureFormat::Astc4x4)),
        StreamInfo::markers(1),
    ];
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    let bc1_payloads: [(bool, Vec<u8>); 5] = [
        (true, bc1_frame(0)),
        (false, [vec![0b01], vec![1; 8]].concat()),
        (false, [vec![0b10], vec![12; 8]].concat()),
        (true, bc1_frame(3)),
        (false, [vec![0b11], vec![4; 8], vec![14; 8]].concat()),
    ];
    for (n, (key, payload)) in bc1_payloads.iter().enumerate() {
        let pts = n as u64;
        if n == 2 {
            w.write_chunk(2, 150_000, true, b"a", None).unwrap();
        }
        w.write_chunk(0, pts, *key, payload, None).unwrap();
        w.write_chunk(1, pts, true, &astc_frame(n as u8), None).unwrap();
        if n == 4 {
            w.write_chunk(2, 420_000, true, b"b", None).unwrap();
        }
    }
    w.finish(500_000).unwrap().into_inner()
}

fn open(formats: &[TextureFormat]) -> Player<Cursor<Vec<u8>>> {
    Player::open(Cursor::new(sample_file()), &Capabilities::new(formats.iter().copied())).unwrap()
}

fn events<R: std::io::Read + std::io::Seek>(p: &mut Player<R>) -> Vec<Event> {
    std::iter::from_fn(|| p.poll_event()).collect()
}

fn marker(name: &str, ms: u64) -> Event {
    Event::Marker { name: name.into(), time: ms * MS }
}

#[test]
fn selects_variant_by_preference() {
    let p = open(&[TextureFormat::Astc4x4, TextureFormat::Bc1]);
    assert_eq!(p.tracks().len(), 1);
    assert_eq!(p.tracks()[0].params.format, TextureFormat::Astc4x4);
    let p = open(&[TextureFormat::Bc7, TextureFormat::Bc1]);
    assert_eq!(p.tracks()[0].params.format, TextureFormat::Bc1);
    assert_eq!(p.tracks()[0].stream, 0);

    let err = Player::open(Cursor::new(sample_file()), &Capabilities::new([TextureFormat::Bc7]));
    assert!(matches!(err, Err(Error::NoSupportedVariant { group: 0 })));
}

#[test]
fn plays_frames_and_emits_events() {
    let mut p = open(&[TextureFormat::Bc1]);
    assert!(p.video_frame(0).is_none());
    p.advance(100 * MS).unwrap();
    assert!(p.video_frame(0).is_none(), "stopped players ignore advance");

    p.play();
    p.advance(Duration::ZERO).unwrap();
    assert_eq!(events(&mut p), [Event::Started]);
    let f = p.video_frame(0).unwrap();
    assert_eq!((f.format, f.srgb, f.width, f.height), (TextureFormat::Bc1, true, 8, 4));
    assert_eq!(f.data, bc1_frame(0));
    let first_index = f.index;

    p.advance(50 * MS).unwrap();
    assert_eq!(p.video_frame(0).unwrap().index, first_index, "no new frame before 100 ms");

    let mut seen = vec![];
    for step in 1..=4u8 {
        p.advance(50 * MS).unwrap();
        let f = p.video_frame(0).unwrap();
        assert_eq!(f.data, bc1_frame(step), "frame {step}");
        assert_eq!(f.time, step as u32 * 100 * MS);
        seen.extend(events(&mut p));
        p.advance(50 * MS).unwrap();
        seen.extend(events(&mut p));
    }
    assert_eq!(seen, [marker("a", 150), marker("b", 420)]);
    assert!(!p.is_finished());

    p.advance(50 * MS).unwrap();
    assert_eq!(events(&mut p), [Event::Finished]);
    assert!(p.is_finished());
    assert_eq!(p.position(), 500 * MS);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(4), "last frame stays visible");

    p.play();
    p.advance(Duration::ZERO).unwrap();
    assert_eq!(events(&mut p), [Event::Started]);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(0), "play after finish restarts");
}

#[test]
fn pause_and_resume() {
    let mut p = open(&[TextureFormat::Bc1]);
    p.play();
    p.advance(120 * MS).unwrap();
    p.pause();
    p.advance(Duration::from_secs(5)).unwrap();
    assert_eq!(p.position(), 120 * MS);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(1));
    p.play();
    assert!(p.is_playing());
    assert_eq!(events(&mut p), [Event::Started, Event::Paused, Event::Resumed]);
}

#[test]
fn loops_with_carry_over() {
    let mut p = open(&[TextureFormat::Bc1]);
    p.set_looping(true);
    p.play();
    p.advance(480 * MS).unwrap();
    assert_eq!(events(&mut p), [Event::Started, marker("a", 150), marker("b", 420)]);
    p.advance(140 * MS).unwrap();
    assert_eq!(events(&mut p), [Event::Looped]);
    assert_eq!(p.position(), 120 * MS);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(1));
    assert!(p.is_playing());
}

#[test]
fn seeks_from_preceding_keyframe() {
    let mut p = open(&[TextureFormat::Bc1]);
    p.seek(350 * MS).unwrap();
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(3));
    p.seek(250 * MS).unwrap();
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(2), "deltas replayed from keyframe 0");
    assert_eq!(events(&mut p), [Event::Seeked { to: 350 * MS }, Event::Seeked { to: 250 * MS }]);

    p.play();
    p.advance(200 * MS).unwrap();
    assert_eq!(events(&mut p), [Event::Started, marker("b", 420)], "marker a lies before the seek target");
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(4));

    let mut p = open(&[TextureFormat::Astc4x4]);
    p.seek(Duration::from_secs(9)).unwrap();
    assert_eq!(p.position(), 500 * MS, "seek clamps to the duration");
    assert_eq!(p.video_frame(0).unwrap().data, astc_frame(4));
}
