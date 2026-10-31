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
    match block_bytes {
        8 => from_planes_n::<8>(planes, out),
        16 => from_planes_n::<16>(planes, out),
        _ => {
            let n = planes.len() / block_bytes;
            for (b, block) in out.chunks_exact_mut(block_bytes).enumerate() {
                for (k, byte) in block.iter_mut().enumerate() {
                    *byte = planes[k * n + b];
                }
            }
        }
    }
}

fn from_planes_n<const B: usize>(planes: &[u8], out: &mut [u8]) {
    let n = planes.len() / B;
    if n == 0 {
        return;
    }
    let rows: [&[u8]; B] = std::array::from_fn(|k| &planes[k * n..(k + 1) * n]);
    for (b, block) in out.as_chunks_mut::<B>().0.iter_mut().enumerate() {
        for k in 0..B {
            block[k] = rows[k][b];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_roundtrip() {
        for block_bytes in [1, 3, 8, 16] {
            for n in [0, 1, 7, 100] {
                let blocks: Vec<u8> = (0..n * block_bytes).map(|i| (i * 31 % 251) as u8).collect();
                let mut planes = vec![0; blocks.len()];
                to_planes(&blocks, block_bytes, &mut planes);
                if n > 0 {
                    assert_eq!(planes[1], blocks[block_bytes % blocks.len()], "plane 0 holds byte 0 of each block");
                }
                let mut back = vec![0; blocks.len()];
                from_planes(&planes, block_bytes, &mut back);
                assert_eq!(back, blocks, "block_bytes {block_bytes}, n {n}");
            }
        }
    }
}
