//! The selected cue or stream as editable text (`docs/features/cue-editor.md#window`).
//!
//! Drafts hold what the user typed; they reach the document only when valid, so the document
//! always holds a valid cue sheet.

use crate::doc::EditCue;
use eframe::egui;
use praxi::Value;
use praxi::format::{CUE_NAME_SIZE, Cue, POSITION_X, POSITION_Y, cue_position};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    String,
    Int,
    Float,
    Bool,
    Bytes,
}

impl ValueType {
    const ALL: [ValueType; 5] = [
        ValueType::String,
        ValueType::Int,
        ValueType::Float,
        ValueType::Bool,
        ValueType::Bytes,
    ];

    fn name(self) -> &'static str {
        match self {
            ValueType::String => "string",
            ValueType::Int => "int",
            ValueType::Float => "float",
            ValueType::Bool => "bool",
            ValueType::Bytes => "bytes",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDraft {
    pub key: String,
    pub ty: ValueType,
    pub value: String,
}

/// Text of the selected cue.
#[derive(Debug, Clone, PartialEq)]
pub struct CueDraft {
    pub uid: u64,
    /// The cue and stream index the draft was made from.
    pub base: (usize, EditCue),
    pub stream: usize,
    pub time: String,
    pub duration: String,
    pub kind: String,
    pub fields: Vec<FieldDraft>,
}

/// Seconds as the shortest decimal that reads back to the same microseconds.
pub fn seconds(us: u64) -> String {
    format!("{}", us as f64 / 1e6)
}

/// Non-negative seconds to whole microseconds.
pub fn parse_seconds(s: &str) -> Option<u64> {
    let v: f64 = s.trim().parse().ok()?;
    let us = (v * 1e6).round();
    (v.is_finite() && (0.0..=u64::MAX as f64).contains(&us)).then_some(us as u64)
}

fn value_text(v: &Value) -> (ValueType, String) {
    match v {
        Value::String(s) => (ValueType::String, s.clone()),
        Value::Int(i) => (ValueType::Int, i.to_string()),
        Value::Float(f) => (ValueType::Float, format!("{f:?}")),
        Value::Bool(b) => (ValueType::Bool, b.to_string()),
        Value::Bytes(b) => (
            ValueType::Bytes,
            b.iter().map(|b| format!("{b:02x}")).collect(),
        ),
    }
}

fn parse_value(ty: ValueType, s: &str) -> Option<Value> {
    Some(match ty {
        ValueType::String => Value::String(s.to_string()),
        ValueType::Int => Value::Int(s.trim().parse().ok()?),
        ValueType::Float => Value::Float(s.trim().parse::<f64>().ok().filter(|f| f.is_finite())?),
        ValueType::Bool => Value::Bool(s.parse().ok()?),
        ValueType::Bytes => {
            let s = s.trim();
            if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Value::Bytes(
                (0..s.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
                    .collect(),
            )
        }
    })
}

/// A position coordinate to three decimals, as the inspector shows it.
pub fn round3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

/// Whether a cue type or field key is valid in a cue sheet.
fn valid_text16(s: &str) -> bool {
    !s.is_empty() && s.len() <= u16::MAX as usize
}

pub fn valid_stream_name(s: &str) -> bool {
    s.len() <= CUE_NAME_SIZE && !s.contains('\0')
}

impl CueDraft {
    pub fn new(stream: usize, cue: &EditCue) -> CueDraft {
        CueDraft {
            uid: cue.uid,
            base: (stream, cue.clone()),
            stream,
            time: seconds(cue.time_us),
            duration: seconds(cue.cue.duration),
            kind: cue.cue.kind.clone(),
            fields: cue
                .cue
                .fields
                .iter()
                .map(|(k, v)| {
                    let (ty, value) = value_text(v);
                    FieldDraft {
                        key: k.clone(),
                        ty,
                        value,
                    }
                })
                .collect(),
        }
    }

    fn key_ok(&self, i: usize) -> bool {
        let key = &self.fields[i].key;
        valid_text16(key) && !self.fields[..i].iter().any(|f| f.key == *key)
    }

    /// The stream index and cue, or `None` while any text is invalid.
    pub fn parse(&self) -> Option<(usize, EditCue)> {
        let time_us = parse_seconds(&self.time)?;
        let duration = parse_seconds(&self.duration)?;
        if !valid_text16(&self.kind) {
            return None;
        }
        let mut fields = Vec::new();
        for (i, f) in self.fields.iter().enumerate() {
            if !self.key_ok(i) {
                return None;
            }
            fields.push((f.key.clone(), parse_value(f.ty, &f.value)?));
        }
        Some((
            self.stream,
            EditCue {
                uid: self.uid,
                time_us,
                cue: Cue {
                    duration,
                    kind: self.kind.clone(),
                    fields,
                },
            },
        ))
    }

    /// The position the draft's `x` and `y` fields give, as `Cue::position` reads it.
    pub fn position(&self) -> Option<(f32, f32)> {
        let fields: Vec<(String, Value)> = self
            .fields
            .iter()
            .filter_map(|f| Some((f.key.clone(), parse_value(f.ty, &f.value)?)))
            .collect();
        cue_position(&fields)
    }

    /// Sets or removes the `x` and `y` fields, as `Cue::set_position` does.
    pub fn set_position(&mut self, position: Option<(f32, f32)>) {
        let mut cue = Cue::new("-");
        cue.set_position(position);
        match position {
            Some(_) => {
                for (key, value) in cue.fields {
                    let (ty, value) = value_text(&value);
                    match self.fields.iter_mut().find(|f| f.key == key) {
                        Some(f) => (f.ty, f.value) = (ty, value),
                        None => self.fields.push(FieldDraft { key, ty, value }),
                    }
                }
            }
            None => self
                .fields
                .retain(|f| f.key != POSITION_X && f.key != POSITION_Y),
        }
    }

    /// Shows the draft; returns true when an edit finished (a field left, a choice made).
    /// `picking` is the video position picker's state, toggled by the Pick button.
    pub fn ui(&mut self, ui: &mut egui::Ui, streams: &[String], picking: &mut bool) -> bool {
        let mut done = false;
        let red = ui.visuals().error_fg_color;
        egui::Grid::new("cue")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Stream");
                egui::ComboBox::from_id_salt("stream")
                    .selected_text(streams.get(self.stream).map_or("", |s| s.as_str()))
                    .show_ui(ui, |ui| {
                        for (i, name) in streams.iter().enumerate() {
                            done |= ui
                                .selectable_value(
                                    &mut self.stream,
                                    i,
                                    if name.is_empty() { "(unnamed)" } else { name },
                                )
                                .changed();
                        }
                    });
                ui.end_row();

                let mut text = |ui: &mut egui::Ui, label: &str, value: &mut String, ok: bool| {
                    ui.label(label);
                    let edit = egui::TextEdit::singleline(value).desired_width(f32::INFINITY);
                    let edit = if ok { edit } else { edit.text_color(red) };
                    done |= ui.add(edit).lost_focus();
                    ui.end_row();
                };
                let time_ok = parse_seconds(&self.time).is_some();
                text(ui, "Start (s)", &mut self.time, time_ok);
                let duration_ok = parse_seconds(&self.duration).is_some();
                text(ui, "Duration (s)", &mut self.duration, duration_ok);
                ui.label("End (s)");
                match (parse_seconds(&self.time), parse_seconds(&self.duration)) {
                    (Some(t), Some(d)) => ui.label(seconds(t.saturating_add(d))),
                    _ => ui.label("-"),
                };
                ui.end_row();
                let kind_ok = valid_text16(&self.kind);
                text(ui, "Type", &mut self.kind, kind_ok);

                ui.label("Position");
                ui.horizontal(|ui| {
                    let position = self.position();
                    let mut on = position.is_some();
                    if ui
                        .checkbox(&mut on, "On screen")
                        .on_hover_text("x and y fields in 0..1, from the top left of the video")
                        .changed()
                    {
                        self.set_position(on.then_some((0.5, 0.5)));
                        done = true;
                    }
                    if let Some((mut x, mut y)) = position {
                        let drag = |v| {
                            egui::DragValue::new(v)
                                .range(0.0..=1.0)
                                .speed(0.002)
                                .fixed_decimals(3)
                        };
                        let dx = ui.add(drag(&mut x).prefix("x "));
                        let dy = ui.add(drag(&mut y).prefix("y "));
                        if dx.changed() || dy.changed() {
                            self.set_position(Some((round3(x), round3(y))));
                        }
                        done |= dx.drag_stopped()
                            || dy.drag_stopped()
                            || dx.lost_focus()
                            || dy.lost_focus();
                    }
                    if ui
                        .selectable_label(*picking, "Pick")
                        .on_hover_text("Click the video to place the cue (Escape cancels)")
                        .clicked()
                    {
                        *picking = !*picking;
                    }
                });
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.strong("Fields");
            if ui.small_button("+ Add").clicked() {
                let mut n = self.fields.len() + 1;
                while self.fields.iter().any(|f| f.key == format!("key{n}")) {
                    n += 1;
                }
                self.fields.push(FieldDraft {
                    key: format!("key{n}"),
                    ty: ValueType::String,
                    value: String::new(),
                });
                done = true;
            }
        });
        let mut remove = None;
        let mut swap = None;
        let count = self.fields.len();
        for i in 0..count {
            let key_ok = self.key_ok(i);
            let f = &mut self.fields[i];
            let value_ok = parse_value(f.ty, &f.value).is_some();
            ui.push_id(i, |ui| {
                ui.horizontal(|ui| {
                    let key = egui::TextEdit::singleline(&mut f.key)
                        .desired_width(90.0)
                        .hint_text("key");
                    done |= ui
                        .add(if key_ok { key } else { key.text_color(red) })
                        .lost_focus();
                    egui::ComboBox::from_id_salt("type")
                        .width(64.0)
                        .selected_text(f.ty.name())
                        .show_ui(ui, |ui| {
                            for ty in ValueType::ALL {
                                done |= ui.selectable_value(&mut f.ty, ty, ty.name()).changed();
                            }
                        });
                    if f.ty == ValueType::Bool {
                        let mut b = f.value == "true";
                        if ui.checkbox(&mut b, "").changed() {
                            done = true;
                        }
                        f.value = b.to_string();
                    } else {
                        let value =
                            egui::TextEdit::singleline(&mut f.value).desired_width(f32::INFINITY);
                        done |= ui
                            .add(if value_ok {
                                value
                            } else {
                                value.text_color(red)
                            })
                            .lost_focus();
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    if ui
                        .add_enabled(i > 0, egui::Button::new("Up").small())
                        .clicked()
                    {
                        swap = Some(i - 1);
                    }
                    if ui
                        .add_enabled(i + 1 < count, egui::Button::new("Down").small())
                        .clicked()
                    {
                        swap = Some(i);
                    }
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = swap {
            self.fields.swap(i, i + 1);
            done = true;
        }
        if let Some(i) = remove {
            self.fields.remove(i);
            done = true;
        }
        if self.parse().is_none() {
            ui.add_space(4.0);
            ui.colored_label(red, "Fix the values in red to apply the edit.");
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue() -> EditCue {
        EditCue {
            uid: 7,
            time_us: 1_500_000,
            cue: Cue {
                duration: 333_333,
                kind: "subtitle".into(),
                fields: vec![
                    ("text".into(), Value::from("Hi")),
                    ("n".into(), Value::Int(-2)),
                    ("f".into(), Value::Float(3.0)),
                    ("b".into(), Value::Bool(true)),
                    ("raw".into(), Value::Bytes(vec![0, 255])),
                ],
            },
        }
    }

    #[test]
    fn drafts_roundtrip() {
        let d = CueDraft::new(1, &cue());
        assert_eq!((d.time.as_str(), d.duration.as_str()), ("1.5", "0.333333"));
        assert_eq!(d.fields[2].value, "3.0", "floats keep a fraction");
        assert_eq!(d.fields[4].value, "00ff");
        assert_eq!(d.parse(), Some((1, cue())));
    }

    #[test]
    fn invalid_drafts_do_not_parse() {
        let edits: [fn(&mut CueDraft); 8] = [
            |d| d.time = "-1".into(),
            |d| d.duration = "x".into(),
            |d| d.kind.clear(),
            |d| d.fields[1].key = "text".into(),
            |d| d.fields[0].key.clear(),
            |d| d.fields[1].value = "1.5".into(),
            |d| d.fields[2].value = "inf".into(),
            |d| d.fields[4].value = "abc".into(),
        ];
        for (i, edit) in edits.iter().enumerate() {
            let mut d = CueDraft::new(0, &cue());
            edit(&mut d);
            assert_eq!(d.parse(), None, "edit {i}");
        }
        let mut d = CueDraft::new(0, &cue());
        d.fields[1].ty = ValueType::Float;
        assert_eq!(
            d.parse().unwrap().1.cue.fields[1].1,
            Value::Float(-2.0),
            "ints read as floats"
        );
        assert!(
            valid_stream_name(&"n".repeat(32))
                && !valid_stream_name(&"n".repeat(33))
                && !valid_stream_name("a\0")
        );
    }

    #[test]
    fn drafts_edit_positions_like_cues() {
        let mut d = CueDraft::new(0, &cue());
        assert_eq!(d.position(), None);
        d.set_position(Some((0.3, 1.0)));
        assert_eq!(d.position(), Some((0.3, 1.0)));
        let (_, parsed) = d.parse().unwrap();
        let mut expected = cue();
        expected.cue.set_position(Some((0.3, 1.0)));
        assert_eq!(parsed, expected);
        d.fields[5].value = "1.5".into();
        assert_eq!(d.position(), None, "out of range");
        d.set_position(Some((0.25, 0.5)));
        assert_eq!(
            (d.fields.len(), d.position()),
            (7, Some((0.25, 0.5))),
            "replaced in place"
        );
        d.set_position(None);
        assert_eq!(d.parse(), Some((0, cue())));
        assert_eq!(round3(0.12345), 0.123);
    }
}
