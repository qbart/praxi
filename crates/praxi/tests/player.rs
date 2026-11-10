use praxi::format::{COLOR_FLAG_SRGB, Cue, StreamInfo, TimeBase, Value, VideoParams, Writer};
use praxi::{Capabilities, CueStreamInfo, CueTrack, Error, Event, Player, TextureFormat, TimedCue};
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

fn flash(color: &str, ms: u64) -> Cue {
    Cue { duration: ms * 1000, kind: "light.flash".into(), fields: vec![("color".into(), Value::from(color))] }
}

/// 8x4 video at 10 fps, 5 frames, 500 ms. Group 0 in BC1 (keyframes 0 and 3) and ASTC 4x4
/// (all keyframes). Cue stream 2 "cues": `a` at 150 ms, `b` at 420 ms, `long` at 450 ms
/// lasting past the end. Cue stream 3 "gameplay": flashes `red` 120-320 ms and `blue` 250-350 ms.
fn sample_file() -> Vec<u8> {
    let params = |format| VideoParams::new(8, 4, format, COLOR_FLAG_SRGB);
    let tb = TimeBase::new(1, 10);
    let streams = vec![
        StreamInfo::video(0, tb, params(TextureFormat::Bc1)),
        StreamInfo::video(0, tb, params(TextureFormat::Astc4x4)),
        StreamInfo::cues("cues"),
        StreamInfo::cues("gameplay"),
    ];
    let mut w = Writer::new(Cursor::new(Vec::new()), streams).unwrap();
    // Delta frames carry residuals: new block XOR previous block.
    let bc1_payloads: [(bool, Vec<u8>); 5] = [
        (true, bc1_frame(0)),
        (false, [vec![0b01], vec![1; 8]].concat()), // 0 ^ 1
        (false, [vec![0b10], vec![10 ^ 12; 8]].concat()),
        (true, bc1_frame(3)),
        (false, [vec![0b11], vec![3 ^ 4; 8], vec![13 ^ 14; 8]].concat()),
    ];
    for (n, (key, payload)) in bc1_payloads.iter().enumerate() {
        match n {
            2 => {
                w.write_cue(3, 120_000, &flash("red", 200)).unwrap();
                w.write_cue(2, 150_000, &Cue::new("a")).unwrap();
            }
            3 => w.write_cue(3, 250_000, &flash("blue", 100)).unwrap(),
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
                    let name = cue.get_str("color").unwrap_or(&cue.kind).to_string();
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

    p.play().unwrap();
    assert_eq!(log.take(&mut p), ["started"]);
    let f = p.video_frame(0).expect("play decodes the first frame");
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
        ["cue red@120", "cue a@150", "cue blue@250", "end red", "end blue", "cue b@420", "cue long@450"]
    );
    assert!(!p.is_finished());

    p.advance(50 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["end long", "finished"], "finishing ends active cues");
    assert!(p.is_finished());
    assert_eq!(p.position(), 500 * MS);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(4), "last frame stays visible");

    p.play().unwrap();
    assert_eq!(log.take(&mut p), ["started"]);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(0), "play after finish restarts");
}

#[test]
fn cue_event_carries_fields() {
    let mut p = open(&[TextureFormat::Bc1]);
    p.play().unwrap();
    p.advance(130 * MS).unwrap();
    let cue = std::iter::from_fn(|| p.poll_event())
        .find_map(|e| match e {
            Event::Cue(c) => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!((cue.kind.as_str(), cue.stream, cue.time, cue.duration), ("light.flash", 3, 120 * MS, 200 * MS));
    assert_eq!(cue.get("color"), Some(&Value::from("red")));
}

#[test]
fn pause_and_resume() {
    let mut log = Log::default();
    let mut p = open(&[TextureFormat::Bc1]);
    p.play().unwrap();
    p.advance(100 * MS).unwrap();
    p.pause();
    p.advance(Duration::from_secs(5)).unwrap();
    assert_eq!(p.position(), 100 * MS);
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(1));
    p.play().unwrap();
    assert!(p.is_playing());
    assert_eq!(log.take(&mut p), ["started", "paused", "resumed"]);
}

#[test]
fn loops_with_carry_over() {
    let mut log = Log::default();
    let mut p = open(&[TextureFormat::Bc1]);
    p.set_looping(true);
    p.play().unwrap();
    p.advance(480 * MS).unwrap();
    assert_eq!(
        log.take(&mut p),
        ["started", "cue red@120", "cue a@150", "cue blue@250", "end red", "end blue", "cue b@420", "cue long@450"]
    );
    p.advance(140 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["end long", "looped", "cue red@120"]);
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
    assert_eq!(log.take(&mut p), ["seeked@300", "cue red@120", "cue blue@250"], "active cues restart");
    p.seek(250 * MS).unwrap();
    assert_eq!(p.video_frame(0).unwrap().data, bc1_frame(2), "deltas replayed from keyframe 0");
    assert_eq!(log.take(&mut p), ["end red", "end blue", "seeked@250", "cue red@120", "cue blue@250"]);

    p.play().unwrap();
    p.advance(200 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["started", "end red", "end blue", "cue b@420", "cue long@450"]);
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
            CueStreamInfo { stream: 2, name: "cues".into() },
            CueStreamInfo { stream: 3, name: "gameplay".into() },
        ]
    );
    p.play().unwrap();
    p.advance(130 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["started", "cue red@120"]);
    p.set_cue_stream_enabled(3, false);
    assert_eq!(log.take(&mut p), ["end red"], "disabling ends active cues");
    p.set_cue_stream_enabled(0, false);
    p.advance(300 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["cue a@150", "cue b@420"]);
}

