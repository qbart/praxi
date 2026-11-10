//! Opus encoding of audio tracks (`docs/features/audio.md#opus`).

use opus1::multistream::MultistreamEncoder;
use opus1::{Application, Bandwidth, Bitrate};
use praxi_format::{AudioParams, opus};
use std::num::NonZeroU32;

/// Packet length.
pub const PACKET_MS: u32 = 20;
/// Default bitrate per channel.
pub const DEFAULT_BITRATE_PER_CHANNEL: u32 = 64_000;
/// The largest packet Opus writes is 1275 bytes per stream.
const MAX_PACKET_PER_STREAM: usize = 1275;

/// Encodes one track into chunk payloads of whole packets.
pub struct Encoder {
    inner: MultistreamEncoder,
    channels: usize,
    /// Frames per packet.
    packet: usize,
    /// The encoder's delay (pre-skip): decoded frame `i` is input frame `i - lookahead`.
    lookahead: u64,
    /// Packets encoded so far.
    packets: u64,
    /// Input frames taken from the source so far.
    source_frames: u64,
    /// Input not yet encoded, interleaved.
    input: Vec<f32>,
    out: Vec<Vec<u8>>,
    scratch: Vec<u8>,
}

impl Encoder {
    /// `bitrate` covers the whole track, all channels.
    pub fn new(params: &AudioParams, bitrate: u32) -> Result<Encoder, String> {
        params.check_format()?;
        let rate = opus::sample_rate(params.sample_rate).expect("checked");
        let channels = params.channels as usize;
        let l = opus::layout(channels, params.channel_mask);
        let mut inner = MultistreamEncoder::new(
            rate,
            channels,
            l.streams,
            l.coupled,
            &l.mapping,
            Application::Audio,
        )
        .map_err(|e| format!("opus encoder: {e}"))?;
        let bitrate = NonZeroU32::new(bitrate).ok_or("opus bitrate must not be zero")?;
        inner.set_bitrate(Bitrate::BitsPerSecond(bitrate));
        inner
            .set_complexity(10)
            .map_err(|e| format!("opus encoder: {e}"))?;
        if let Some(s) = l.lfe_stream
            && let Some(e) = inner.stream_encoder_mut(s)
        {
            e.set_max_bandwidth(Bandwidth::Narrowband);
        }
        let lookahead = inner.lookahead() as u64;
        Ok(Encoder {
            inner,
            channels,
            packet: (params.sample_rate * PACKET_MS / 1000) as usize,
            lookahead,
            packets: 0,
            source_frames: 0,
            input: Vec::new(),
            out: Vec::new(),
            scratch: vec![0; MAX_PACKET_PER_STREAM * l.streams],
        })
    }

    /// Frames per packet; chunks hold whole packets.
    pub fn packet_frames(&self) -> usize {
        self.packet
    }

    /// Input frames taken from the source so far.
    pub fn source_frames(&self) -> u64 {
        self.source_frames
    }

    /// The pts of the next chunk.
    pub fn next_pts(&self) -> u64 {
        (self.packets * self.packet as u64).saturating_sub(self.lookahead)
    }

    /// Takes `samples` (interleaved), encodes the whole packets they complete and writes a
    /// chunk payload of them to `out`. With `end`, the source is exhausted: the input is
    /// padded with silence so that the decoded output covers its last frame. Returns the
    /// chunk's pts and frame count, or `None` when it has no frames to keep.
    pub fn chunk(
        &mut self,
        samples: &[f32],
        end: bool,
        out: &mut Vec<u8>,
    ) -> Result<Option<(u64, u64)>, String> {
        let (ch, n) = (self.channels, self.packet);
        self.input.extend_from_slice(samples);
        self.source_frames += (samples.len() / ch) as u64;
        let mut count = self.input.len() / (n * ch);
        if end {
            // Packets until the decoded output reaches the last input frame.
            let total = (self.source_frames + self.lookahead).div_ceil(n as u64);
            count = total.saturating_sub(self.packets) as usize;
            self.input.resize(count * n * ch, 0.0);
        }
        self.out.clear();
        for i in 0..count {
            let len = self
                .inner
                .encode(
                    &self.input[i * n * ch..(i + 1) * n * ch],
                    n,
                    &mut self.scratch,
                )
                .map_err(|e| format!("opus encoder: {e}"))?;
            self.out.push(self.scratch[..len].to_vec());
        }
        self.input.drain(..count * n * ch);
        let first = self.packets * n as u64;
        self.packets += count as u64;
        let last = self.packets * n as u64;
        // Kept: input frames [start, stop), decoded frames [start + L, stop + L).
        let start = first.saturating_sub(self.lookahead);
        let stop = last.saturating_sub(self.lookahead).min(self.source_frames);
        if count == 0 || stop <= start {
            return Ok(None);
        }
        let skip = (start + self.lookahead - first) as u16;
        out.clear();
        opus::write_payload((stop - start) as u32, skip, &self.out, out);
        Ok(Some((start, stop - start)))
    }
}
