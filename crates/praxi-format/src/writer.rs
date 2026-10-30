use crate::{
    CHUNK_FLAG_KEYFRAME, CHUNK_HEADER_SIZE, CUE_NAME_SIZE, Compression, Cue,
    Error, HEADER_SIZE, IndexEntry, MAGIC, Result, STREAM_ENTRY_SIZE, StreamInfo, StreamParams,
    VERSION_MAJOR, VERSION_MINOR,
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
    /// Last chunk time in microseconds, to enforce file order.
    last_time_us: u64,
    scratch: Vec<u8>,
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
            scratch: Vec::new(),
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

    /// Writes one chunk of a video stream. `compression = Some(c)` forces `c`; `None` uses
    /// LZ4 only when it makes the payload smaller. Cue streams take [`Writer::write_cue`].
    pub fn write_chunk(
        &mut self,
        stream: u16,
        pts: u64,
        keyframe: bool,
        payload: &[u8],
        compression: Option<Compression>,
    ) -> Result<()> {
        if let Some(StreamParams::Cues(_)) = self.streams.get(stream as usize).map(|s| &s.params) {
            return Err(Error::Invalid(format!("stream {stream} is a cue stream, use write_cue")));
        }
        self.write_raw(stream, pts, keyframe, payload, compression)
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
        if let Some(last) = self.last_pts[stream as usize]
            && (pts < last || (pts == last && !is_cue))
        {
            return Err(Error::Invalid(format!("stream {stream}: pts {pts} after {last} breaks stream order")));
        }
        let time_us = info.time_base.to_micros(pts);
        if time_us < self.last_time_us {
            return Err(Error::Invalid(format!("stream {stream}: chunk at {time_us} us written out of time order")));
        }
        if payload.len() > u32::MAX as usize {
            return Err(Error::Invalid("payload too large".into()));
        }

        let (compression, stored): (Compression, &[u8]) = match compression {
            Some(Compression::None) => (Compression::None, payload),
            Some(Compression::Lz4) => {
                self.scratch = lz4_flex::block::compress(payload);
                (Compression::Lz4, &self.scratch)
            }
            None => {
                self.scratch = lz4_flex::block::compress(payload);
                if self.scratch.len() < payload.len() {
                    (Compression::Lz4, &self.scratch)
                } else {
                    (Compression::None, payload)
                }
            }
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

        if is_cue || (keyframe && matches!(info.params, StreamParams::Video(_))) {
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
    b
}
