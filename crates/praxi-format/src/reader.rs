use crate::{
    CHUNK_HEADER_SIZE, ChunkHeader, Compression, Error, HEADER_SIZE, Header, INDEX_ENTRY_SIZE,
    IndexEntry, MAGIC, Result, STREAM_ENTRY_SIZE, StreamInfo, StreamParams, TextureFormat,
    TimeBase, VERSION_MAJOR, VideoParams, corrupt, le_u16, le_u32, le_u64,
};
use std::io::{Read, Seek, SeekFrom};

/// Reads a praxi file: header and stream table up front, then chunks one by one.
pub struct Reader<R: Read + Seek> {
    inner: R,
    header: Header,
    streams: Vec<StreamInfo>,
    data_start: u64,
    /// Offset of the next chunk header.
    pos: u64,
    scratch: Vec<u8>,
}

impl<R: Read + Seek> Reader<R> {
    pub fn new(mut inner: R) -> Result<Reader<R>> {
        inner.seek(SeekFrom::Start(0))?;
        let mut h = [0u8; HEADER_SIZE as usize];
        inner.read_exact(&mut h)?;
        if h[0..8] != MAGIC {
            return Err(Error::BadMagic);
        }
        let header = Header {
            version_major: le_u16(&h, 8),
            version_minor: le_u16(&h, 10),
            duration_us: le_u64(&h, 24),
            index_offset: le_u64(&h, 32),
            index_count: le_u32(&h, 40),
        };
        if header.version_major != VERSION_MAJOR {
            return Err(Error::UnsupportedVersion(header.version_major));
        }
        let stream_count = le_u16(&h, 16) as usize;
        if stream_count == 0 {
            return corrupt("no streams");
        }

        let mut streams = Vec::with_capacity(stream_count);
        let mut b = [0u8; STREAM_ENTRY_SIZE as usize];
        for i in 0..stream_count {
            inner.read_exact(&mut b)?;
            streams.push(decode_stream(i, &b)?);
        }
        let data_start = HEADER_SIZE + STREAM_ENTRY_SIZE * stream_count as u64;
        if header.index_offset < data_start {
            return corrupt(format!("index offset {} inside stream table", header.index_offset));
        }
        Ok(Reader { inner, header, streams, data_start, pos: data_start, scratch: Vec::new() })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    /// Offset of the first chunk.
    pub fn data_start(&self) -> u64 {
        self.data_start
    }

    /// Offset of the next chunk header.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Continues reading at the chunk header at `offset` (from the index or `data_start`).
    pub fn seek_to(&mut self, offset: u64) -> Result<()> {
        if offset < self.data_start || offset > self.header.index_offset {
            return corrupt(format!("chunk offset {offset} out of range"));
        }
        self.pos = offset;
        Ok(())
    }

    /// Reads the next chunk header, or `None` at the end of the chunks. The payload can be
    /// read with [`Reader::read_payload`] at any time; not reading it skips it.
    pub fn next_chunk(&mut self) -> Result<Option<ChunkHeader>> {
        if self.pos >= self.header.index_offset {
            return Ok(None);
        }
        if self.pos + CHUNK_HEADER_SIZE > self.header.index_offset {
            return corrupt(format!("truncated chunk header at {}", self.pos));
        }
        self.inner.seek(SeekFrom::Start(self.pos))?;
        let mut h = [0u8; CHUNK_HEADER_SIZE as usize];
        self.inner.read_exact(&mut h)?;
        let Some(compression) = Compression::from_id(h[3]) else {
            return corrupt(format!("unknown compression {} at {}", h[3], self.pos));
        };
        let chunk = ChunkHeader {
            offset: self.pos,
            stream: le_u16(&h, 0),
            flags: h[2],
            compression,
            stored_size: le_u32(&h, 4),
            raw_size: le_u32(&h, 8),
            pts: le_u64(&h, 16),
        };
        if chunk.stream as usize >= self.streams.len() {
            return corrupt(format!("chunk at {} has unknown stream {}", chunk.offset, chunk.stream));
        }
        if compression == Compression::None && chunk.stored_size != chunk.raw_size {
            return corrupt(format!("uncompressed chunk at {} has mismatched sizes", chunk.offset));
        }
        let end = self.pos + CHUNK_HEADER_SIZE + chunk.stored_size as u64;
        if end > self.header.index_offset {
            return corrupt(format!("chunk at {} runs past the index", chunk.offset));
        }
        self.pos = end;
        Ok(Some(chunk))
    }

    /// Reads and decompresses the payload of `chunk` into `out` (resized to `raw_size`).
    pub fn read_payload(&mut self, chunk: &ChunkHeader, out: &mut Vec<u8>) -> Result<()> {
        self.inner.seek(SeekFrom::Start(chunk.offset + CHUNK_HEADER_SIZE))?;
        out.resize(chunk.raw_size as usize, 0);
        match chunk.compression {
            Compression::None => self.inner.read_exact(out)?,
            Compression::Lz4 => {
                self.scratch.resize(chunk.stored_size as usize, 0);
                self.inner.read_exact(&mut self.scratch)?;
                match lz4_flex::block::decompress_into(&self.scratch, out) {
                    Ok(n) if n == out.len() => {}
                    Ok(n) => return corrupt(format!("chunk at {} decompressed to {n} bytes, expected {}", chunk.offset, out.len())),
                    Err(e) => return corrupt(format!("chunk at {}: {e}", chunk.offset)),
                }
            }
        }
        Ok(())
    }

    /// Reads the index.
    pub fn read_index(&mut self) -> Result<Vec<IndexEntry>> {
        let count = self.header.index_count as usize;
        self.inner.seek(SeekFrom::Start(self.header.index_offset))?;
        let mut buf = vec![0u8; count * INDEX_ENTRY_SIZE as usize];
        self.inner.read_exact(&mut buf)?;
        let mut out = Vec::with_capacity(count);
        for b in buf.chunks_exact(INDEX_ENTRY_SIZE as usize) {
            let e = IndexEntry { stream: le_u16(b, 0), pts: le_u64(b, 8), offset: le_u64(b, 16) };
            if e.stream as usize >= self.streams.len() || e.offset < self.data_start || e.offset >= self.header.index_offset {
                return corrupt(format!("bad index entry {e:?}"));
            }
            out.push(e);
        }
        Ok(out)
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

fn decode_stream(i: usize, b: &[u8]) -> Result<StreamInfo> {
    let time_base = TimeBase { num: le_u32(b, 4), den: le_u32(b, 8) };
    if time_base.num == 0 || time_base.den == 0 {
        return corrupt(format!("stream {i}: zero time base"));
    }
    let params = match b[0] {
        1 => {
            let id = le_u16(b, 24);
            let Some(format) = TextureFormat::from_id(id) else {
                return corrupt(format!("stream {i}: unknown texture format {id:#06x}"));
            };
            let (bw, bh) = format.block_size();
            if (b[28] as u32, b[29] as u32, b[30] as u32) != (bw, bh, format.block_bytes()) {
                return corrupt(format!("stream {i}: block fields do not match {format}"));
            }
            let v = VideoParams { width: le_u32(b, 16), height: le_u32(b, 20), format, color_flags: le_u16(b, 26) };
            if v.width == 0 || v.height == 0 {
                return corrupt(format!("stream {i}: zero frame size"));
            }
            StreamParams::Video(v)
        }
        3 => StreamParams::Markers,
        kind => StreamParams::Unknown(kind),
    };
    Ok(StreamInfo { group: le_u16(b, 2), time_base, chunk_count: le_u32(b, 12), params })
}
