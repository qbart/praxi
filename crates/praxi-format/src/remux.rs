//! Rewriting the cue streams of a file (`docs/features/cue-editing.md`).

use crate::{Cue, Error, Reader, Result, StreamInfo, Writer};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::Path;

/// A cue and its start time.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedCue {
    pub time_us: u64,
    /// The cue, with `duration` in microseconds.
    pub cue: Cue,
}

/// The cues of one cue stream.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CueTrack {
    pub name: String,
    pub cues: Vec<TimedCue>,
}

/// Reads the cues of every cue stream, in stream order and file order, with times in
/// microseconds (rounded down).
pub fn read_cues<R: Read + Seek>(reader: &mut Reader<R>) -> Result<Vec<CueTrack>> {
    let streams = reader.streams().to_vec();
    let mut tracks: Vec<Option<CueTrack>> = streams
        .iter()
        .map(|s| {
            s.cue_params().map(|c| CueTrack {
                name: c.name.clone(),
                cues: Vec::new(),
            })
        })
        .collect();
    let mut payload = Vec::new();
    for entry in reader.read_index()? {
        let Some(track) = &mut tracks[entry.stream as usize] else {
            continue;
        };
        let chunk = reader.chunk_at(entry.offset)?;
        reader.read_payload(&chunk, &mut payload)?;
        let mut cue = Cue::decode(&payload)?;
        let time_base = streams[entry.stream as usize].time_base;
        cue.duration = time_base.to_micros(cue.duration);
        track.cues.push(TimedCue {
            time_us: time_base.to_micros(chunk.pts),
            cue,
        });
    }
    Ok(tracks.into_iter().flatten().collect())
}

/// Writes the file of `reader` to `out` with its cue streams replaced by `tracks`. Other
/// streams keep their order and their chunks are copied in stored form; the cue streams
/// follow them. The duration becomes the larger of the old one and the latest cue end.
pub fn replace_cues<R: Read + Seek, W: Write + Seek>(
    mut reader: Reader<R>,
    out: W,
    tracks: &[CueTrack],
) -> Result<W> {
    let input = reader.streams().to_vec();
    // New stream number of each kept input stream.
    let mut renumber = vec![None; input.len()];
    let mut streams = Vec::new();
    for (i, s) in input.iter().enumerate() {
        if s.cue_params().is_none() {
            renumber[i] = Some(streams.len() as u16);
            streams.push(StreamInfo {
                chunk_count: 0,
                ..s.clone()
            });
        }
    }
    let first_cue_stream = streams.len();
    streams.extend(tracks.iter().map(|t| StreamInfo::cues(t.name.clone())));
    let mut w = Writer::new(out, streams)?;

    // Every cue in time order; the sort is stable, so ties keep track and sheet order.
    let mut cues: Vec<(u16, &TimedCue)> = tracks
        .iter()
        .enumerate()
        .flat_map(|(t, track)| {
            track
                .cues
                .iter()
                .map(move |c| ((first_cue_stream + t) as u16, c))
        })
        .collect();
    cues.sort_by_key(|(_, c)| c.time_us);
    let mut duration_us = reader.header().duration_us;
    for (_, c) in &cues {
        duration_us = duration_us.max(c.time_us.saturating_add(c.cue.duration));
    }

    let mut next = cues.iter().peekable();
    let mut stored = Vec::new();
    reader.seek_to(reader.data_start())?;
    while let Some(mut chunk) = reader.next_chunk()? {
        let Some(stream) = renumber[chunk.stream as usize] else {
            continue;
        };
        let time_us = input[chunk.stream as usize].decode_time_us(chunk.pts);
        while let Some((s, c)) = next.next_if(|(_, c)| c.time_us <= time_us) {
            w.write_cue(*s, c.time_us, &c.cue)?;
        }
        reader.read_stored(&chunk, &mut stored)?;
        chunk.stream = stream;
        w.copy_chunk(&chunk, &stored)?;
    }
    for (s, c) in next {
        w.write_cue(*s, c.time_us, &c.cue)?;
    }
    w.finish(duration_us)
}

/// [`replace_cues`] from the file `input` to `output` through a temporary file next to
/// `output`, renamed over it when complete; `output` may be `input`. The temporary file is
/// removed on failure.
pub fn replace_cues_in_file(input: &Path, output: &Path, tracks: &[CueTrack]) -> Result<()> {
    let name = output
        .file_name()
        .ok_or_else(|| Error::Invalid("output has no file name".into()))?
        .to_string_lossy();
    let temp = output.with_file_name(format!(".{name}.praxi-tmp"));
    let result = (|| -> Result<()> {
        let reader = Reader::new(BufReader::new(File::open(input)?))?;
        let out = replace_cues(reader, BufWriter::new(File::create(&temp)?), tracks)?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
        return result;
    }
    std::fs::rename(&temp, output)?;
    Ok(())
}