#[test]
fn sub_microsecond_steps_add_up() {
    let mut p = open(&[TextureFormat::Bc1]);
    p.play().unwrap();
    // 144 Hz: 6944.44 us per frame; truncating each step would lose 0.44 us a frame.
    let frame = Duration::from_nanos(6_944_444);
    for _ in 0..36 {
        p.advance(frame).unwrap();
    }
    assert_eq!(p.position(), Duration::from_micros(249_999), "36 frames of 6944.444 us");
    // Steps below a microsecond still move the clock.
    for _ in 0..2000 {
        p.advance(Duration::from_nanos(500)).unwrap();
    }
    assert_eq!(p.position(), Duration::from_micros(250_999));
}

/// Two cue streams for `replace_cues`, unsorted, with cues on chunk times (0, 100, 300 ms),
/// at the same time in both streams (300 ms) and overlapping, all ending by 500 ms.
fn replacement_tracks() -> Vec<CueTrack> {
    let cue = |ms: u64, c: Cue| TimedCue { time_us: ms * 1000, cue: c };
    vec![
        CueTrack {
            name: "subtitles".into(),
            cues: vec![
                cue(300, Cue { fields: vec![("text".into(), Value::from("two"))], ..Cue { duration: 150_000, ..Cue::new("subtitle") } }),
                cue(0, Cue { fields: vec![("text".into(), Value::from("one"))], ..Cue { duration: 250_000, ..Cue::new("subtitle") } }),
            ],
        },
        CueTrack {
            name: "cues".into(),
            cues: vec![cue(100, Cue::new("hit")), cue(300, flash("green", 200)), cue(300, Cue::new("tie")), cue(490, Cue::new("last"))],
        },
    ]
}

/// Plays through a fixed script of transport calls and records every event (cue ids by
/// first appearance, streams by name) and the frame after each call.
fn script_trace(p: &mut Player<Cursor<Vec<u8>>>) -> Vec<String> {
    let mut ids: Vec<u64> = Vec::new();
    let mut trace = Vec::new();
    let mut record = |p: &mut Player<Cursor<Vec<u8>>>, step: &str| {
        let names: HashMap<u16, String> = p.cue_streams().iter().map(|c| (c.stream, c.name.clone())).collect();
        let mut id = |raw: u64| match ids.iter().position(|&i| i == raw) {
            Some(n) => n,
            None => {
                ids.push(raw);
                ids.len() - 1
            }
        };
        while let Some(e) = p.poll_event() {
            trace.push(match e {
                Event::Cue(c) => format!(
                    "{step}: cue #{} {} {}@{} +{} {:?}",
                    id(c.id),
                    names[&c.stream],
                    c.kind,
                    c.time.as_millis(),
                    c.duration.as_millis(),
                    c.fields
                ),
                Event::CueEnd { id: raw, stream } => format!("{step}: end #{} {}", id(raw), names[&stream]),
                other => format!("{step}: {other:?}"),
            });
        }
        trace.push(format!("{step}: frame {:?} at {:?}", p.video_frame(0).map(|f| f.data[0]), p.position()));
    };
    p.play().unwrap();
    record(p, "play");
    for i in 0..8 {
        p.advance(37 * MS).unwrap();
        record(p, &format!("advance {i}"));
    }
    p.seek(320 * MS).unwrap();
    record(p, "seek 320");
    p.set_looping(true);
    for i in 0..10 {
        p.advance(45 * MS).unwrap();
        record(p, &format!("loop {i}"));
    }
    p.set_looping(false);
    p.advance(Duration::from_secs(1)).unwrap();
    record(p, "finish");
    trace
}

