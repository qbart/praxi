//! Opus payloads (`docs/spec/PRAXI.md#opus-payload`), decoded with `opus1`.

use crate::AudioParams;
use opus1::SampleRate;
use opus1::multistream::MultistreamDecoder;

/// Bytes before the packets: `frames`, `skip`, `packet_count`.
pub const HEADER_SIZE: usize = 8;
/// Decoding starts at least this long before a seek target. RFC 7845 (section 4.6) asks for
/// 80 ms; on steady tones the error left by a fresh decoder falls by about 3 dB per 10 ms,
/// so 200 ms puts the target 50 dB or more below the signal.
pub const PREROLL_US: u64 = 200_000;
/// The longest Opus packet, 120 ms, in frames at `rate`.
fn max_packet_frames(rate: u32) -> usize {
    rate as usize * 120 / 1000
}

/// The `opus1` rate for `rate`, if Opus codes at it.
pub fn sample_rate(rate: u32) -> Option<SampleRate> {
    Some(match rate {
        8000 => SampleRate::Hz8000,
        12000 => SampleRate::Hz12000,
        16000 => SampleRate::Hz16000,
        24000 => SampleRate::Hz24000,
        48000 => SampleRate::Hz48000,
        _ => return None,
    })
}

/// Speaker pairs coded as stereo streams, in stream order.
const PAIRS: [(u32, u32); 6] = [(0x1, 0x2), (0x10, 0x20), (0x40, 0x80), (0x200, 0x400), (0x1000, 0x4000), (0x8000, 0x20000)];
const LFE: u32 = 0x8;

/// The multistream layout of a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub streams: usize,
    pub coupled: usize,
    /// Coded channel of each channel.
    pub mapping: Vec<u8>,
    /// The stream coding the low-frequency channel.
    pub lfe_stream: Option<usize>,
}

/// The layout `channels` and `channel_mask` give (`docs/spec/PRAXI.md#opus-payload`).
pub fn layout(channels: usize, mask: u32) -> Layout {
    // Channels are in increasing speaker bit order.
    let channel_of = |bit: u32| (mask & (bit - 1)).count_ones() as usize;
    let mut pairs = Vec::new();
    if mask != 0 {
        pairs.extend(PAIRS.iter().filter(|(l, r)| mask & l != 0 && mask & r != 0).map(|&(l, r)| (channel_of(l), channel_of(r))));
    } else if channels == 2 {
        pairs.push((0, 1));
    }
    let coupled = pairs.len();
    let mut mapping = vec![u8::MAX; channels];
    for (s, &(l, r)) in pairs.iter().enumerate() {
        mapping[l] = (2 * s) as u8;
        mapping[r] = (2 * s + 1) as u8;
    }
    let mut mono = 0;
    let mut lfe_stream = None;
    for (c, coded) in mapping.iter_mut().enumerate() {
        if *coded == u8::MAX {
            *coded = (2 * coupled + mono) as u8;
            if mask & LFE != 0 && c == channel_of(LFE) {
                lfe_stream = Some(coupled + mono);
            }
            mono += 1;
        }
    }
    Layout { streams: coupled + mono, coupled, mapping, lfe_stream }
}

/// `frames`, `skip` and the packets of a payload, if it is well formed.
fn parse(payload: &[u8]) -> Option<(u32, u16, impl Iterator<Item = &[u8]>)> {
    let header = payload.get(..HEADER_SIZE)?;
    let frames = u32::from_le_bytes(header[0..4].try_into().expect("4 bytes"));
    let skip = u16::from_le_bytes(header[4..6].try_into().expect("2 bytes"));
    let count = u16::from_le_bytes(header[6..8].try_into().expect("2 bytes"));
    let mut rest = &payload[HEADER_SIZE..];
    for _ in 0..count {
        let len = u16::from_le_bytes(rest.get(..2)?.try_into().expect("2 bytes")) as usize;
        if len == 0 {
            return None;
        }
        rest = rest.get(2 + len..)?;
    }
    if frames == 0 || count == 0 || !rest.is_empty() {
        return None;
    }
    let mut rest = &payload[HEADER_SIZE..];
    let packets = std::iter::from_fn(move || {
        let len = u16::from_le_bytes(rest.get(..2)?.try_into().expect("2 bytes")) as usize;
        let packet = &rest[2..2 + len];
        rest = &rest[2 + len..];
        Some(packet)
    });
    Some((frames, skip, packets))
}

/// The frame count of a payload, if it is well formed.
pub fn frames(payload: &[u8]) -> Option<u64> {
    parse(payload).map(|(frames, _, _)| frames as u64)
}

