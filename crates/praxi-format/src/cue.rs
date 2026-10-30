//! Cue payloads (`docs/spec/PRAXI.md#cue-payload`).

use crate::{Error, Result, corrupt};

const VALUE_STRING: u8 = 1;
const VALUE_INT: u8 = 2;
const VALUE_FLOAT: u8 = 3;
const VALUE_BOOL: u8 = 4;
const VALUE_BYTES: u8 = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Bytes(Vec<u8>),
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::String(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Value {
        Value::String(s)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Value {
        Value::Int(v)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Value {
        Value::Float(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Value {
        Value::Bool(v)
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Value {
        Value::Bytes(v)
    }
}

/// One cue: a typed event with a duration (in its stream's time base) and fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub duration: u64,
    pub kind: String,
    pub fields: Vec<(String, Value)>,
}

impl Cue {
    pub fn new(kind: impl Into<String>) -> Cue {
        Cue { duration: 0, kind: kind.into(), fields: Vec::new() }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Appends the payload encoding of this cue to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let invalid = |msg: String| Err(Error::Invalid(msg));
        if self.kind.is_empty() || self.kind.len() > u16::MAX as usize {
            return invalid(format!("cue type length {} out of range", self.kind.len()));
        }
        if self.fields.len() > u16::MAX as usize {
            return invalid("too many cue fields".into());
        }
        out.extend_from_slice(&self.duration.to_le_bytes());
        out.extend_from_slice(&(self.kind.len() as u16).to_le_bytes());
        out.extend_from_slice(self.kind.as_bytes());
        out.extend_from_slice(&(self.fields.len() as u16).to_le_bytes());
        for (i, (key, value)) in self.fields.iter().enumerate() {
            if key.is_empty() || key.len() > u16::MAX as usize {
                return invalid(format!("cue field key length {} out of range", key.len()));
            }
            if self.fields[..i].iter().any(|(k, _)| k == key) {
                return invalid(format!("duplicate cue field {key:?}"));
            }
            out.extend_from_slice(&(key.len() as u16).to_le_bytes());
            out.extend_from_slice(key.as_bytes());
            let with_len = |out: &mut Vec<u8>, ty: u8, bytes: &[u8]| {
                if bytes.len() > u32::MAX as usize {
                    return invalid(format!("cue field {key:?} too long"));
                }
                out.push(ty);
                out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                out.extend_from_slice(bytes);
                Ok(())
            };
            match value {
                Value::String(s) => with_len(out, VALUE_STRING, s.as_bytes())?,
                Value::Bytes(b) => with_len(out, VALUE_BYTES, b)?,
                Value::Int(v) => {
                    out.push(VALUE_INT);
                    out.extend_from_slice(&v.to_le_bytes());
                }
                Value::Float(v) => {
                    out.push(VALUE_FLOAT);
                    out.extend_from_slice(&v.to_le_bytes());
                }
                Value::Bool(v) => {
                    out.push(VALUE_BOOL);
                    out.push(*v as u8);
                }
            }
        }
        Ok(())
    }

    pub fn decode(payload: &[u8]) -> Result<Cue> {
        let mut r = Cursor { b: payload, at: 0 };
        let duration = u64::from_le_bytes(r.array()?);
        let kind = r.text16()?;
        if kind.is_empty() {
            return corrupt("empty cue type");
        }
        let count = u16::from_le_bytes(r.array()?) as usize;
        let mut fields: Vec<(String, Value)> = Vec::with_capacity(count);
        for _ in 0..count {
            let key = r.text16()?;
            if key.is_empty() {
                return corrupt("empty cue field key");
            }
            if fields.iter().any(|(k, _)| *k == key) {
                return corrupt(format!("duplicate cue field {key:?}"));
            }
            let value = match r.take(1)?[0] {
                VALUE_STRING => {
                    let len = u32::from_le_bytes(r.array()?) as usize;
                    Value::String(utf8(r.take(len)?)?)
                }
                VALUE_INT => Value::Int(i64::from_le_bytes(r.array()?)),
                VALUE_FLOAT => Value::Float(f64::from_le_bytes(r.array()?)),
                VALUE_BOOL => match r.take(1)?[0] {
                    0 => Value::Bool(false),
                    1 => Value::Bool(true),
                    b => return corrupt(format!("cue bool value {b}")),
                },
                VALUE_BYTES => {
                    let len = u32::from_le_bytes(r.array()?) as usize;
                    Value::Bytes(r.take(len)?.to_vec())
                }
                ty => return corrupt(format!("unknown cue value type {ty}")),
            };
            fields.push((key, value));
        }
        if r.at != payload.len() {
            return corrupt("trailing bytes after cue fields");
        }
        Ok(Cue { duration, kind, fields })
    }
}

struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let Some(s) = self.at.checked_add(n).and_then(|end| self.b.get(self.at..end)) else {
            return corrupt("truncated cue payload");
        };
        self.at += n;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("take returns N bytes"))
    }

    fn text16(&mut self) -> Result<String> {
        let len = u16::from_le_bytes(self.array()?) as usize;
        utf8(self.take(len)?)
    }
}

fn utf8(b: &[u8]) -> Result<String> {
    match std::str::from_utf8(b) {
        Ok(s) => Ok(s.to_string()),
        Err(e) => corrupt(format!("invalid UTF-8 in cue: {e}")),
    }
}
