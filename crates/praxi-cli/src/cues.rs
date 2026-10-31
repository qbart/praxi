//! `praxi cues`: cue sheets (`docs/features/cue-editing.md`, `docs/spec/CUESHEET.md`).

use crate::AnyResult;
use praxi_format::{Cue, CueTrack, Reader, TimedCue, Value};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value as Json};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

const SHEET_VERSION: u32 = 1;

#[derive(clap::Subcommand)]
pub enum CuesCmd {
    /// Write the cues of a .praxi file as a cue sheet.
    Export {
        file: PathBuf,
        /// Output file (default: standard output).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Replace the cue streams of a .praxi file with those of cue sheets.
    Import {
        file: PathBuf,
        #[arg(required = true)]
        sheets: Vec<PathBuf>,
        /// Output file; may be the input file.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Convert SubRip (.srt) subtitles to a cue sheet.
    Srt {
        input: PathBuf,
        /// Output file (default: standard output).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Name of the cue stream.
        #[arg(long, default_value = "subtitles")]
        stream: String,
        /// Type of every cue.
        #[arg(long = "type", default_value = "subtitle")]
        kind: String,
        /// Field holding the subtitle text.
        #[arg(long, default_value = "text")]
        text_key: String,
        /// String field added to every cue, KEY=VALUE (repeatable).
        #[arg(long = "field", value_parser = parse_field)]
        fields: Vec<(String, String)>,
    },
}

pub fn run(cmd: CuesCmd) -> AnyResult<()> {
    match cmd {
        CuesCmd::Export { file, output } => {
            let mut reader = Reader::new(BufReader::new(File::open(&file)?))?;
            let tracks = praxi_format::read_cues(&mut reader)?;
            write_sheet(&to_sheet(&tracks)?, output.as_deref())
        }
        CuesCmd::Import { file, sheets, output } => {
            let mut tracks = Vec::new();
            for path in &sheets {
                let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
                tracks.extend(parse_sheet(&text).map_err(|e| format!("{}: {e}", path.display()))?);
            }
            import(&file, &tracks, &output)
        }
        CuesCmd::Srt { input, output, stream, kind, text_key, fields: extra } => {
            for (i, (key, _)) in extra.iter().enumerate() {
                if *key == text_key || extra[..i].iter().any(|(k, _)| k == key) {
                    return Err(format!("field {key} given twice").into());
                }
            }
            let text = std::fs::read_to_string(&input).map_err(|e| format!("{}: {e}", input.display()))?;
            let subtitles = parse_srt(&text).map_err(|e| format!("{}: {e}", input.display()))?;
            let cues = subtitles
                .into_iter()
                .map(|s| {
                    let mut fields = vec![(text_key.clone(), Value::String(s.text))];
                    fields.extend(extra.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))));
                    TimedCue { time_us: s.start_us, cue: Cue { duration: s.end_us - s.start_us, kind: kind.clone(), fields } }
                })
                .collect();
            write_sheet(&to_sheet(&[CueTrack { name: stream, cues }])?, output.as_deref())
        }
    }
}

fn parse_field(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.to_string(), v.to_string())),
        _ => Err(format!("expected KEY=VALUE, got {s:?}")),
    }
}

/// Writes the file with the new cue streams to a temporary file next to `output`, then
/// renames it over `output` (which may be `input`).
fn import(input: &Path, tracks: &[CueTrack], output: &Path) -> AnyResult<()> {
    let name = output.file_name().ok_or("output has no file name")?.to_string_lossy();
    let temp = output.with_file_name(format!(".{name}.praxi-tmp"));
    let result = (|| -> AnyResult<()> {
        let reader = Reader::new(BufReader::new(File::open(input)?))?;
        let out = praxi_format::replace_cues(reader, BufWriter::new(File::create(&temp)?), tracks)?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    std::fs::rename(&temp, output)?;
    let count: usize = tracks.iter().map(|t| t.cues.len()).sum();
    eprintln!("{}: {} cue streams, {count} cues", output.display(), tracks.len());
    Ok(())
}

// Sheet documents, as serde sees them.

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sheet {
    version: u32,
    streams: Vec<SheetStream>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SheetStream {
    name: String,
    cues: Vec<SheetCue>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SheetCue {
    time: Json,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration: Option<Json>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, skip_serializing_if = "Fields::is_empty")]
    fields: Fields,
}

/// Fields in order, rejecting duplicate keys (a `Map` would keep the last one).
#[derive(Default)]
struct Fields(Vec<(String, Json)>);

impl Fields {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for Fields {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(self.0.iter().map(|(k, v)| (k, v)))
    }
}

impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Fields, D::Error> {
        struct FieldsVisitor;
        impl<'de> Visitor<'de> for FieldsVisitor {
            type Value = Fields;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object of cue fields")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Fields, A::Error> {
                let mut fields: Vec<(String, Json)> = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Json>()? {
                    if fields.iter().any(|(k, _)| *k == key) {
                        return Err(serde::de::Error::custom(format!("duplicate field {key:?}")));
                    }
                    fields.push((key, value));
                }
                Ok(Fields(fields))
            }
        }
        d.deserialize_map(FieldsVisitor)
    }
}

