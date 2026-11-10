//! ADPCM audio payloads (`docs/spec/PRAXI.md#adpcm-payload`).

/// Frames per block.
pub const BLOCK_FRAMES: usize = 16;
/// Bytes per block and channel: header and 16 residuals of 4 bits.
const BLOCK_BYTES: usize = 9;
const MAX_SHIFT: u8 = 12;

/// Second-order predictors `(c1, c2)` in units of `1/2048`.
pub const PREDICTORS: [(i32, i32); 16] = [
    (0, 0),
    (2048, 0),
    (-1024, 0),
    (4076, -2041),
    (3944, -1951),
    (3817, -1825),
    (3637, -1686),
    (3434, -1473),
    (3197, -1231),
    (2889, -962),
    (2870, -1719),
    (2513, -593),
    (2022, -1855),
    (2006, -99),
    (1545, 333),
    (497, -1376),
];

/// Bytes of a payload of `frames` frames.
pub fn payload_size(frames: u64, channels: usize) -> u64 {
    let ch = channels as u64;
    4 + 4 * ch + BLOCK_BYTES as u64 * ch * frames.div_ceil(BLOCK_FRAMES as u64)
}

/// The frame count of a payload, or `None` when its size does not match it.
pub fn frames(payload: &[u8], channels: usize) -> Option<u64> {
    let frames = u32::from_le_bytes(payload.get(..4)?.try_into().ok()?) as u64;
    (frames > 0 && payload_size(frames, channels) == payload.len() as u64).then_some(frames)
}

fn predict(predictor: usize, h1: i32, h2: i32) -> i32 {
    let (c1, c2) = PREDICTORS[predictor];
    (c1 * h1 + c2 * h2 + 1024) >> 11
}

/// Encodes consecutive chunks of one stream, carrying the decoded history across them.
#[derive(Debug, Clone)]
pub struct Encoder {
    /// `(h1, h2)` per channel.
    history: Vec<(i16, i16)>,
}

impl Encoder {
    pub fn new(channels: usize) -> Encoder {
        Encoder {
            history: vec![(0, 0); channels],
        }
    }

    /// Appends the payload of `samples`, interleaved whole frames, to `out`.
    pub fn encode(&mut self, samples: &[i16], out: &mut Vec<u8>) {
        let ch = self.history.len();
        let frames = samples.len() / ch;
        assert!(frames > 0 && frames <= u32::MAX as usize && samples.len() == frames * ch);
        out.extend_from_slice(&(frames as u32).to_le_bytes());
        for &(h1, h2) in &self.history {
            out.extend_from_slice(&h1.to_le_bytes());
            out.extend_from_slice(&h2.to_le_bytes());
        }
        let mut x = [0i32; BLOCK_FRAMES];
        for start in (0..frames).step_by(BLOCK_FRAMES) {
            let n = BLOCK_FRAMES.min(frames - start);
            for (c, history) in self.history.iter_mut().enumerate() {
                for (i, v) in x[..n].iter_mut().enumerate() {
                    *v = samples[(start + i) * ch + c] as i32;
                }
                let block = encode_block(&x[..n], history);
                out.extend_from_slice(&block);
            }
        }
    }
}

/// Picks the predictor and shift with the least squared error for one channel's block,
/// updating `history` to the decoded values.
fn encode_block(x: &[i32], history: &mut (i16, i16)) -> [u8; BLOCK_BYTES] {
    let mut best = (u64::MAX, [0u8; BLOCK_BYTES], *history);
    for predictor in 0..PREDICTORS.len() {
        for shift in 0..=MAX_SHIFT {
            let (mut h1, mut h2) = (history.0 as i32, history.1 as i32);
            let mut block = [0u8; BLOCK_BYTES];
            block[0] = (predictor as u8) << 4 | shift;
            let mut err = 0u64;
            for (i, &s) in x.iter().enumerate() {
                let p = predict(predictor, h1, h2);
                let r = ((s - p + (1 << shift >> 1)) >> shift).clamp(-8, 7);
                let y = (p + (r << shift)).clamp(-32768, 32767);
                err += (s - y).unsigned_abs() as u64 * (s - y).unsigned_abs() as u64;
                if err >= best.0 {
                    break;
                }
                block[1 + i / 2] |= ((r as u8) & 0xF) << (4 * (i % 2));
                (h2, h1) = (h1, y);
            }
            if err < best.0 {
                best = (err, block, (h1 as i16, h2 as i16));
            }
        }
    }
    *history = best.2;
    best.1
}

