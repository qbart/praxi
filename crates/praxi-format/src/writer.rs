use crate::{
    AUDIO_NAME_SIZE, CHUNK_FLAG_KEYFRAME, CHUNK_HEADER_SIZE, CUE_NAME_SIZE, Compression, Cue,
    Error, HEADER_SIZE, IndexEntry, MAGIC, Result, STREAM_ENTRY_SIZE, StreamInfo, StreamParams,
    VERSION_MAJOR, VERSION_MINOR, to_planes,
};
use std::io::{Seek, SeekFrom, Write};

/// Writes a praxi file. Chunks must be written in time order (`docs/spec/PRAXI.md#ordering`).
pub struct Writer<W: Write + Seek> {
    inner: W,
    streams: Vec<StreamInfo>,
    offset: u64,
    index: Vec<IndexEntry>,
    /// Last pts per stream, to enforce pts order within a stream.
    last_pts: Vec<Option<u64>>,
    /// Last chunk decode time in microseconds, to enforce file order.
    last_time_us: u64,
    /// Per audio stream: the frame after the last written chunk.
    audio_end: Vec<u64>,
    scratch: Vec<u8>,
    planes: Vec<u8>,
    planes_lz4: Vec<u8>,
    cue_payload: Vec<u8>,
}

impl<W: Write + Seek> Writer<W> {
    pub fn new(mut inner: W, streams: Vec<StreamInfo>) -> Result<Writer<W>> {
        if streams.is_empty() || streams.len() > u16::MAX as usize {
            return Err(Error::Invalid(format!("invalid stream count {}", streams.len())));
        }
        for (i, s) in streams.iter().enumerate() {
            if s.time_base.num == 0 || s.time_base.den == 0 {
                return Err(Error::Invalid(format!("stream {i}: zero time base")));
            }
            if let StreamParams::Video(v) = &s.params
                && (v.width == 0 || v.height == 0)
            {
                return Err(Error::Invalid(format!("stream {i}: zero frame size")));
            }
            if let StreamParams::Cues(c) = &s.params
                && (c.name.len() > CUE_NAME_SIZE || c.name.contains('\0'))
            {
                return Err(Error::Invalid(format!("stream {i}: cue stream name too long or has a zero byte")));
            }
            if let StreamParams::Audio(a) = &s.params {
                if a.sample_rate == 0 || a.channels == 0 {
                    return Err(Error::Invalid(format!("stream {i}: zero sample rate or channels")));
                }
                if s.time_base != crate::TimeBase::new(1, a.sample_rate) {
                    return Err(Error::Invalid(format!("stream {i}: audio time base must be 1/sample_rate")));
                }
                if a.channel_mask != 0 && a.channel_mask.count_ones() != a.channels as u32 {
                    return Err(Error::Invalid(format!("stream {i}: channel mask does not match {} channels", a.channels)));
                }
                if a.name.len() > AUDIO_NAME_SIZE || a.name.contains('\0') {
                    return Err(Error::Invalid(format!("stream {i}: audio stream name too long or has a zero byte")));
                }
            }
        }
        inner.seek(SeekFrom::Start(0))?;
        inner.write_all(&[0u8; HEADER_SIZE as usize])?;
        for s in &streams {
            inner.write_all(&encode_stream(s))?;
        }
        let n = streams.len();
        Ok(Writer {
            inner,
            streams,
            offset: HEADER_SIZE + STREAM_ENTRY_SIZE * n as u64,
            index: Vec::new(),
            last_pts: vec![None; n],
            last_time_us: 0,
            audio_end: vec![0; n],
            scratch: Vec::new(),
            planes: Vec::new(),
            planes_lz4: Vec::new(),
            cue_payload: Vec::new(),
        })
    }

    pub fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    /// Bytes written so far (header, stream table and chunks).
    pub fn bytes_written(&self) -> u64 {
        self.offset
    }

    /// Writes one chunk of a video stream. `compression = Some(c)` forces `c`; `None` picks
    /// the smallest of none, LZ4 and LZ4 over byte planes. Cue streams take [`Writer::write_cue`],
    /// audio streams [`Writer::write_audio`].
    pub fn write_chunk(
        &mut self,
        stream: u16,
        pts: u64,
        keyframe: bool,
        payload: &[u8],
        compression: Option<Compression>,
    ) -> Result<()> {
        match self.streams.get(stream as usize).map(|s| &s.params) {
            Some(StreamParams::Cues(_)) => Err(Error::Invalid(format!("stream {stream} is a cue stream, use write_cue"))),
            Some(StreamParams::Audio(_)) => Err(Error::Invalid(format!("stream {stream} is an audio stream, use write_audio"))),
            _ => self.write_raw(stream, pts, keyframe, payload, compression),
        }
    }