/// Writes a payload of `packets`.
pub fn write_payload(frames: u32, skip: u16, packets: &[Vec<u8>], out: &mut Vec<u8>) {
    out.extend_from_slice(&frames.to_le_bytes());
    out.extend_from_slice(&skip.to_le_bytes());
    out.extend_from_slice(&(packets.len() as u16).to_le_bytes());
    for p in packets {
        out.extend_from_slice(&(p.len() as u16).to_le_bytes());
        out.extend_from_slice(p);
    }
}

/// Decodes consecutive chunks of one Opus stream.
pub struct Decoder {
    inner: MultistreamDecoder,
    channels: usize,
    max_frames: usize,
    /// The pts that continues this decoder's state; `None` while fresh.
    next_pts: Option<u64>,
    decoded: Vec<f32>,
}

impl Decoder {
    pub fn new(params: &AudioParams) -> Result<Decoder, String> {
        let rate = sample_rate(params.sample_rate).ok_or_else(|| format!("{} Hz is not an Opus rate", params.sample_rate))?;
        let channels = params.channels as usize;
        let l = layout(channels, params.channel_mask);
        let inner = MultistreamDecoder::new(rate, channels, l.streams, l.coupled, &l.mapping).map_err(|e| format!("opus layout: {e}"))?;
        Ok(Decoder { inner, channels, max_frames: max_packet_frames(params.sample_rate), next_pts: None, decoded: Vec::new() })
    }

    /// The pts of the chunk that continues this decoder's state, `None` while fresh.
    pub fn next_pts(&self) -> Option<u64> {
        self.next_pts
    }

    /// Makes the decoder fresh, as for the stream's first chunk.
    pub fn reset(&mut self) {
        self.inner.reset_state();
        self.next_pts = None;
    }

    /// Decodes the chunk at `pts` into `out`: its frames, interleaved.
    pub fn decode(&mut self, pts: u64, payload: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        let (frames, skip, packets) = parse(payload).ok_or_else(|| format!("{} bytes is not a valid opus payload", payload.len()))?;
        let ch = self.channels;
        self.decoded.clear();
        for packet in packets {
            let start = self.decoded.len();
            self.decoded.resize(start + self.max_frames * ch, 0.0);
            match self.inner.decode(packet, self.max_frames, &mut self.decoded[start..]) {
                Ok(n) => self.decoded.truncate(start + n * ch),
                Err(e) => {
                    self.reset();
                    return Err(format!("opus packet: {e}"));
                }
            }
        }
        let (skip, frames) = (skip as usize, frames as usize);
        if (skip + frames) * ch > self.decoded.len() {
            self.reset();
            return Err(format!("opus chunk keeps {} frames after {skip} but decodes {}", frames, self.decoded.len() / ch));
        }
        out.clear();
        out.extend_from_slice(&self.decoded[skip * ch..(skip + frames) * ch]);
        self.next_pts = Some(pts + frames as u64);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_follow_the_spec() {
        let mono = layout(1, 0x4);
        assert_eq!((mono.streams, mono.coupled, mono.mapping), (1, 0, vec![0]));
        assert_eq!(layout(2, 0x3).mapping, [0, 1]);
        assert_eq!((layout(2, 0).coupled, layout(3, 0).coupled), (1, 0));
        // 5.1 (side): FL FR FC LFE SL SR.
        let l = layout(6, 0x60f);
        assert_eq!((l.streams, l.coupled, l.lfe_stream), (4, 2, Some(3)));
        assert_eq!(l.mapping, [0, 1, 4, 5, 2, 3]);
        // 7.1: FL FR FC LFE BL BR SL SR.
        let l = layout(8, 0x63f);
        assert_eq!((l.streams, l.coupled), (5, 3));
        assert_eq!(l.mapping, [0, 1, 6, 7, 2, 3, 4, 5]);
    }

    #[test]
    fn payloads_parse_strictly() {
        let mut p = Vec::new();
        write_payload(960, 312, &[vec![1, 2, 3], vec![4]], &mut p);
        assert_eq!(p.len(), HEADER_SIZE + 2 + 3 + 2 + 1);
        assert_eq!(frames(&p), Some(960));
        let packets: Vec<&[u8]> = parse(&p).unwrap().2.collect();
        assert_eq!(packets, [&[1u8, 2, 3][..], &[4][..]]);
        assert_eq!(frames(&p[..p.len() - 1]), None, "truncated");
        let mut extra = p.clone();
        extra.push(0);
        assert_eq!(frames(&extra), None, "trailing bytes");
        let mut empty = Vec::new();
        write_payload(10, 0, &[], &mut empty);
        assert_eq!(frames(&empty), None, "no packets");
        let mut zero = Vec::new();
        write_payload(0, 0, &[vec![1]], &mut zero);
        assert_eq!(frames(&zero), None, "no frames");
    }
}