/// Parses a cue sheet into tracks.
pub fn parse_sheet(text: &str) -> Result<Vec<CueTrack>, String> {
    let sheet: Sheet = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if sheet.version != SHEET_VERSION {
        return Err(format!("unsupported cue sheet version {}", sheet.version));
    }
    sheet
        .streams
        .into_iter()
        .map(|s| {
            let cues = s
                .cues
                .into_iter()
                .enumerate()
                .map(|(i, c)| {
                    let at = |e: String| format!("stream {:?}, cue {i}: {e}", s.name);
                    let time_us = seconds_to_us(&c.time).map_err(|e| at(format!("time {e}")))?;
                    let duration = match &c.duration {
                        Some(d) => seconds_to_us(d).map_err(|e| at(format!("duration {e}")))?,
                        None => 0,
                    };
                    let fields = c
                        .fields
                        .0
                        .into_iter()
                        .map(|(k, v)| json_to_value(&v).map(|v| (k.clone(), v)).map_err(|e| at(format!("field {k:?}: {e}"))))
                        .collect::<Result<_, _>>()?;
                    Ok(TimedCue { time_us, cue: Cue { duration, kind: c.kind, fields } })
                })
                .collect::<Result<_, String>>()?;
            Ok(CueTrack { name: s.name, cues })
        })
        .collect()
}

/// Seconds, a non-negative number, to whole microseconds.
fn seconds_to_us(v: &Json) -> Result<u64, String> {
    let seconds = v.as_f64().filter(|s| s.is_finite()).ok_or_else(|| format!("must be a number, got {v}"))?;
    let us = (seconds * 1e6).round();
    if !(0.0..=u64::MAX as f64).contains(&us) {
        return Err(format!("{seconds} is out of range"));
    }
    Ok(us as u64)
}

fn json_to_value(v: &Json) -> Result<Value, String> {
    Ok(match v {
        Json::String(s) => Value::String(s.clone()),
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) if n.is_i64() => Value::Int(n.as_i64().expect("i64")),
        Json::Number(n) if n.is_f64() => Value::Float(n.as_f64().expect("f64")),
        Json::Number(n) => return Err(format!("integer {n} is outside the i64 range")),
        Json::Object(o) if o.len() == 1 && o.contains_key("bytes") => match &o["bytes"] {
            Json::String(hex) => Value::Bytes(from_hex(hex)?),
            _ => return Err("bytes must be a hex string".into()),
        },
        other => return Err(format!("unsupported value {other}")),
    })
}

fn value_to_json(v: &Value) -> Result<Json, String> {
    Ok(match v {
        Value::String(s) => Json::String(s.clone()),
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::Number((*i).into()),
        Value::Float(f) => Json::Number(Number::from_f64(*f).ok_or_else(|| format!("float {f} cannot be written to JSON"))?),
        Value::Bytes(b) => {
            let hex: String = b.iter().map(|b| format!("{b:02x}")).collect();
            Json::Object(Map::from_iter([("bytes".to_string(), Json::String(hex))]))
        }
    })
}

