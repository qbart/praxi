//! The edited cue streams, with undo (`docs/features/cue-editor.md#editing-model`).

use praxi::format::Cue;
use praxi::{CueTrack, TimedCue};

/// Undo steps kept.
const MAX_UNDO: usize = 200;

/// A cue with an id that survives sorting, undo and moving between streams.
#[derive(Debug, Clone, PartialEq)]
pub struct EditCue {
    pub uid: u64,
    pub time_us: u64,
    pub cue: Cue,
}

impl EditCue {
    pub fn end_us(&self) -> u64 {
        self.time_us.saturating_add(self.cue.duration)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub uid: u64,
    pub name: String,
    pub cues: Vec<EditCue>,
}

pub struct Document {
    streams: Vec<Stream>,
    undo: Vec<Vec<Stream>>,
    redo: Vec<Vec<Stream>>,
    /// The tracks as last saved or written.
    saved: Vec<CueTrack>,
    next_uid: u64,
}

impl Document {
    /// A document holding `tracks`, which count as saved.
    pub fn new(tracks: Vec<CueTrack>) -> Document {
        let mut doc = Document { streams: Vec::new(), undo: Vec::new(), redo: Vec::new(), saved: Vec::new(), next_uid: 0 };
        let mut streams = Vec::new();
        for t in &tracks {
            let cues = t.cues.iter().map(|c| EditCue { uid: doc.new_uid(), time_us: c.time_us, cue: c.cue.clone() }).collect();
            streams.push(Stream { uid: doc.new_uid(), name: t.name.clone(), cues });
        }
        sort(&mut streams);
        doc.streams = streams;
        doc.saved = doc.tracks();
        doc
    }

    pub fn streams(&self) -> &[Stream] {
        &self.streams
    }

    /// The streams as cue tracks, cues in time order.
    pub fn tracks(&self) -> Vec<CueTrack> {
        self.streams
            .iter()
            .map(|s| CueTrack {
                name: s.name.clone(),
                cues: s.cues.iter().map(|c| TimedCue { time_us: c.time_us, cue: c.cue.clone() }).collect(),
            })
            .collect()
    }

    pub fn new_uid(&mut self) -> u64 {
        self.next_uid += 1;
        self.next_uid
    }

    /// Whether the streams differ from the last saved or written tracks.
    pub fn is_dirty(&self) -> bool {
        self.streams.len() != self.saved.len()
            || self.streams.iter().zip(&self.saved).any(|(s, t)| {
                s.name != t.name
                    || s.cues.len() != t.cues.len()
                    || s.cues.iter().zip(&t.cues).any(|(a, b)| a.time_us != b.time_us || a.cue != b.cue)
            })
    }

    pub fn mark_saved(&mut self) {
        self.saved = self.tracks();
    }

    /// Applies `f` as one undo step and sorts cues by time. Returns false, adding no step,
    /// when nothing changed.
    pub fn edit(&mut self, f: impl FnOnce(&mut Vec<Stream>)) -> bool {
        let before = self.streams.clone();
        f(&mut self.streams);
        sort(&mut self.streams);
        if self.streams == before {
            return false;
        }
        self.undo.push(before);
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.streams, prev));
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.streams, next));
        true
    }

    /// Stream and cue index of the cue `uid`.
    pub fn find(&self, uid: u64) -> Option<(usize, usize)> {
        self.streams.iter().enumerate().find_map(|(s, stream)| stream.cues.iter().position(|c| c.uid == uid).map(|c| (s, c)))
    }

    pub fn stream_index(&self, uid: u64) -> Option<usize> {
        self.streams.iter().position(|s| s.uid == uid)
    }
}

/// Sorts every stream's cues by start time, keeping the order of equal times.
fn sort(streams: &mut [Stream]) {
    for s in streams {
        s.cues.sort_by_key(|c| c.time_us);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(times: &[u64]) -> CueTrack {
        CueTrack { name: "s".into(), cues: times.iter().map(|&t| TimedCue { time_us: t, cue: Cue::new(format!("c{t}")) }).collect() }
    }

    #[test]
    fn edits_sort_and_undo() {
        let mut doc = Document::new(vec![track(&[300, 100])]);
        assert_eq!(doc.tracks(), [track(&[100, 300])], "cues are sorted on load");
        assert!(!doc.is_dirty());
        let uid = doc.streams()[0].cues[0].uid;

        assert!(doc.edit(|s| s[0].cues[0].time_us = 500));
        assert_eq!(doc.find(uid), Some((0, 1)), "the moved cue is found after sorting");
        assert!(doc.is_dirty());
        assert!(!doc.edit(|_| {}), "no-op edits add no undo step");

        assert!(doc.undo());
        assert!(!doc.is_dirty());
        assert_eq!(doc.find(uid), Some((0, 0)));
        assert!(doc.redo());
        assert_eq!(doc.streams()[0].cues[1].time_us, 500);
        assert!(!doc.redo());

        doc.mark_saved();
        assert!(!doc.is_dirty());
        doc.undo();
        assert!(doc.is_dirty(), "undoing past the save makes the document dirty");
    }

    #[test]
    fn keeps_a_bounded_history() {
        let mut doc = Document::new(vec![track(&[0])]);
        for i in 1..=MAX_UNDO as u64 + 10 {
            doc.edit(|s| s[0].cues[0].time_us = i);
        }
        let mut undone = 0;
        while doc.undo() {
            undone += 1;
        }
        assert_eq!(undone, MAX_UNDO);
        assert_eq!(doc.streams()[0].cues[0].time_us, 10);
        doc.edit(|s| s[0].name = "new".into());
        assert!(!doc.can_redo(), "a new edit clears redo");
    }
}