    /// Writes one chunk of an audio stream: interleaved samples in the stream's sample
    /// format, starting at frame `pts`. LZ4 is used when it makes the payload smaller.
    pub fn write_audio(&mut self, stream: u16, pts: u64, samples: &[u8]) -> Result<()> {
        let Some(StreamParams::Audio(a)) = self.streams.get(stream as usize).map(|s| &s.params) else {
            return Err(Error::Invalid(format!("stream {stream} is not an audio stream")));
        };
        let frame_bytes = a.frame_bytes();
        if samples.is_empty() || !samples.len().is_multiple_of(frame_bytes) {
            return Err(Error::Invalid(format!("stream {stream}: {} bytes is not a whole number of frames", samples.len())));
        }
        let end = self.audio_end[stream as usize];
        if self.last_pts[stream as usize].is_some() && pts < end {
            return Err(Error::Invalid(format!("stream {stream}: audio chunk at {pts} overlaps the previous one ending at {end}")));
        }
        self.write_raw(stream, pts, true, samples, None)?;
        self.audio_end[stream as usize] = pts + (samples.len() / frame_bytes) as u64;
        Ok(())
    }

    /// Writes one cue starting at `pts` to a cue stream.
    pub fn write_cue(&mut self, stream: u16, pts: u64, cue: &Cue) -> Result<()> {
        if !matches!(self.streams.get(stream as usize).map(|s| &s.params), Some(StreamParams::Cues(_))) {
            return Err(Error::Invalid(format!("stream {stream} is not a cue stream")));
        }
        let mut payload = std::mem::take(&mut self.cue_payload);
        payload.clear();
        let result = cue.encode(&mut payload).and_then(|()| self.write_raw(stream, pts, true, &payload, None));
        self.cue_payload = payload;
        result?;
        if let StreamParams::Cues(c) = &mut self.streams[stream as usize].params {
            c.max_duration = c.max_duration.max(cue.duration);
        }
        Ok(())
    }

