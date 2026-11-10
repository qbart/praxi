//! Peaks of an audio stream for the timeline.

use praxi::format::{AudioDecoder, Reader};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Width of one peak bucket.
pub const BUCKET_US: u64 = 5_000;

/// Minimum and maximum sample of every channel per bucket.
pub struct Waveform {
    peaks: Vec<(f32, f32)>,
    /// Largest absolute sample, at least 1/64, so quiet audio still shows.
    pub peak: f32,
}

impl Waveform {
    /// Decodes every chunk of audio stream `stream` of `path`.
    pub fn load(path: &Path, stream: u16) -> Result<Waveform, String> {
        let mut reader = Reader::new(BufReader::new(File::open(path).map_err(|e| e.to_string())?))
            .map_err(|e| e.to_string())?;
        let info = reader.streams()[stream as usize].clone();
        let params = info.audio_params().ok_or("not an audio stream")?.clone();
        let channels = params.channels as usize;
        let mut decoder = AudioDecoder::new(&params)?;
        let (mut payload, mut samples) = (Vec::new(), Vec::new());
        let mut peaks: Vec<(f32, f32)> = Vec::new();
        let index = reader.read_index().map_err(|e| e.to_string())?;
        for entry in index.iter().filter(|e| e.stream == stream) {
            let chunk = reader.chunk_at(entry.offset).map_err(|e| e.to_string())?;
            reader
                .read_payload(&chunk, &mut payload)
                .map_err(|e| e.to_string())?;
            decoder.decode(chunk.pts, &payload, &mut samples)?;
            let start_us = info.time_base.to_micros(chunk.pts);
            for (i, frame) in samples.chunks_exact(channels).enumerate() {
                let t = start_us + i as u64 * 1_000_000 / params.sample_rate as u64;
                let bucket = (t / BUCKET_US) as usize;
                if bucket >= peaks.len() {
                    peaks.resize(bucket + 1, (0.0, 0.0));
                }
                let p = &mut peaks[bucket];
                for &s in frame {
                    p.0 = p.0.min(s);
                    p.1 = p.1.max(s);
                }
            }
        }
        let peak = peaks
            .iter()
            .fold(1.0f32 / 64.0, |m, p| m.max(-p.0).max(p.1));
        Ok(Waveform { peaks, peak })
    }

    /// Minimum and maximum over `[from_us, to_us)`, or `None` without audio there.
    pub fn range(&self, from_us: u64, to_us: u64) -> Option<(f32, f32)> {
        let a = (from_us / BUCKET_US) as usize;
        let b = (to_us.div_ceil(BUCKET_US) as usize)
            .max(a + 1)
            .min(self.peaks.len());
        self.peaks.get(a..b).filter(|p| !p.is_empty()).map(|p| {
            p.iter()
                .fold((0.0f32, 0.0f32), |acc, x| (acc.0.min(x.0), acc.1.max(x.1)))
        })
    }
}