fn from_hex(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{hex:?} is not an even number of hex digits"));
    }
    Ok((0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex")).collect())
}

/// Microseconds as seconds: the shortest decimal for the `f64` nearest to `us / 10^6` has
/// at most six decimals and reads back to `us`.
fn us_to_seconds(us: u64) -> Json {
    Json::Number(Number::from_f64(us as f64 / 1e6).expect("finite"))
}

fn to_sheet(tracks: &[CueTrack]) -> Result<Sheet, String> {
    let streams = tracks
        .iter()
        .map(|t| {
            let cues = t
                .cues
                .iter()
                .map(|c| {
                    let fields = c
                        .cue
                        .fields
                        .iter()
                        .map(|(k, v)| value_to_json(v).map(|v| (k.clone(), v)))
                        .collect::<Result<_, _>>()
                        .map_err(|e| format!("stream {:?}, cue at {} us: {e}", t.name, c.time_us))?;
                    Ok(SheetCue {
                        time: us_to_seconds(c.time_us),
                        duration: (c.cue.duration > 0).then(|| us_to_seconds(c.cue.duration)),
                        kind: c.cue.kind.clone(),
                        fields: Fields(fields),
                    })
                })
                .collect::<Result<_, String>>()?;
            Ok(SheetStream { name: t.name.clone(), cues })
        })
        .collect::<Result<_, String>>()?;
    Ok(Sheet { version: SHEET_VERSION, streams })
}

fn write_sheet(sheet: &Sheet, output: Option<&Path>) -> AnyResult<()> {
    let mut text = serde_json::to_string_pretty(sheet)?;
    text.push('\n');
    match output {
        Some(path) => std::fs::write(path, text)?,
        None => std::io::stdout().lock().write_all(text.as_bytes())?,
    }
    Ok(())
}

/// One SubRip subtitle.
#[derive(Debug, PartialEq)]
pub struct Subtitle {
    pub start_us: u64,
    pub end_us: u64,
    pub text: String,
}

/// Parses SubRip text: blocks separated by blank lines, each an optional sequence number,
/// a `START --> END` line and text lines.
pub fn parse_srt(text: &str) -> Result<Vec<Subtitle>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut blocks = vec![Vec::new()];
    for line in text.lines() {
        match blocks.last_mut().expect("block") {
            block if line.trim().is_empty() => {
                if !block.is_empty() {
                    blocks.push(Vec::new());
                }
            }
            block => block.push(line),
        }
    }
    let mut out = Vec::new();
    for (n, block) in blocks.into_iter().filter(|b| !b.is_empty()).enumerate() {
        let at = |e: &str| format!("subtitle {}: {e}", n + 1);
        let mut lines = block.into_iter().peekable();
        if lines.peek().is_some_and(|l| !l.contains("-->")) {
            lines.next(); // sequence number
        }
        let timing = lines.next().filter(|l| l.contains("-->")).ok_or_else(|| at("no START --> END line"))?;
        let (start, end) = timing.split_once("-->").expect("-->");
        let end = end.split_whitespace().next().unwrap_or("");
        let start_us = srt_time(start.trim()).ok_or_else(|| at(&format!("bad time {:?}", start.trim())))?;
        let end_us = srt_time(end).ok_or_else(|| at(&format!("bad time {end:?}")))?;
        if end_us < start_us {
            return Err(at("ends before it starts"));
        }
        out.push(Subtitle { start_us, end_us, text: lines.collect::<Vec<_>>().join("\n") });
    }
    Ok(out)
}