    fn write_raw(
        &mut self,
        stream: u16,
        pts: u64,
        keyframe: bool,
        payload: &[u8],
        compression: Option<Compression>,
    ) -> Result<()> {
        let Some(info) = self.streams.get(stream as usize) else {
            return Err(Error::Invalid(format!("unknown stream {stream}")));
        };
        let is_cue = matches!(info.params, StreamParams::Cues(_));
        let is_audio = matches!(info.params, StreamParams::Audio(_));
        let is_video = matches!(info.params, StreamParams::Video(_));
        if let Some(last) = self.last_pts[stream as usize]
            && (pts < last || (pts == last && !is_cue))
        {
            return Err(Error::Invalid(format!("stream {stream}: pts {pts} after {last} breaks stream order")));
        }
        let time_us = info.decode_time_us(pts);
        if time_us < self.last_time_us {
            return Err(Error::Invalid(format!("stream {stream}: chunk due at {time_us} us written out of order")));
        }
        if payload.len() > u32::MAX as usize {
            return Err(Error::Invalid("payload too large".into()));
        }

        // (prefix, block bytes) when the payload can take the byte-plane form.
        let plane_layout = match &info.params {
            StreamParams::Video(v) => {
                let (prefix, block_bytes) = (v.plane_prefix(keyframe), v.format.block_bytes() as usize);
                (payload.len() >= prefix && (payload.len() - prefix).is_multiple_of(block_bytes))
                    .then_some((prefix, block_bytes))
            }
            _ => None,
        };
        let planes = |w: &mut Self, (prefix, block_bytes): (usize, usize)| {
            w.planes.clear();
            w.planes.resize(payload.len(), 0);
            w.planes[..prefix].copy_from_slice(&payload[..prefix]);
            to_planes(&payload[prefix..], block_bytes, &mut w.planes[prefix..]);
            w.planes_lz4 = lz4_flex::block::compress(&w.planes);
        };
        let compression = match compression {
            Some(Compression::Lz4Planes) => {
                let layout = plane_layout.ok_or_else(|| {
                    Error::Invalid(format!("stream {stream}: byte planes need a video payload of whole blocks"))
                })?;
                planes(self, layout);
                Compression::Lz4Planes
            }
            Some(Compression::None) => Compression::None,
            Some(Compression::Lz4) => {
                self.scratch = lz4_flex::block::compress(payload);
                Compression::Lz4
            }
            None => {
                self.scratch = lz4_flex::block::compress(payload);
                let mut best = if self.scratch.len() < payload.len() { Compression::Lz4 } else { Compression::None };
                if let Some(layout) = plane_layout {
                    planes(self, layout);
                    if self.planes_lz4.len() < self.scratch.len().min(payload.len()) {
                        best = Compression::Lz4Planes;
                    }
                }
                best
            }
        };
        let stored: &[u8] = match compression {
            Compression::None => payload,
            Compression::Lz4 => &self.scratch,
            Compression::Lz4Planes => &self.planes_lz4,
        };

        let flags = if keyframe { CHUNK_FLAG_KEYFRAME } else { 0 };
        let mut h = [0u8; CHUNK_HEADER_SIZE as usize];
        h[0..2].copy_from_slice(&stream.to_le_bytes());
        h[2] = flags;
        h[3] = compression.id();
        h[4..8].copy_from_slice(&(stored.len() as u32).to_le_bytes());
        h[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        h[16..24].copy_from_slice(&pts.to_le_bytes());
        self.inner.write_all(&h)?;
        self.inner.write_all(stored)?;

        if is_cue || is_audio || (keyframe && is_video) {
            self.index.push(IndexEntry { stream, pts, offset: self.offset });
        }
        self.offset += CHUNK_HEADER_SIZE + stored.len() as u64;
        self.streams[stream as usize].chunk_count += 1;
        self.last_pts[stream as usize] = Some(pts);
        self.last_time_us = time_us;
        Ok(())
    }

    /// Writes the index, patches the header and stream table, and returns the inner writer.
    pub fn finish(mut self, duration_us: u64) -> Result<W> {
        let index_offset = self.offset;
        for e in &self.index {
            let mut b = [0u8; crate::INDEX_ENTRY_SIZE as usize];
            b[0..2].copy_from_slice(&e.stream.to_le_bytes());
            b[8..16].copy_from_slice(&e.pts.to_le_bytes());
            b[16..24].copy_from_slice(&e.offset.to_le_bytes());
            self.inner.write_all(&b)?;
        }

        let mut h = [0u8; HEADER_SIZE as usize];
        h[0..8].copy_from_slice(&MAGIC);
        h[8..10].copy_from_slice(&VERSION_MAJOR.to_le_bytes());
        h[10..12].copy_from_slice(&VERSION_MINOR.to_le_bytes());
        h[16..18].copy_from_slice(&(self.streams.len() as u16).to_le_bytes());
        h[24..32].copy_from_slice(&duration_us.to_le_bytes());
        h[32..40].copy_from_slice(&index_offset.to_le_bytes());
        h[40..44].copy_from_slice(&(self.index.len() as u32).to_le_bytes());
        self.inner.seek(SeekFrom::Start(0))?;
        self.inner.write_all(&h)?;
        for s in &self.streams {
            self.inner.write_all(&encode_stream(s))?;
        }
        self.inner.seek(SeekFrom::End(0))?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

fn encode_stream(s: &StreamInfo) -> [u8; STREAM_ENTRY_SIZE as usize] {
    let mut b = [0u8; STREAM_ENTRY_SIZE as usize];
    b[0] = s.params.kind();
    b[2..4].copy_from_slice(&s.group.to_le_bytes());
    b[4..8].copy_from_slice(&s.time_base.num.to_le_bytes());
    b[8..12].copy_from_slice(&s.time_base.den.to_le_bytes());
    b[12..16].copy_from_slice(&s.chunk_count.to_le_bytes());
    if let StreamParams::Video(v) = &s.params {
        let (bw, bh) = v.format.block_size();
        b[16..20].copy_from_slice(&v.width.to_le_bytes());
        b[20..24].copy_from_slice(&v.height.to_le_bytes());
        b[24..26].copy_from_slice(&v.format.id().to_le_bytes());
        b[26..28].copy_from_slice(&v.color_flags.to_le_bytes());
        b[28] = bw as u8;
        b[29] = bh as u8;
        b[30] = v.format.block_bytes() as u8;
    }
    if let StreamParams::Cues(c) = &s.params {
        b[16..24].copy_from_slice(&c.max_duration.to_le_bytes());
        b[24..24 + c.name.len()].copy_from_slice(c.name.as_bytes());
    }
    if let StreamParams::Audio(a) = &s.params {
        b[16..20].copy_from_slice(&a.sample_rate.to_le_bytes());
        b[20..22].copy_from_slice(&a.channels.to_le_bytes());
        b[22..24].copy_from_slice(&a.format.id().to_le_bytes());
        b[24..28].copy_from_slice(&a.channel_mask.to_le_bytes());
        b[28..32].copy_from_slice(&a.preload_us.to_le_bytes());
        b[32..32 + a.name.len()].copy_from_slice(a.name.as_bytes());
    }
    b
}