#[test]
fn replaced_cues_play_like_an_imported_file() {
    let caps = Capabilities::new([TextureFormat::Bc1]);
    let mut preview = Player::open(Cursor::new(sample_file()), &caps).unwrap();
    preview.replace_cues(replacement_tracks());
    let reader = praxi::format::Reader::new(Cursor::new(sample_file())).unwrap();
    let imported = praxi::format::replace_cues(reader, Cursor::new(Vec::new()), &replacement_tracks()).unwrap();
    let mut imported = Player::open(Cursor::new(imported.into_inner()), &caps).unwrap();
    assert_eq!(imported.duration(), preview.duration());

    let expected = script_trace(&mut imported);
    assert!(expected.iter().any(|l| l.contains("cue #0 subtitles subtitle@0")), "{expected:#?}");
    assert!(expected.iter().any(|l| l.starts_with("loop") && l.contains("Looped")), "{expected:#?}");
    assert_eq!(script_trace(&mut preview), expected);
}

#[test]
fn replace_cues_acts_like_a_seek_for_cues() {
    let mut log = Log::default();
    let mut p = open(&[TextureFormat::Bc1]);
    p.play().unwrap();
    p.advance(260 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["started", "cue red@120", "cue a@150", "cue blue@250"]);
    let frame = p.video_frame(0).unwrap().index;

    let cue = |ms: u64, c: Cue| TimedCue { time_us: ms * 1000, cue: c };
    p.replace_cues(vec![CueTrack {
        name: "edited".into(),
        cues: vec![cue(100, Cue::new("passed")), cue(200, Cue { duration: 200_000, ..Cue::new("held") }), cue(300, Cue::new("later"))],
    }]);
    assert_eq!(log.take(&mut p), ["end red", "end blue", "cue held@200"], "active cues end, active new ones start");
    assert_eq!(p.cue_streams(), [CueStreamInfo { stream: 4, name: "edited".into() }], "numbered after the file's streams");
    assert_eq!(p.video_frame(0).unwrap().index, frame, "video is untouched");
    assert_eq!(p.position(), 260 * MS);

    p.advance(60 * MS).unwrap();
    let ids: Vec<u64> = std::iter::from_fn(|| p.poll_event())
        .filter_map(|e| match e {
            Event::Cue(c) => Some(c.id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, [(1 << 63) + 2], "later is the third cue; ids start at 2^63");
    p.advance(200 * MS).unwrap();
    assert!(log.take(&mut p).iter().all(|e| !e.contains("@4")), "file cues b and long stay replaced");

    p.set_cue_stream_enabled(2, false);
    p.set_cue_stream_enabled(4, false);
    p.seek(250 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["seeked@250"], "disabled replaced streams emit nothing");

    // Before `play()` decodes the start, cues at 0 are still ahead.
    let mut p = open(&[TextureFormat::Bc1]);
    p.replace_cues(vec![CueTrack { name: "s".into(), cues: vec![cue(0, Cue::new("zero"))] }]);
    assert_eq!(log.take(&mut p), Vec::<String>::new());
    p.play().unwrap();
    assert_eq!(log.take(&mut p), ["started", "cue zero@0"]);
}

#[test]
fn replaced_cues_stop_at_the_duration() {
    let mut log = Log::default();
    let cue = |ms: u64, c: Cue| TimedCue { time_us: ms * 1000, cue: c };
    let tracks = vec![CueTrack {
        name: "s".into(),
        cues: vec![cue(450, Cue { duration: 200_000, ..Cue::new("over") }), cue(500, Cue::new("end")), cue(520, Cue::new("after"))],
    }];
    let mut p = open(&[TextureFormat::Bc1]);
    p.replace_cues(tracks.clone());
    p.set_looping(true);
    p.play().unwrap();
    p.advance(480 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["started", "cue over@450"]);
    p.advance(60 * MS).unwrap();
    assert_eq!(log.take(&mut p), ["cue end@500", "end over", "looped"], "the clock overshoots to 540 ms but after stays unplayed");

    let mut p = open(&[TextureFormat::Bc1]);
    p.replace_cues(tracks);
    p.play().unwrap();
    p.advance(Duration::from_secs(1)).unwrap();
    assert_eq!(log.take(&mut p), ["started", "cue over@450", "cue end@500", "end over", "finished"]);
    p.replace_cues(Vec::new());
    assert_eq!(log.take(&mut p), Vec::<String>::new(), "finished players start no cues");
    assert!(p.cue_streams().is_empty());
}

/// Names of the active cues, as `Log` names them.
fn active(p: &Player<Cursor<Vec<u8>>>) -> Vec<String> {
    p.active_cues().map(|c| c.get_str("color").unwrap_or(&c.kind).to_string()).collect()
}

/// The upcoming cues within `ms`, as `name+until_ms`.
fn upcoming(p: &Player<Cursor<Vec<u8>>>, ms: u32) -> Vec<String> {
    p.upcoming_cues(ms * MS).map(|(until, c)| format!("{}+{}", c.get_str("color").unwrap_or(&c.kind), until.as_millis())).collect()
}

#[test]
fn active_cues_follow_the_events() {
    let mut p = open(&[TextureFormat::Bc1]);
    assert!(active(&p).is_empty());
    p.play().unwrap();
    p.advance(130 * MS).unwrap();
    assert_eq!(active(&p), ["red"]);
    p.advance(130 * MS).unwrap();
    assert_eq!(active(&p), ["red", "blue"], "instantaneous a is never active");
    p.advance(70 * MS).unwrap();
    assert_eq!(active(&p), ["blue"], "red ended at 320 ms");
    let blue = p.active_cues().next().unwrap().clone();
    assert_eq!((blue.time, blue.duration), (250 * MS, 100 * MS));

    p.seek(300 * MS).unwrap();
    assert_eq!(active(&p), ["red", "blue"], "in start order after a seek");
    p.set_cue_stream_enabled(3, false);
    assert!(active(&p).is_empty());
    p.set_cue_stream_enabled(3, true);
    p.advance(160 * MS).unwrap();
    assert_eq!(active(&p), ["long"]);
    p.set_looping(true);
    p.advance(50 * MS).unwrap();
    assert!(active(&p).is_empty(), "looping ends long");
    p.set_looping(false);
    p.advance(Duration::from_secs(1)).unwrap();
    assert!(p.is_finished());
    assert!(active(&p).is_empty());

    let mut p = open(&[TextureFormat::Bc1]);
    p.play().unwrap();
    p.advance(100 * MS).unwrap();
    p.replace_cues(replacement_tracks());
    let texts: Vec<_> = p.active_cues().filter_map(|c| c.get_str("text")).collect();
    assert_eq!(texts, ["one"], "replaced cues are active like file cues");
}

#[test]
fn upcoming_cues_look_ahead() {
    let mut p = open(&[TextureFormat::Bc1]);
    assert_eq!(upcoming(&p, 200), ["red+120", "a+150"], "before play, from the start");
    p.play().unwrap();
    p.advance(130 * MS).unwrap();
    assert_eq!(upcoming(&p, 200), ["a+20", "blue+120"], "emitted red is not upcoming");
    let blue_id = p.upcoming_cues(200 * MS).nth(1).unwrap().1.id;
    p.set_cue_stream_enabled(3, false);
    assert_eq!(upcoming(&p, 200), ["a+20"], "disabled streams are left out");
    p.set_cue_stream_enabled(3, true);
    p.advance(130 * MS).unwrap();
    let started: Vec<u64> = std::iter::from_fn(|| p.poll_event())
        .filter_map(|e| match e {
            Event::Cue(c) => Some(c.id),
            _ => None,
        })
        .collect();
    assert!(started.contains(&blue_id), "the ids are the events' ids");

    p.set_looping(true);
    p.seek(430 * MS).unwrap();
    assert_eq!(upcoming(&p, 200), ["long+20", "red+190"], "into the next pass when looping");
    assert_eq!(upcoming(&p, 10_000), ["long+20", "red+190", "a+220", "blue+320", "b+490"], "each cue once");
    p.set_looping(false);
    assert_eq!(upcoming(&p, 10_000), ["long+20"]);
    p.play().unwrap();
    p.advance(Duration::from_secs(1)).unwrap();
    assert!(p.is_finished());
    assert!(upcoming(&p, 10_000).is_empty(), "finished players have nothing ahead");
}

#[cfg(feature = "serde")]
#[test]
fn cue_events_parse_into_game_types() {
    #[derive(Debug, PartialEq, serde::Deserialize)]
    enum GameCue {
        #[serde(rename = "light.flash")]
        Flash { color: String },
        #[serde(rename = "long")]
        Long,
    }
    let mut p = open(&[TextureFormat::Bc1]);
    p.play().unwrap();
    p.advance(Duration::from_secs(1)).unwrap();
    let mut parsed = Vec::new();
    while let Some(e) = p.poll_event() {
        if let Event::Cue(c) = e {
            match c.parse::<GameCue>() {
                Ok(cue) => parsed.push(cue),
                Err(e) => assert!(e.is_unknown_type(), "{e}"),
            }
        }
    }
    let flash = |c: &str| GameCue::Flash { color: c.into() };
    assert_eq!(parsed, [flash("red"), flash("blue"), GameCue::Long]);
}
