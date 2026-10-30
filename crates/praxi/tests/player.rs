use praxi::format::{COLOR_FLAG_SRGB, Cue, StreamInfo, TimeBase, Value, VideoParams, Writer};
use praxi::{Capabilities, CueStreamInfo, Error, Event, Player, TextureFormat};
use std::collections::HashMap;
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

fn subtitle(text: &str, ms: u64) -> Cue {
    Cue { duration: ms * 1000, kind: "subtitle".into(), fields: vec![("text".into(), Value::from(text))] }
}

/// 8x4 video at 10 fps, 5 frames, 500 ms. Group 0 in BC1 (keyframes 0 and 3) and ASTC 4x4
/// (all keyframes). Cue stream 2 "cues": `a` at 150 ms, `b` at 420 ms, `long` at 450 ms
/// lasting past the end. Cue stream 3 "subtitles" (en): `hello` 120-320 ms, `world` 250-350 ms.
fn sample_file() -> Vec<u8> {
    let params = |format| VideoParams { width: 8, height: 4, format, color_flags: COLOR_FLAG_SRGB };
    let tb = TimeBase::new(1, 10);
    let streams = vec![
        StreamInfo::video(0, tb, params(TextureFormat::Bc1)),
        StreamInfo::video(0, tb, params(TextureFormat::Astc4x4)),
        StreamInfo::cues("cues", ""),
        StreamInfo::cues("subtitles", "en"),
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
        match n {
            2 => {
                w.write_cue(3, 120_000, &subtitle("hello", 200)).unwrap();
                w.write_cue(2, 150_000, &Cue::new("a")).unwrap();
            }
            3 => w.write_cue(3, 250_000, &subtitle("world", 100)).unwrap(),
            _ => {}
        }
        let pts = n as u64;
        w.write_chunk(0, pts, *key, payload, None).unwrap();
        w.write_chunk(1, pts, true, &astc_frame(n as u8), None).unwrap();
    }
    w.write_cue(2, 420_000, &Cue::new("b")).unwrap();
    w.write_cue(2, 450_000, &Cue { duration: 100_000, ..Cue::new("long") }).unwrap();
    w.finish(500_000).unwrap().into_inner()
}

fn open(formats: &[TextureFormat]) -> Player<Cursor<Vec<u8>>> {
    Player::open(Cursor::new(sample_file()), &Capabilities::new(formats.iter().copied())).unwrap()
}

/// Turns events into short strings; cue ends are named after the cue they end.
#[derive(Default)]
struct Log {
    names: HashMap<u64, String>,
}

impl Log {
    fn take<R: std::io::Read + std::io::Seek>(&mut self, p: &mut Player<R>) -> Vec<String> {
        std::iter::from_fn(|| p.poll_event())
            .map(|e| match e {
                Event::Started => "started".into(),
                Event::Paused => "paused".into(),
                Event::Resumed => "resumed".into(),
                Event::Seeked { to } => format!("seeked@{}", to.as_millis()),
                Event::Looped => "looped".into(),
                Event::Finished => "finished".into(),
                Event::Cue(cue) => {
                    let name = cue.get_str("text").unwrap_or(&cue.kind).to_string();
                    self.names.insert(cue.id, name.clone());
                    format!("cue {name}@{}", cue.time.as_millis())
                }
                Event::CueEnd { id, .. } => format!("end {}", self.names[&id]),
            })
            .collect()
    }
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
    let mut log = Log::default();
    let mut p = open(&[TextureFormat::Bc1]);
    assert!(p.video_frame(0).is_none());
    p.advance(100 * MS).unwrap();
    assert!(p.video_frame(0).is_none(), "stopped players ignore advance");

