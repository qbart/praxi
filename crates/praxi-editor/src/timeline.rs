//! The timeline: ruler, waveform and one lane per cue stream
//! (`docs/features/cue-editor.md#timeline-input`).

use crate::doc::{EditCue, Stream};
use crate::waveform::Waveform;
use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Vec2};

const HEADER_W: f32 = 110.0;
const RULER_H: f32 = 22.0;
const WAVE_H: f32 = 56.0;
const LANE_H: f32 = 26.0;
const MIN_LANE_H: f32 = 16.0;
/// Width of the grip at the end of a bar that changes its duration.
const GRIP_W: f32 = 6.0;
const DIAMOND_R: f32 = 6.0;

pub enum Action {
    Seek(u64),
    SelectCue(u64),
    SelectStream(u64),
    /// A drag finished: the cue's new start and duration.
    Change {
        uid: u64,
        time_us: u64,
        duration: u64,
    },
    Add {
        stream: usize,
        time_us: u64,
    },
}

/// What the timeline shows.
pub struct View<'a> {
    pub streams: &'a [Stream],
    pub selected_cue: Option<u64>,
    pub selected_stream: Option<u64>,
    pub playhead: u64,
    pub duration: u64,
    pub waveform: Option<&'a Waveform>,
    /// Snaps a time to a video frame start, when snapping is on.
    pub snap: Option<&'a dyn Fn(u64) -> u64>,
    /// Keep the playhead in view (while playing).
    pub follow: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    Move,
    Resize,
}

struct Drag {
    uid: u64,
    mode: DragMode,
    orig: (u64, u64),
    grab_us: f64,
    /// Start and duration while dragging.
    now: (u64, u64),
}

pub struct Timeline {
    /// Time at the left edge of the lanes, in microseconds.
    start_us: f64,
    us_per_px: f64,
    drag: Option<Drag>,
    fitted: bool,
}

impl Default for Timeline {
    fn default() -> Timeline {
        Timeline {
            start_us: 0.0,
            us_per_px: 10_000.0,
            drag: None,
            fitted: false,
        }
    }
}

/// A lane colour per stream.
fn stream_color(i: usize) -> Color32 {
    egui::ecolor::Hsva::new((0.58 + i as f32 * 0.17) % 1.0, 0.55, 0.75, 1.0).into()
}

