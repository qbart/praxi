//! `praxi cues`: cue sheets (`docs/features/cue-editing.md`, `docs/spec/CUESHEET.md`).

use crate::AnyResult;
use praxi_encoder::CueConfig;
use praxi_format::{Cue, CueTrack, Reader, TimedCue, Value, cue_sheet};
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

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
    /// Add cues to a .praxi file, without a cue sheet.
    Add {
        file: PathBuf,
        /// Cue as TIME[+DURATION]=TYPE[:KEY=VALUE[;KEY=VALUE...]], times in seconds (repeatable).
        #[arg(long = "cue", value_parser = crate::parse_cue, required = true)]
        cues: Vec<CueConfig>,
        /// Name of the cue stream.
        #[arg(long, default_value = "cues")]
        stream: String,
        /// Output file (default: the input file).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Remove the cues of a .praxi file matching every filter given.
    Remove {
        file: PathBuf,
        /// Only cues of streams with this name.
        #[arg(long)]
        stream: Option<String>,
        /// Only cues of this type.
        #[arg(long = "type")]
        kind: Option<String>,
        /// Only cues starting at this time or later, in seconds.
        #[arg(long)]
        from: Option<f64>,
        /// Only cues starting before this time, in seconds.
        #[arg(long)]
        to: Option<f64>,
        /// Output file (default: the input file).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

/// Which cues `cues remove` removes.
#[derive(Default)]
pub struct Filter {
    pub stream: Option<String>,
    pub kind: Option<String>,
    pub from_us: Option<u64>,
    pub to_us: Option<u64>,
}

impl Filter {
    fn matches(&self, stream: &str, cue: &TimedCue) -> bool {
        self.stream.as_ref().is_none_or(|s| s == stream)
            && self.kind.as_ref().is_none_or(|k| *k == cue.cue.kind)
            && self.from_us.is_none_or(|t| cue.time_us >= t)
            && self.to_us.is_none_or(|t| cue.time_us < t)
    }
}

/// Adds cues to the first stream named `stream`, or to a new stream after the others.
pub fn add_cues(tracks: &mut Vec<CueTrack>, stream: &str, cues: &[CueConfig]) -> Result<(), String> {
    let mut scratch = Vec::new();
    let mut timed = Vec::with_capacity(cues.len());
    for c in cues {
        let micros = |secs: f64, what: &str| {
            if secs.is_finite() && secs >= 0.0 {
                Ok((secs * 1e6).round() as u64)
            } else {
                Err(format!("cue {:?} has invalid {what} {secs}", c.kind))
            }
        };
        let cue = Cue { duration: micros(c.duration, "duration")?, kind: c.kind.clone(), fields: c.fields.clone() };
        scratch.clear();
        cue.encode(&mut scratch).map_err(|e| format!("cue {:?}: {e}", c.kind))?;
        timed.push(TimedCue { time_us: micros(c.time, "time")?, cue });
    }
    match tracks.iter_mut().find(|t| t.name == stream) {
        Some(track) => track.cues.extend(timed),
        None => tracks.push(CueTrack { name: stream.to_string(), cues: timed }),
    }
    Ok(())
}

/// Removes the cues matching `filter` and the streams left without cues; returns how many
/// cues were removed.
pub fn remove_cues(tracks: &mut Vec<CueTrack>, filter: &Filter) -> usize {
    let mut removed = 0;
    for track in tracks.iter_mut() {
        let before = track.cues.len();
        track.cues.retain(|c| !filter.matches(&track.name, c));
        removed += before - track.cues.len();
    }
    tracks.retain(|t| !t.cues.is_empty());
    removed
}

fn seconds_option(secs: Option<f64>, what: &str) -> AnyResult<Option<u64>> {
    match secs {
        Some(s) if !s.is_finite() || s < 0.0 => Err(format!("invalid --{what} {s}").into()),
        s => Ok(s.map(|s| (s * 1e6).round() as u64)),
    }
}

fn read_tracks(file: &Path) -> AnyResult<Vec<CueTrack>> {
    let mut reader = Reader::new(BufReader::new(File::open(file).map_err(|e| format!("{}: {e}", file.display()))?))?;
    Ok(praxi_format::read_cues(&mut reader)?)
}

pub fn run(cmd: CuesCmd) -> AnyResult<()> {
    match cmd {
        CuesCmd::Export { file, output } => {
            let mut reader = Reader::new(BufReader::new(File::open(&file)?))?;
            let tracks = praxi_format::read_cues(&mut reader)?;
            write_sheet(&tracks, output.as_deref())
        }
        CuesCmd::Import { file, sheets, output } => {
            let mut tracks = Vec::new();
            for path in &sheets {
                let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
                tracks.extend(cue_sheet::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?);
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
            write_sheet(&[CueTrack { name: stream, cues }], output.as_deref())
        }
        CuesCmd::Add { file, cues, stream, output } => {
            let mut tracks = read_tracks(&file)?;
            add_cues(&mut tracks, &stream, &cues)?;
            eprintln!("added {} cues to stream {stream:?}", cues.len());
            import(&file, &tracks, output.as_deref().unwrap_or(&file))
        }
        CuesCmd::Remove { file, stream, kind, from, to, output } => {
            if stream.is_none() && kind.is_none() && from.is_none() && to.is_none() {
                return Err("give at least one of --stream, --type, --from, --to".into());
            }
            let filter = Filter { stream, kind, from_us: seconds_option(from, "from")?, to_us: seconds_option(to, "to")? };
            let mut tracks = read_tracks(&file)?;
            let removed = remove_cues(&mut tracks, &filter);
            eprintln!("removed {removed} cues");
            import(&file, &tracks, output.as_deref().unwrap_or(&file))
        }
    }
}

fn parse_field(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.to_string(), v.to_string())),
        _ => Err(format!("expected KEY=VALUE, got {s:?}")),
    }
}