    p.play();
    p.advance(Duration::ZERO).unwrap();
    assert_eq!(log.take(&mut p), ["started"]);
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
        seen.extend(log.take(&mut p));
        p.advance(50 * MS).unwrap();
        seen.extend(log.take(&mut p));
    }
    assert_eq!(
        seen,
        ["cue hello@120", "cue a@150", "cue world@250", "end hello", "end world", "cue b@420", "cue long@450"]
    );
    assert!(!p.is_finished());

    p.advance(50 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["end long", "finished"], "finishing ends active cues");
    assert!(p.is_finished());
    assert_eq!(p.position(), 500 * MS);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(4), "last frame stays visible");

    p.play();
    p.advance(Duration::ZERO).unwrap();
    assert_eq!(log.take(&mut p), ["started"]);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(0), "play after finish restarts");
}

#[test]
fn cue_event_carries_fields() {
    let mut p = open(&[TextureFormat::Bc1]);
    p.play();
    p.advance(130 * MS).unwrap();
    let cue = std::iter::from_fn(|| p.poll_event())
        .find_map(|e| match e {
            Event::Cue(c) => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!((cue.kind.as_str(), cue.stream, cue.time, cue.duration), ("subtitle", 3, 120 * MS, 200 * MS));
    assert_eq!(cue.get("text"), Some(&Value::from("hello")));
}

#[test]
fn pause_and_resume() {
    let mut log = Log::default();
    let mut p = open(&[TextureFormat::Bc1]);
    p.play();
    p.advance(100 * MS).unwrap();
    p.pause();
    p.advance(Duration::from_secs(5)).unwrap();
    assert_eq!(p.position(), 100 * MS);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(1));
    p.play();
    assert!(p.is_playing());
    assert_eq!(log.take(&mut p), ["started", "paused", "resumed"]);
}

#[test]
fn loops_with_carry_over() {
    let mut log = Log::default();
    let mut p = open(&[TextureFormat::Bc1]);
    p.set_looping(true);
    p.play();
    p.advance(480 * MS).unwrap();
    assert_eq!(
        log.take(&mut p),
        ["started", "cue hello@120", "cue a@150", "cue world@250", "end hello", "end world", "cue b@420", "cue long@450"]
    );
    p.advance(140 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["end long", "looped", "cue hello@120"]);
    assert_eq!(p.position(), 120 * MS);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(1));
    assert!(p.is_playing());
}

#[test]
fn seeks_from_preceding_keyframe() {
    let mut log = Log::default();
    let mut p = open(&[TextureFormat::Bc1]);
    p.seek(350 * MS).unwrap();
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(3));
    assert_eq!(log.take(&mut p), ["seeked@350"], "no cue is active at 350 ms");

    p.seek(300 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["seeked@300", "cue hello@120", "cue world@250"], "active cues restart");
    p.seek(250 * MS).unwrap();
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(2), "deltas replayed from keyframe 0");
    assert_eq!(log.take(&mut p), ["end hello", "end world", "seeked@250", "cue hello@120", "cue world@250"]);

    p.play();
    p.advance(200 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["started", "end hello", "end world", "cue b@420", "cue long@450"]);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(4));

    let mut p = open(&[TextureFormat::Astc4x4]);
    p.seek(Duration::from_secs(9)).unwrap();
    assert_eq!(p.position(), 500 * MS, "seek clamps to the duration");
    assert_eq!(p.video_frame(0).unwrap().data, astc_frame(4));
    assert_eq!(log.take(&mut p), ["seeked@500", "cue long@450"]);
}

#[test]
fn cue_streams_can_be_disabled() {
    let mut log = Log::default();
    let mut p = open(&[TextureFormat::Bc1]);
    assert_eq!(
        p.cue_streams(),
        [
            CueStreamInfo { stream: 2, name: "cues".into(), language: "".into() },
            CueStreamInfo { stream: 3, name: "subtitles".into(), language: "en".into() },
        ]
    );
    p.play();
    p.advance(130 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["started", "cue hello@120"]);
    p.set_cue_stream_enabled(3, false);
    assert_eq!(log.take(&mut p), ["end hello"], "disabling ends active cues");
    p.set_cue_stream_enabled(0, false);
    p.advance(300 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["cue a@150", "cue b@420"]);
}
