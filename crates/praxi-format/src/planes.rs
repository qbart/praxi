//! The byte-plane form of video payloads (`docs/spec/PRAXI.md`, compression `2`).

use crate::huffman;

const SECTION_RAW: u8 = 0;
const SECTION_LZ4: u8 = 1;
const SECTION_HUFFMAN: u8 = 2;
/// Codec byte and size.
const SECTION_HEADER_SIZE: usize = 5;

/// Appends `planes` (the prefix followed by `block_bytes` planes) to `out` as sections.
/// Each section is raw or LZ4, whichever is smaller, or Huffman when that is smaller still
/// by at least `min_saving` percent. `huff` is scratch space.
pub(crate) fn encode_sections(
    planes: &[u8],
    prefix: usize,
    block_bytes: usize,
    min_saving: f64,
    out: &mut Vec<u8>,
    huff: &mut Vec<u8>,
) {
    let n = (planes.len() - prefix) / block_bytes;
    let sections = std::iter::once(&planes[..prefix])
        .chain(planes[prefix..].chunks_exact(n.max(1)).take(block_bytes));
    for section in sections {
        let lz4 = lz4_flex::block::compress(section);
        let (mut codec, mut data) = if lz4.len() < section.len() {
            (SECTION_LZ4, &lz4[..])
        } else {
            (SECTION_RAW, section)
        };
        if !section.is_empty() && min_saving < 100.0 {
            huff.clear();
            huffman::encode(section, huff);
            if huff.len() < data.len()
                && huff.len() as f64 <= data.len() as f64 * (1.0 - min_saving / 100.0)
            {
                (codec, data) = (SECTION_HUFFMAN, &huff[..]);
            }
        }
        out.push(codec);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
}

/// Decodes the sections in `stored` into `out` (the prefix followed by the planes).
pub(crate) fn decode_sections(
    stored: &[u8],
    prefix: usize,
    block_bytes: usize,
    out: &mut [u8],
) -> Result<(), String> {
    let n = (out.len() - prefix) / block_bytes;
    let (head, planes) = out.split_at_mut(prefix);
    let sections = std::iter::once(head).chain(planes.chunks_exact_mut(n.max(1)).take(block_bytes));
    let mut rest = stored;
    for (i, section) in sections.enumerate() {
        if rest.len() < SECTION_HEADER_SIZE {
            return Err(format!("section {i} header past the end"));
        }
        let size = u32::from_le_bytes(rest[1..5].try_into().expect("4 bytes")) as usize;
        let Some(data) = rest.get(SECTION_HEADER_SIZE..SECTION_HEADER_SIZE + size) else {
            return Err(format!("section {i} runs past the end"));
        };
        match rest[0] {
            SECTION_RAW if size == section.len() => section.copy_from_slice(data),
            SECTION_RAW => {
                return Err(format!(
                    "raw section {i} has {size} bytes, expected {}",
                    section.len()
                ));
            }
            SECTION_LZ4 => match lz4_flex::block::decompress_into(data, section) {
                Ok(len) if len == section.len() => {}
                Ok(len) => {
                    return Err(format!(
                        "section {i} decompressed to {len} bytes, expected {}",
                        section.len()
                    ));
                }
                Err(e) => return Err(format!("section {i}: {e}")),
            },
            SECTION_HUFFMAN => {
                huffman::decode(data, section).map_err(|e| format!("section {i}: {e}"))?
            }
            codec => return Err(format!("section {i} has unknown codec {codec}")),
        }
        rest = &rest[SECTION_HEADER_SIZE + size..];
    }
    if !rest.is_empty() {
        return Err(format!("{} bytes after the last section", rest.len()));
    }
    Ok(())
}

/// Writes `blocks` (whole blocks of `block_bytes`) to `out` as planes: byte `k` of block
/// `b` goes to `k * n + b`. `out` must be as long as `blocks`.
pub fn to_planes(blocks: &[u8], block_bytes: usize, out: &mut [u8]) {
    assert_eq!(blocks.len(), out.len());
    assert!(block_bytes > 0 && blocks.len().is_multiple_of(block_bytes));
    let n = blocks.len() / block_bytes;
    for (k, plane) in out.chunks_exact_mut(n.max(1)).enumerate().take(block_bytes) {
        for (dst, block) in plane.iter_mut().zip(blocks.chunks_exact(block_bytes)) {
            *dst = block[k];
        }
    }
}

/// Inverse of [`to_planes`].
pub fn from_planes(planes: &[u8], block_bytes: usize, out: &mut [u8]) {
    assert_eq!(planes.len(), out.len());
    assert!(block_bytes > 0 && planes.len().is_multiple_of(block_bytes));
    let n = planes.len() / block_bytes;
    // Groups of 8 blocks are transposed as 8x8 byte tiles in u64s; the rest byte by byte.
    let done = match block_bytes {
        8 => from_planes_swar::<8, 1>(planes, n, out),
        16 => from_planes_swar::<16, 2>(planes, n, out),
        _ => 0,
    };
    for b in done..n {
        for k in 0..block_bytes {
            out[b * block_bytes + k] = planes[k * n + b];
        }
    }
}

/// Transposes whole groups of 8 blocks of `B = 8 * T` bytes; returns the blocks done.
fn from_planes_swar<const B: usize, const T: usize>(
    planes: &[u8],
    n: usize,
    out: &mut [u8],
) -> usize {
    let groups = n / 8;
    for (g, dst) in out[..groups * 8 * B].chunks_exact_mut(8 * B).enumerate() {
        let at = g * 8;
        for t in 0..T {
            // Row k: byte 8t + k of blocks at..at + 8.
            let mut tile: [u64; 8] = std::array::from_fn(|k| {
                let start = (8 * t + k) * n + at;
                u64::from_le_bytes(planes[start..start + 8].try_into().expect("8 bytes"))
            });
            transpose8(&mut tile);
            for (b, row) in tile.iter().enumerate() {
                dst[b * B + 8 * t..b * B + 8 * t + 8].copy_from_slice(&row.to_le_bytes());
            }
        }
    }
    groups * 8
}

/// Transposes an 8x8 byte matrix held as 8 little-endian rows.
#[inline(always)]
fn transpose8(r: &mut [u64; 8]) {
    for i in 0..4 {
        let t = ((r[i] >> 32) ^ r[i + 4]) & 0x0000_0000_FFFF_FFFF;
        r[i + 4] ^= t;
        r[i] ^= t << 32;
    }
    for i in [0, 1, 4, 5] {
        let t = ((r[i] >> 16) ^ r[i + 2]) & 0x0000_FFFF_0000_FFFF;
        r[i + 2] ^= t;
        r[i] ^= t << 16;
    }
    for i in [0, 2, 4, 6] {
        let t = ((r[i] >> 8) ^ r[i + 1]) & 0x00FF_00FF_00FF_00FF;
        r[i + 1] ^= t;
        r[i] ^= t << 8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section_codecs(mut stored: &[u8]) -> Vec<u8> {
        let mut codecs = Vec::new();
        while !stored.is_empty() {
            let size = u32::from_le_bytes(stored[1..5].try_into().unwrap()) as usize;
            codecs.push(stored[0]);
            stored = &stored[SECTION_HEADER_SIZE + size..];
        }
        codecs
    }

    #[test]
    fn sections_roundtrip() {
        let mut x = 1u32;
        let mut rnd = || {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        };
        for (prefix, n) in [(0, 0), (0, 1), (5, 0), (3, 7), (0, 1000), (130, 1000)] {
            // Plane 0 constant (LZ4), plane 1 skewed (Huffman), the rest noise (raw).
            let mut planes: Vec<u8> = (0..prefix).map(|_| rnd()).collect();
            planes.extend((0..16 * n).map(|i| match i / n.max(1) {
                0 => 7,
                1 => rnd() & 3,
                _ => rnd(),
            }));
            for min_saving in [0.0, 8.0, 100.0] {
                let (mut stored, mut huff) = (Vec::new(), Vec::new());
                encode_sections(&planes, prefix, 16, min_saving, &mut stored, &mut huff);
                assert!(stored.len() <= planes.len() + 17 * SECTION_HEADER_SIZE);
                let mut out = vec![0xEE; planes.len()];
                decode_sections(&stored, prefix, 16, &mut out).unwrap();
                assert_eq!(out, planes, "prefix {prefix} n {n} min_saving {min_saving}");
                if n == 1000 {
                    let codecs = section_codecs(&stored);
                    assert_eq!(codecs.len(), 17);
                    assert_eq!(codecs[1], SECTION_LZ4, "constant plane");
                    assert_eq!(
                        codecs[2] == SECTION_HUFFMAN,
                        min_saving < 100.0,
                        "skewed plane"
                    );
                    assert_eq!(codecs[3], SECTION_RAW, "noise plane");
                }
                if n > 0 {
                    assert!(
                        decode_sections(&stored[..stored.len() - 1], prefix, 16, &mut out).is_err()
                    );
                    let mut extra = stored.clone();
                    extra.push(0);
                    assert!(decode_sections(&extra, prefix, 16, &mut out).is_err());
                }
            }
        }
    }

    #[test]
    fn planes_roundtrip() {
        for block_bytes in [1, 3, 8, 16] {
            for n in [0, 1, 7, 8, 9, 100, 1003] {
                let blocks: Vec<u8> = (0..n * block_bytes).map(|i| (i * 31 % 251) as u8).collect();
                let mut planes = vec![0; blocks.len()];
                to_planes(&blocks, block_bytes, &mut planes);
                if n > 1 {
                    assert_eq!(
                        planes[1], blocks[block_bytes],
                        "plane 0 holds byte 0 of each block"
                    );
                }
                let mut back = vec![0; blocks.len()];
                from_planes(&planes, block_bytes, &mut back);
                assert_eq!(back, blocks, "block_bytes {block_bytes}, n {n}");
            }
        }
    }
}