fn import(input: &Path, tracks: &[CueTrack], output: &Path) -> AnyResult<()> {
    praxi_format::replace_cues_in_file(input, output, tracks)?;
    let count: usize = tracks.iter().map(|t| t.cues.len()).sum();
    eprintln!("{}: {} cue streams, {count} cues", output.display(), tracks.len());
    Ok(())
}

fn write_sheet(tracks: &[CueTrack], output: Option<&Path>) -> AnyResult<()> {
    let text = cue_sheet::write(tracks)?;
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
    fn adds_and_removes_cues() {
        let mut tracks = vec![CueTrack { name: "cues".into(), cues: vec![TimedCue { time_us: 0, cue: Cue::new("a") }] }];
        let added = [crate::parse_cue("1.5+0.25=boom:power=2").unwrap(), crate::parse_cue("3=b").unwrap()];
        add_cues(&mut tracks, "cues", &added).unwrap();
        add_cues(&mut tracks, "gameplay", &added[1..]).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(
            tracks[0].cues[1],
            TimedCue {
                time_us: 1_500_000,
                cue: Cue { duration: 250_000, kind: "boom".into(), fields: vec![("power".into(), Value::String("2".into()))] }
            }
        );
        assert!(add_cues(&mut tracks, "cues", &[crate::parse_cue("1=x:k=1;k=2").unwrap()]).is_err(), "duplicate key");
        assert!(add_cues(&mut tracks, "cues", &[crate::parse_cue("-1=x").unwrap()]).is_err(), "negative time");
        assert_eq!(tracks[0].cues.len(), 3, "nothing added on errors");

        let only_b = Filter { kind: Some("b".into()), from_us: Some(3_000_000), ..Filter::default() };
        assert_eq!(remove_cues(&mut tracks, &only_b), 2);
        assert_eq!(tracks.len(), 1, "empty stream removed");
        let early = Filter { stream: Some("cues".into()), to_us: Some(1_500_000), ..Filter::default() };
        assert_eq!(remove_cues(&mut tracks, &early), 1);
        assert_eq!(tracks[0].cues.iter().map(|c| c.cue.kind.as_str()).collect::<Vec<_>>(), ["boom"]);
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