/// Decodes a payload of `channels` channels into `out`, as interleaved samples in
/// `[-1, 1]`.
pub fn decode(payload: &[u8], channels: usize, out: &mut Vec<f32>) -> Result<(), &'static str> {
    let frames = frames(payload, channels)
        .ok_or("ADPCM payload size does not match its frame count")? as usize;
    let ch = channels;
    out.clear();
    out.resize(frames * ch, 0.0);
    let (history, blocks) = payload[4..].split_at(4 * ch);
    for c in 0..ch {
        let mut h1 = i16::from_le_bytes([history[4 * c], history[4 * c + 1]]) as i32;
        let mut h2 = i16::from_le_bytes([history[4 * c + 2], history[4 * c + 3]]) as i32;
        for (b, start) in (0..frames).step_by(BLOCK_FRAMES).enumerate() {
            let block = &blocks[(b * ch + c) * BLOCK_BYTES..][..BLOCK_BYTES];
            let (predictor, shift) = ((block[0] >> 4) as usize, block[0] & 0xF);
            if shift > MAX_SHIFT {
                return Err("ADPCM block shift above 12");
            }
            let n = BLOCK_FRAMES.min(frames - start);
            for i in 0..n {
                // Sign-extend the nibble.
                let r = ((block[1 + i / 2] >> (4 * (i % 2))) << 4) as i8 as i32 >> 4;
                let s = (predict(predictor, h1, h2) + (r << shift)).clamp(-32768, 32767);
                out[(start + i) * ch + c] = s as f32 / 32768.0;
                (h2, h1) = (h1, s);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, channels: usize) -> Vec<i16> {
        (0..frames * channels)
            .map(|i| {
                let (f, c) = ((i / channels) as f64, (i % channels) as f64);
                (12000.0 * (f * 0.03 * (c + 1.0)).sin() + 3000.0 * (f * 0.31).sin()) as i16
            })
            .collect()
    }

    fn snr(a: &[i16], b: &[f32]) -> f64 {
        let (mut sig, mut noise) = (0.0, 0.0);
        for (&x, &y) in a.iter().zip(b) {
            let x = x as f64;
            sig += x * x;
            noise += (x - y as f64 * 32768.0).powi(2);
        }
        10.0 * (sig / noise.max(1e-9)).log10()
    }

    #[test]
    fn roundtrips_chunks_like_one_stream() {
        let samples = tone(1000, 3);
        let mut whole = Vec::new();
        Encoder::new(3).encode(&samples, &mut whole);
        assert_eq!(whole.len() as u64, payload_size(1000, 3));
        assert_eq!(frames(&whole, 3), Some(1000));
        let mut decoded = Vec::new();
        decode(&whole, 3, &mut decoded).unwrap();
        assert_eq!(decoded.len(), 3000);
        assert!(
            snr(&samples, &decoded) > 40.0,
            "snr {}",
            snr(&samples, &decoded)
        );

        // Chunks on block boundaries decode to exactly the same samples.
        let mut enc = Encoder::new(3);
        let mut chunked = Vec::new();
        for part in samples.chunks(3 * 336) {
            let mut payload = Vec::new();
            enc.encode(part, &mut payload);
            let mut out = Vec::new();
            decode(&payload, 3, &mut out).unwrap();
            chunked.extend(out);
        }
        assert_eq!(chunked, decoded);

        // Chunks of any size start from the decoded end of the previous one.
        let mut enc = Encoder::new(3);
        let mut chunked: Vec<f32> = Vec::new();
        for part in samples.chunks(3 * 333) {
            let mut payload = Vec::new();
            enc.encode(part, &mut payload);
            let history: Vec<i16> = payload[4..16]
                .chunks(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
            let expected: Vec<i16> = match chunked.len() {
                0 => vec![0; 6],
                n => (0..3)
                    .flat_map(|c| [chunked[n - 3 + c], chunked[n - 6 + c]])
                    .map(|v| (v * 32768.0) as i16)
                    .collect(),
            };
            assert_eq!(history, expected);
            let mut out = Vec::new();
            decode(&payload, 3, &mut out).unwrap();
            chunked.extend(out);
        }
        assert!(snr(&samples, &chunked) > 40.0);
    }

    #[test]
    fn handles_extremes() {
        let samples: Vec<i16> = (0..64)
            .map(|i| if i % 2 == 0 { i16::MAX } else { i16::MIN })
            .collect();
        let mut payload = Vec::new();
        Encoder::new(1).encode(&samples, &mut payload);
        let mut out = Vec::new();
        decode(&payload, 1, &mut out).unwrap();
        assert!(out.iter().all(|s| (-1.0..=1.0).contains(s)));
        assert!(snr(&samples, &out) > 20.0);
    }

    #[test]
    fn rejects_bad_payloads() {
        let mut payload = Vec::new();
        Encoder::new(2).encode(&tone(20, 2), &mut payload);
        let mut out = Vec::new();
        assert!(
            decode(&payload[..payload.len() - 1], 2, &mut out).is_err(),
            "short"
        );
        assert!(
            decode(&payload, 1, &mut out).is_err(),
            "wrong channel count"
        );
        let mut zero = payload.clone();
        zero[..4].copy_from_slice(&0u32.to_le_bytes());
        assert!(decode(&zero, 2, &mut out).is_err(), "no frames");
        let mut shift = payload.clone();
        shift[4 + 8] = 0x0D;
        assert!(decode(&shift, 2, &mut out).is_err(), "shift 13");
    }
}
