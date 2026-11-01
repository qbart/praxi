//! Cue sheets: cue streams as JSON (`docs/spec/CUESHEET.md`).

use crate::{CUE_NAME_SIZE, Cue, CueTrack, TimedCue, Value};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value as Json};
use std::fmt;

/// The cue sheet version this module reads and writes.
pub const VERSION: u32 = 1;

/// Parses a cue sheet into tracks, checking every rule of the spec.
pub fn parse(text: &str) -> Result<Vec<CueTrack>, String> {
    let sheet: Sheet = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if sheet.version != VERSION {
        return Err(format!("unsupported cue sheet version {}", sheet.version));
    }
    let mut scratch = Vec::new();
    sheet
        .streams
        .into_iter()
        .map(|s| {
            if s.name.len() > CUE_NAME_SIZE || s.name.contains('\0') {
                return Err(format!("stream {:?}: name longer than {CUE_NAME_SIZE} bytes or with a zero byte", s.name));
            }
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
                    let cue = Cue { duration, kind: c.kind, fields };
                    // Type and key lengths: the payload encoder checks them.
                    scratch.clear();
                    cue.encode(&mut scratch).map_err(|e| at(e.to_string()))?;
                    Ok(TimedCue { time_us, cue })
                })
                .collect::<Result<_, String>>()?;
            Ok(CueTrack { name: s.name, cues })
        })
        .collect()
}

/// Writes tracks as a pretty-printed cue sheet ending with a line break.
pub fn write(tracks: &[CueTrack]) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(&to_sheet(tracks)?).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
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
    Ok(Sheet { version: VERSION, streams })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_types_and_order() {
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
        let text = write(&tracks).unwrap();
        assert!(text.ends_with("}\n"), "{text}");
        assert!(text.contains("\"time\": 1.000001"), "{text}");
        assert!(text.contains("\"f\": 3.0"), "{text}");
        assert!(text.contains("\"bytes\": \"00ff10\""), "{text}");
        let json: Json = serde_json::from_str(&text).unwrap();
        assert!(json["streams"][0]["cues"][0].get("duration").is_none(), "zero durations are left out");
        assert!(json["streams"][0]["cues"][0].get("fields").is_none(), "no fields are left out");
        assert_eq!(parse(&text).unwrap(), tracks);
    }

    #[test]
    fn rejects_bad_input() {
        let cue = |c: &str| format!(r#"{{"version": 1, "streams": [{{"name": "s", "cues": [{c}]}}]}}"#);
        assert!(parse(&cue(r#"{"time": 1, "type": "t"}"#)).is_ok());
        let long_name = format!(r#"{{"version": 1, "streams": [{{"name": "{}", "cues": []}}]}}"#, "n".repeat(33));
        let bad = [
            r#"{"version": 2, "streams": []}"#.to_string(),
            r#"{"version": 1, "streams": [], "extra": 1}"#.to_string(),
            long_name,
            r#"{"version": 1, "streams": [{"name": "a\u0000b", "cues": []}]}"#.to_string(),
            cue(r#"{"time": -1, "type": "t"}"#),
            cue(r#"{"time": "1", "type": "t"}"#),
            cue(r#"{"time": 1, "duration": -0.5, "type": "t"}"#),
            cue(r#"{"time": 1, "type": ""}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"": 1}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": 1, "a": 2}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": null}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": [1]}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": 9223372036854775808}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": {"bytes": "abc"}}}"#),
            cue(r#"{"time": 1, "type": "t", "fields": {"a": {"other": "ab"}}}"#),
            cue(r#"{"time": 1, "typ": "t"}"#),
        ];
        for b in bad {
            assert!(parse(&b).is_err(), "{b}");
        }
        let max_name = format!(r#"{{"version": 1, "streams": [{{"name": "{}", "cues": []}}]}}"#, "n".repeat(32));
        assert!(parse(&max_name).is_ok());
        let t = parse(&cue(r#"{"time": 0.0000004, "type": "t", "fields": {"i": 9223372036854775807, "e": 1e3}}"#)).unwrap();
        let c = &t[0].cues[0];
        assert_eq!(c.time_us, 0, "rounded to microseconds");
        assert_eq!(c.cue.fields, [("i".into(), Value::Int(i64::MAX)), ("e".into(), Value::Float(1000.0))]);
        let nan = Cue { fields: vec![("n".into(), Value::Float(f64::NAN))], ..Cue::new("t") };
        assert!(write(&[CueTrack { name: "s".into(), cues: vec![TimedCue { time_us: 0, cue: nan }] }]).is_err());
    }
}
