//! Cues into game types with serde (`docs/features/cue-runtime.md#typed-cues`).

use super::Value;
use serde::Deserialize;
use serde::de::value::{BorrowedStrDeserializer, SeqDeserializer};
use serde::de::{
    self, DeserializeSeed, Deserializer, EnumAccess, MapAccess, VariantAccess, Visitor,
};
use std::fmt;

/// Why a cue did not parse into a game type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// The cue type.
    pub kind: String,
    /// The field the error is about, if any.
    pub field: Option<String>,
    pub message: String,
    unknown_type: bool,
}

impl ParseError {
    /// The cue type is not a variant of the enum the cue was parsed into.
    pub fn is_unknown_type(&self) -> bool {
        self.unknown_type
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cue {:?}: ", self.kind)?;
        if let Some(field) = &self.field {
            write!(f, "field {field:?}: ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

/// Reads a cue of type `kind` with `fields` into `T`: an enum takes the variant named by
/// `kind`, a struct or map takes the fields.
pub fn parse<'a, T: Deserialize<'a>>(
    kind: &'a str,
    fields: &'a [(String, Value)],
) -> Result<T, ParseError> {
    T::deserialize(CueDe { kind, fields }).map_err(|e| ParseError {
        kind: kind.to_string(),
        field: e.field,
        message: e.message,
        unknown_type: e.unknown_variant,
    })
}

/// The error while deserializing; [`parse`] adds the cue type.
#[derive(Debug)]
struct Error {
    field: Option<String>,
    message: String,
    /// An unknown enum variant; only the cue type's counts as an unknown type.
    unknown_variant: bool,
}

impl Error {
    /// Attaches `field` unless the error already names one.
    fn at(mut self, field: &str) -> Error {
        self.field.get_or_insert_with(|| field.to_string());
        self.unknown_variant = false;
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl de::Error for Error {
    fn custom<M: fmt::Display>(msg: M) -> Error {
        Error {
            field: None,
            message: msg.to_string(),
            unknown_variant: false,
        }
    }

    fn unknown_variant(variant: &str, expected: &'static [&'static str]) -> Error {
        let expected: Vec<String> = expected.iter().map(|v| format!("`{v}`")).collect();
        let message = format!(
            "unknown variant `{variant}`, expected one of {}",
            expected.join(", ")
        );
        Error {
            field: None,
            message,
            unknown_variant: true,
        }
    }

    fn missing_field(field: &'static str) -> Error {
        Error {
            field: Some(field.to_string()),
            message: "missing".into(),
            unknown_variant: false,
        }
    }
}

/// A whole cue: its type selects an enum variant.
struct CueDe<'a> {
    kind: &'a str,
    fields: &'a [(String, Value)],
}

impl<'de> Deserializer<'de> for CueDe<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        FieldsDe(self.fields).deserialize_any(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_enum(self)
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        option unit newtype_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

impl<'de> EnumAccess<'de> for CueDe<'de> {
    type Error = Error;
    type Variant = FieldsDe<'de>;

    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, FieldsDe<'de>), Error> {
        let variant = seed
            .deserialize(BorrowedStrDeserializer::<Error>::new(self.kind))
            .map_err(|mut e| {
                if e.unknown_variant {
                    e.message = format!("unknown cue type {:?}", self.kind);
                }
                e
            })?;
        Ok((variant, FieldsDe(self.fields)))
    }
}

impl<'de> VariantAccess<'de> for FieldsDe<'de> {
    type Error = Error;

    /// A unit variant ignores the fields.
    fn unit_variant(self) -> Result<(), Error> {
        Ok(())
    }

    fn newtype_variant_seed<S: DeserializeSeed<'de>>(self, seed: S) -> Result<S::Value, Error> {
        seed.deserialize(self)
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, _visitor: V) -> Result<V::Value, Error> {
        Err(de::Error::invalid_type(
            de::Unexpected::Map,
            &"a struct, newtype or unit variant",
        ))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_any(visitor)
    }
}

/// A cue's fields as a map.
struct FieldsDe<'a>(&'a [(String, Value)]);

