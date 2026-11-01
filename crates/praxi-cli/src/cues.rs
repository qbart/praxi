//! `praxi cues`: cue sheets (`docs/features/cue-editing.md`, `docs/spec/CUESHEET.md`).

use crate::AnyResult;
use praxi_format::{Cue, CueTrack, Reader, TimedCue, Value, cue_sheet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
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