/// `m:ss.mmm`.
pub fn clock(us: u64) -> String {
    let ms = us / 1000;
    format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

/// The text a cue shows: its type and its first string field.
fn label(c: &EditCue) -> String {
    let text = c.cue.fields.iter().find_map(|(_, v)| match v {
        praxi::Value::String(s) if !s.is_empty() => Some(s.replace('\n', " ")),
        _ => None,
    });
    match text {
        Some(t) => format!("{}: {t}", c.cue.kind),
        None => c.cue.kind.clone(),
    }
}

impl Timeline {
    /// Shows the whole duration.
    pub fn fit(&mut self, width: f32, duration: u64) {
        self.start_us = 0.0;
        self.us_per_px =
            (duration.max(1_000_000) as f64 / (width - HEADER_W).max(100.0) as f64).max(10.0);
    }

    pub fn show(&mut self, ui: &mut egui::Ui, view: View) -> Vec<Action> {
        let mut actions = Vec::new();
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());
        if !self.fitted {
            self.fit(full.width(), view.duration);
            self.fitted = true;
        }
        let lanes_left = full.left() + HEADER_W;
        let area = Rect::from_min_max(Pos2::new(lanes_left, full.top()), full.max);
        let x_of = |t: f64, s: &Timeline| area.left() + ((t - s.start_us) / s.us_per_px) as f32;
        let t_of =
            |x: f32, s: &Timeline| (s.start_us + (x - area.left()) as f64 * s.us_per_px).max(0.0);

        // Zoom and scroll.
        if ui.rect_contains_pointer(full) {
            let (zoom, scroll, pointer) =
                ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
            if zoom != 1.0 {
                let anchor_x = pointer.map_or(area.center().x, |p| p.x.max(area.left()));
                let anchor_t = t_of(anchor_x, self);
                self.us_per_px = (self.us_per_px / zoom as f64).clamp(10.0, 1e6);
                self.start_us = anchor_t - (anchor_x - area.left()) as f64 * self.us_per_px;
            } else if scroll != Vec2::ZERO {
                self.start_us -= (scroll.x + scroll.y) as f64 * self.us_per_px;
            }
        }
        let width_us = area.width() as f64 * self.us_per_px;
        if view.follow && self.drag.is_none() {
            let p = view.playhead as f64;
            if p < self.start_us || p > self.start_us + width_us {
                self.start_us = p - width_us * 0.1;
            }
        }
        self.start_us = self.start_us.clamp(
            -width_us * 0.5,
            (view.duration as f64 - width_us * 0.5).max(0.0),
        );

        let painter = ui.painter_at(full);
        let visuals = ui.visuals().clone();
        let text_color = visuals.strong_text_color();
        let weak = visuals.weak_text_color();
        painter.rect_filled(full, 0.0, visuals.extreme_bg_color);
        let end_x = x_of(view.duration as f64, self);
        if end_x < area.right() {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(end_x.max(area.left()), full.top()), full.max),
                0.0,
                visuals.faint_bg_color,
            );
        }

        // Ruler.
        let ruler = Rect::from_min_size(area.min, Vec2::new(area.width(), RULER_H));
        let steps_s = [
            0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
            600.0,
        ];
        let step_us = steps_s
            .iter()
            .map(|s| s * 1e6)
            .find(|s| s / self.us_per_px >= 80.0)
            .unwrap_or(1.2e9);
        let mut t = (self.start_us.max(0.0) / step_us).floor() * step_us;
        while t <= self.start_us + width_us {
            let x = x_of(t, self);
            if x >= area.left() {
                painter.line_segment(
                    [
                        Pos2::new(x, ruler.bottom() - 6.0),
                        Pos2::new(x, full.bottom()),
                    ],
                    Stroke::new(1.0, visuals.faint_bg_color.gamma_multiply(2.0)),
                );
                painter.text(
                    Pos2::new(x + 3.0, ruler.top() + 3.0),
                    Align2::LEFT_TOP,
                    clock(t as u64),
                    FontId::monospace(11.0),
                    weak,
                );
            }
            t += step_us;
        }

        // Waveform.
        let wave = Rect::from_min_size(
            Pos2::new(area.left(), ruler.bottom()),
            Vec2::new(area.width(), WAVE_H),
        );
        painter.text(
            Pos2::new(full.left() + 6.0, wave.center().y),
            Align2::LEFT_CENTER,
            "audio",
            FontId::proportional(12.0),
            weak,
        );
        if let Some(w) = view.waveform {
            let mid = wave.center().y;
            // Scaled to the loudest peak.
            let half = (wave.height() / 2.0 - 2.0) / w.peak;
            let mut x = wave.left();
            while x < wave.right() {
                let (a, b) = (t_of(x, self) as u64, t_of(x + 1.0, self) as u64);
                if let Some((lo, hi)) = w.range(a, b) {
                    painter.line_segment(
                        [
                            Pos2::new(x, mid - hi * half),
                            Pos2::new(x, mid - lo * half + 1.0),
                        ],
                        Stroke::new(1.0, weak),
                    );
                }
                x += 1.0;
            }
        }
        let scrub = ui.interact(
            ruler.union(wave),
            ui.scope_id().with("scrub"),
            Sense::click_and_drag(),
        );
        if (scrub.clicked() || scrub.dragged())
            && let Some(p) = scrub.interact_pointer_pos()
        {
            actions.push(Action::Seek((t_of(p.x, self) as u64).min(view.duration)));
        }

        // Lanes.
        let lanes_top = wave.bottom() + 2.0;
        let lane_h = if view.streams.is_empty() {
            LANE_H
        } else {
            ((full.bottom() - lanes_top) / view.streams.len() as f32).clamp(MIN_LANE_H, LANE_H)
        };
        let alt = ui.input(|i| i.modifiers.alt);
        let shift = ui.input(|i| i.modifiers.shift);
        let snap = |t: u64| match view.snap {
            Some(f) if !alt => f(t),
            _ => t,
        };
        for (si, stream) in view.streams.iter().enumerate() {
            let top = lanes_top + si as f32 * lane_h;
            let lane = Rect::from_min_max(
                Pos2::new(area.left(), top),
                Pos2::new(area.right(), top + lane_h),
            );
            let header = Rect::from_min_max(
                Pos2::new(full.left(), top),
                Pos2::new(lanes_left, top + lane_h),
            );
            let color = stream_color(si);
            if view.selected_stream == Some(stream.uid) {
                painter.rect_filled(header.union(lane), 0.0, color.gamma_multiply(0.12));
            }
            painter.line_segment(
                [
                    Pos2::new(full.left(), lane.bottom()),
                    Pos2::new(full.right(), lane.bottom()),
                ],
                Stroke::new(1.0, visuals.faint_bg_color),
            );
            let name = if stream.name.is_empty() {
                "(unnamed)"
            } else {
                &stream.name
            };
            painter.with_clip_rect(header.shrink(2.0)).text(
                Pos2::new(header.left() + 6.0, header.center().y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(13.0),
                text_color,
            );
            if ui
                .interact(
                    header,
                    ui.scope_id().with(("header", stream.uid)),
                    Sense::click(),
                )
                .clicked()
            {
                actions.push(Action::SelectStream(stream.uid));
            }
            let bg = ui.interact(
                lane,
                ui.scope_id().with(("lane", stream.uid)),
                Sense::click(),
            );
            if bg.double_clicked() {
                if let Some(p) = bg.interact_pointer_pos() {
                    actions.push(Action::Add {
                        stream: si,
                        time_us: snap(t_of(p.x, self) as u64),
                    });
                }
            } else if bg.clicked() {
                actions.push(Action::SelectStream(stream.uid));
            }

            for c in &stream.cues {
                let (time_us, duration) = match &self.drag {
                    Some(d) if d.uid == c.uid => d.now,
                    _ => (c.time_us, c.cue.duration),
                };
                let x0 = x_of(time_us as f64, self);
                let x1 = x_of(time_us.saturating_add(duration) as f64, self);
                if x1 < area.left() - DIAMOND_R || x0 > area.right() + DIAMOND_R {
                    continue;
                }
                let selected = view.selected_cue == Some(c.uid);
                let stroke = if selected {
                    Stroke::new(2.0, text_color)
                } else {
                    Stroke::new(1.0, color.gamma_multiply(0.6))
                };
                let cy = lane.center().y;
                let (body, grip) = if duration > 0 {
                    let r = Rect::from_min_max(
                        Pos2::new(x0, lane.top() + 3.0),
                        Pos2::new(x1.max(x0 + GRIP_W), lane.bottom() - 3.0),
                    );
                    painter.rect(
                        r,
                        3.0,
                        color.gamma_multiply(0.55),
                        stroke,
                        egui::StrokeKind::Inside,
                    );
                    painter.with_clip_rect(r.intersect(area).shrink(2.0)).text(
                        Pos2::new(r.left().max(area.left()) + 4.0, cy),
                        Align2::LEFT_CENTER,
                        label(c),
                        FontId::proportional(12.0),
                        text_color,
                    );
                    (
                        r,
                        Some(Rect::from_min_max(
                            Pos2::new(r.right() - GRIP_W, r.top()),
                            r.max,
                        )),
                    )
                } else {
                    let r = Rect::from_center_size(Pos2::new(x0, cy), Vec2::splat(DIAMOND_R * 2.0));
                    let points = vec![
                        Pos2::new(x0, cy - DIAMOND_R),
                        Pos2::new(x0 + DIAMOND_R, cy),
                        Pos2::new(x0, cy + DIAMOND_R),
                        Pos2::new(x0 - DIAMOND_R, cy),
                    ];
                    painter.add(egui::Shape::convex_polygon(points, color, stroke));
                    painter.with_clip_rect(lane.intersect(area)).text(
                        Pos2::new(x0 + DIAMOND_R + 3.0, cy),
                        Align2::LEFT_CENTER,
                        label(c),
                        FontId::proportional(12.0),
                        weak,
                    );
                    (r, None)
                };
                let body_resp = ui.interact(
                    body,
                    ui.scope_id().with(("cue", c.uid)),
                    Sense::click_and_drag(),
                );
                let grip_resp = grip.map(|g| {
                    ui.interact(
                        g,
                        ui.scope_id().with(("grip", c.uid)),
                        Sense::click_and_drag(),
                    )
                });
                for (resp, mode) in [
                    (Some(body_resp), DragMode::Move),
                    (grip_resp, DragMode::Resize),
                ] {
                    let Some(resp) = resp else { continue };
                    let mode = if mode == DragMode::Move && duration == 0 && shift {
                        DragMode::Resize
                    } else {
                        mode
                    };
                    if mode == DragMode::Resize && (resp.hovered() || resp.dragged()) {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                    }
                    if resp.clicked() || resp.drag_started() {
                        actions.push(Action::SelectCue(c.uid));
                    }
                    if resp.drag_started()
                        && let Some(p) = resp.interact_pointer_pos()
                    {
                        let orig = (c.time_us, c.cue.duration);
                        self.drag = Some(Drag {
                            uid: c.uid,
                            mode,
                            orig,
                            grab_us: t_of(p.x, self),
                            now: orig,
                        });
                    }
                    if resp.dragged()
                        && let (Some(p), Some(d)) = (resp.interact_pointer_pos(), &self.drag)
                        && d.uid == c.uid
                    {
                        let delta = t_of(p.x, self) - d.grab_us;
                        let (t0, d0) = d.orig;
                        let now = match d.mode {
                            DragMode::Move => (snap((t0 as f64 + delta).max(0.0) as u64), d0),
                            DragMode::Resize => {
                                let end = snap(((t0 + d0) as f64 + delta).max(0.0) as u64);
                                (t0, end.saturating_sub(t0))
                            }
                        };
                        self.drag.as_mut().expect("drag").now = now;
                    }
                    if resp.drag_stopped()
                        && let Some(d) = self.drag.take_if(|d| d.uid == c.uid)
                    {
                        actions.push(Action::Change {
                            uid: c.uid,
                            time_us: d.now.0,
                            duration: d.now.1,
                        });
                    }
                }
            }
        }

        // Playhead.
        let px = x_of(view.playhead as f64, self);
        if px >= area.left() && px <= area.right() {
            let red = Color32::from_rgb(230, 70, 60);
            painter.line_segment(
                [Pos2::new(px, full.top()), Pos2::new(px, full.bottom())],
                Stroke::new(1.5, red),
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(px - 5.0, full.top()),
                    Pos2::new(px + 5.0, full.top()),
                    Pos2::new(px, full.top() + 7.0),
                ],
                red,
                Stroke::NONE,
            ));
        }
        actions
    }
}
