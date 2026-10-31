//! The byte-plane form of video payloads (`docs/spec/PRAXI.md`, compression `2`).

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
fn from_planes_swar<const B: usize, const T: usize>(planes: &[u8], n: usize, out: &mut [u8]) -> usize {
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

    #[test]
    fn planes_roundtrip() {
        for block_bytes in [1, 3, 8, 16] {
            for n in [0, 1, 7, 8, 9, 100, 1003] {
                let blocks: Vec<u8> = (0..n * block_bytes).map(|i| (i * 31 % 251) as u8).collect();
                let mut planes = vec![0; blocks.len()];
                to_planes(&blocks, block_bytes, &mut planes);
                if n > 1 {
                    assert_eq!(planes[1], blocks[block_bytes], "plane 0 holds byte 0 of each block");
                }
                let mut back = vec![0; blocks.len()];
                from_planes(&planes, block_bytes, &mut back);
                assert_eq!(back, blocks, "block_bytes {block_bytes}, n {n}");
            }
        }
    }
}