impl<'de> Deserializer<'de> for FieldsDe<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_map(FieldMap {
            fields: self.0.iter(),
            value: None,
        })
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        option unit newtype_struct seq tuple tuple_struct map struct enum identifier ignored_any
    }
}

struct FieldMap<'a> {
    fields: std::slice::Iter<'a, (String, Value)>,
    value: Option<(&'a str, &'a Value)>,
}

impl<'de> MapAccess<'de> for FieldMap<'de> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((key, value)) = self.fields.next() else {
            return Ok(None);
        };
        self.value = Some((key, value));
        seed.deserialize(BorrowedStrDeserializer::new(key))
            .map(Some)
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, Error> {
        let (key, value) = self.value.take().expect("next_value after next_key");
        seed.deserialize(ValueDe(value)).map_err(|e| e.at(key))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.fields.len())
    }
}

/// One field value.
struct ValueDe<'a>(&'a Value);

impl<'de> Deserializer<'de> for ValueDe<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Value::String(s) => visitor.visit_borrowed_str(s),
            Value::Int(v) => visitor.visit_i64(*v),
            Value::Float(v) => visitor.visit_f64(*v),
            Value::Bool(v) => visitor.visit_bool(*v),
            Value::Bytes(b) => visitor.visit_borrowed_bytes(b),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    /// Unit variants by name.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.0 {
            Value::String(s) => visitor.visit_enum(BorrowedStrDeserializer::new(s)),
            _ => self.deserialize_any(visitor),
        }
    }

    /// Bytes as a sequence, for `Vec<u8>`.
    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Value::Bytes(b) => visitor.visit_seq(SeqDeserializer::new(b.iter().copied())),
            _ => self.deserialize_any(visitor),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        unit unit_struct tuple tuple_struct map struct identifier ignored_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cue;

    fn cue(kind: &str, fields: &[(&str, Value)]) -> Cue {
        let mut c = Cue::new(kind);
        c.fields = fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        c
    }

    #[derive(Debug, PartialEq, Deserialize)]
    struct Sound {
        name: String,
        volume: Option<f32>,
    }

    #[derive(Debug, PartialEq, Deserialize)]
    enum Mood {
        #[serde(rename = "calm")]
        Calm,
        #[serde(rename = "tense")]
        Tense,
    }

    #[derive(Debug, PartialEq, Deserialize)]
    enum GameCue {
        #[serde(rename = "subtitle")]
        Subtitle {
            text: String,
            speaker: Option<String>,
        },
        #[serde(rename = "camera.shake")]
        Shake { strength: f32, frames: u8 },
        #[serde(rename = "sound")]
        Sound(Sound),
        #[serde(rename = "fade")]
        Fade,
        #[serde(rename = "music")]
        Music {
            mood: Mood,
            data: Vec<u8>,
            loud: bool,
        },
    }

    #[test]
    fn enums_take_the_variant_of_the_type() {
        let c = cue(
            "subtitle",
            &[("text", "Hi".into()), ("x", Value::Float(0.5))],
        );
        assert_eq!(
            c.parse::<GameCue>(),
            Ok(GameCue::Subtitle {
                text: "Hi".into(),
                speaker: None
            })
        );
        let c = cue(
            "camera.shake",
            &[("frames", Value::Int(12)), ("strength", Value::Int(3))],
        );
        assert_eq!(
            c.parse::<GameCue>(),
            Ok(GameCue::Shake {
                strength: 3.0,
                frames: 12
            }),
            "integers into floats"
        );
        let c = cue(
            "sound",
            &[("name", "door".into()), ("volume", Value::Float(0.5))],
        );
        assert_eq!(
            c.parse::<GameCue>(),
            Ok(GameCue::Sound(Sound {
                name: "door".into(),
                volume: Some(0.5)
            }))
        );
        assert_eq!(
            cue("fade", &[("anything", Value::Bool(true))]).parse::<GameCue>(),
            Ok(GameCue::Fade)
        );
        let c = cue(
            "music",
            &[
                ("mood", "tense".into()),
                ("data", Value::Bytes(vec![1, 2])),
                ("loud", Value::Bool(true)),
            ],
        );
        assert_eq!(
            c.parse::<GameCue>(),
            Ok(GameCue::Music {
                mood: Mood::Tense,
                data: vec![1, 2],
                loud: true
            })
        );
        assert_eq!(
            cue("x", &[("mood", "calm".into())])
                .parse::<std::collections::HashMap<String, Mood>>()
                .unwrap()["mood"],
            Mood::Calm
        );
    }

    #[test]
    fn structs_take_the_fields_of_any_cue() {
        #[derive(Debug, PartialEq, Deserialize)]
        struct Borrowed<'a> {
            text: &'a str,
            raw: &'a [u8],
            #[serde(default)]
            count: i64,
        }
        let c = cue(
            "anything",
            &[("text", "a".into()), ("raw", Value::Bytes(vec![7]))],
        );
        assert_eq!(
            c.parse::<Borrowed>(),
            Ok(Borrowed {
                text: "a",
                raw: &[7],
                count: 0
            })
        );

        #[derive(Debug, PartialEq, Deserialize)]
        struct Position {
            x: f32,
            y: f32,
        }
        let mut c = cue("marker", &[("label", "here".into())]);
        c.set_position(Some((0.25, 0.75)));
        assert_eq!(c.parse::<Position>(), Ok(Position { x: 0.25, y: 0.75 }));

        #[derive(Debug, PartialEq, Deserialize)]
        struct Unit;
        assert_eq!(c.parse::<Unit>(), Ok(Unit));
    }

    #[test]
    fn other_catches_unknown_types() {
        #[derive(Debug, PartialEq, Deserialize)]
        enum Known {
            #[serde(rename = "fade")]
            Fade,
            #[serde(other)]
            Other,
        }
        assert_eq!(cue("fade", &[]).parse::<Known>(), Ok(Known::Fade));
        assert_eq!(
            cue("explosion", &[("size", Value::Int(3))]).parse::<Known>(),
            Ok(Known::Other)
        );
    }

    #[test]
    fn errors_name_the_cue_and_the_field() {
        let err = cue("explosion", &[]).parse::<GameCue>().unwrap_err();
        assert!(err.is_unknown_type());
        assert_eq!(
            err.to_string(),
            "cue \"explosion\": unknown cue type \"explosion\""
        );

        let err = cue(
            "camera.shake",
            &[("strength", "hard".into()), ("frames", Value::Int(1))],
        )
        .parse::<GameCue>()
        .unwrap_err();
        assert!(!err.is_unknown_type());
        assert_eq!(err.field.as_deref(), Some("strength"));
        assert_eq!(
            err.to_string(),
            "cue \"camera.shake\": field \"strength\": invalid type: string \"hard\", expected f32"
        );

        let err = cue("subtitle", &[("speaker", "Bob".into())])
            .parse::<GameCue>()
            .unwrap_err();
        assert_eq!(err.to_string(), "cue \"subtitle\": field \"text\": missing");

        let err = cue(
            "camera.shake",
            &[("strength", Value::Int(1)), ("frames", Value::Int(300))],
        )
        .parse::<GameCue>()
        .unwrap_err();
        assert_eq!(err.field.as_deref(), Some("frames"));
        assert!(err.message.contains("300"), "{err}");

        let err = cue(
            "camera.shake",
            &[
                ("strength", Value::Float(1.0)),
                ("frames", Value::Float(2.0)),
            ],
        )
        .parse::<GameCue>()
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "cue \"camera.shake\": field \"frames\": invalid type: floating point `2.0`, expected u8"
        );

        let err = cue("music", &[("mood", "angry".into())])
            .parse::<GameCue>()
            .unwrap_err();
        assert!(
            !err.is_unknown_type(),
            "unknown unit variants in fields are field errors"
        );
        assert_eq!(err.field.as_deref(), Some("mood"));

        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Strict {
            text: String,
        }
        let err = cue(
            "subtitle",
            &[("text", "a".into()), ("x", Value::Float(0.1))],
        )
        .parse::<Strict>()
        .unwrap_err();
        assert!(err.message.contains("unknown field `x`"), "{err}");
    }
}