/// `HH:MM:SS,mmm` (or `.mmm`) in microseconds.
fn srt_time(s: &str) -> Option<u64> {
    let (hms, frac) = s.split_once([',', '.'])?;
    let mut parts = hms.split(':');
    let (h, m, sec) = (parts.next()?, parts.next()?, parts.next()?);
    let digits = |p: &str| (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())).then(|| p.parse::<u64>().ok()).flatten();
    if parts.next().is_some() || frac.len() > 3 {
        return None;
    }
    let (h, m, sec, frac_value) = (digits(h)?, digits(m)?, digits(sec)?, digits(frac)?);
    if m > 59 || sec > 59 {
        return None;
    }
    let ms = frac_value * 10u64.pow(3 - frac.len() as u32);
    Some(((h * 60 + m) * 60 + sec) * 1_000_000 + ms * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_roundtrip_keeps_types_and_order() {
        let tracks = vec![CueTrack {
            name: "cues".into(),
            cues: vec![
                TimedCue { time_us: 2_500_000, cue: Cue::new("explosion") },
                TimedCue {
                    time_us: 1_000_001,
                    cue: Cue {
                        duration: 333_333,
                        kind: "light.flash".into(),
                        fields: vec![
                            ("z".into(), Value::from("red")),
                            ("a".into(), Value::Int(-3)),
                            ("f".into(), Value::Float(3.0)),
                            ("b".into(), Value::Bool(true)),
                            ("raw".into(), Value::Bytes(vec![0, 0xff, 0x10])),
                        ],
                    },
                },
            ],
        }];
        let text = serde_json::to_string_pretty(&to_sheet(&tracks).unwrap()).unwrap();
        assert!(text.contains("\"time\": 1.000001"), "{text}");
        assert!(text.contains("\"f\": 3.0"), "{text}");
        assert!(text.contains("\"bytes\": \"00ff10\""), "{text}");
        let json: Json = serde_json::from_str(&text).unwrap();
        assert!(json["streams"][0]["cues"][0].get("duration").is_none(), "zero durations are left out");
        assert!(json["streams"][0]["cues"][0].get("fields").is_none(), "no fields are left out");
        assert_eq!(parse_sheet(&text).unwrap(), tracks);
    }

    #[test]
    fn sheet_rejects_bad_input() {
        let cue = |c: &str| format!(r#"{{"version": 1, "streams": [{{"name": "s", "cues": [{c}]}}]}}"#);
        assert!(parse_sheet(&cue(r#"{"time": 1, "type": "t"}"#)).is_ok());
        let bad = [
            r#"{"version": 2, "streams": []}"#.to_string(),
            r#"{"version": 1, "streams": [], "extra": 1}"#.to_string(),
            cue(r#"{"time": -1, "type": "t"}"#),
            cue(r#"{"time": "1", "type": "t"}"#),
            cue(r#"{"time": 1, "duration": -0.5, "type": "t"}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": 1, "a": 2}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": null}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": [1]}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": 9223372036854775808}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": {"bytes": "abc"}}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": {"other": "ab"}}}"#),
            cue(r#"{"time": 1, "typ": "t"}"#),
        ];
        for b in bad {
            assert!(parse_sheet(&b).is_err(), "{b}");
        }
        let t = parse_sheet(&cue(r#"{"time": 0.0000004, "type": "t", "fields": {"i": 9223372036854775807, "e": 1e3}}"#)).unwrap();
        let c = &t[0].cues[0];
        assert_eq!(c.time_us, 0, "rounded to microseconds");
        assert_eq!(c.cue.fields, [("i".into(), Value::Int(i64::MAX)), ("e".into(), Value::Float(1000.0))]);
        assert!(to_sheet(&[CueTrack { name: "s".into(), cues: vec![TimedCue { time_us: 0, cue: Cue { fields: vec![("n".into(), Value::Float(f64::NAN))], ..Cue::new("t") } }] }]).is_err());
    }

    #[test]
    fn parses_srt() {
        let srt = "\u{feff}1\r\n00:00:01,000 --> 00:00:04,500\r\nHello.\r\n<i>Second line</i>\r\n\r\n\r\n00:01:02.25 --> 00:01:03,000 X1:10\n\n3\n10:00:00,000 --> 10:00:00,000\n";
        assert_eq!(
            parse_srt(srt).unwrap(),
            [
                Subtitle { start_us: 1_000_000, end_us: 4_500_000, text: "Hello.\n<i>Second line</i>".into() },
                Subtitle { start_us: 62_250_000, end_us: 63_000_000, text: String::new() },
                Subtitle { start_us: 36_000_000_000, end_us: 36_000_000_000, text: String::new() },
            ]
        );
        assert!(parse_srt("1\n00:00:02,000 --> 00:00:01,000\nx\n").is_err(), "ends before start");
        assert!(parse_srt("1\nhello\n").is_err(), "no timing");
        assert!(parse_srt("00:00:01 --> 00:00:02,000\n").is_err(), "no milliseconds");
        assert!(parse_srt("00:61:01,000 --> 00:62:02,000\n").is_err(), "minutes");
    }
}
