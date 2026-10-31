//! Huffman sections of the byte-plane form (`docs/spec/PRAXI.md#huffman-section`).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

const MAX_LEN: usize = 11;
const TABLE_SIZE: usize = 1 << MAX_LEN;
const STREAMS: usize = 4;
/// Code lengths and the sizes of streams 0 to 2.
const HEADER_SIZE: usize = 128 + 12;
/// Symbols decoded per refill: a refill leaves at least 56 bits, 5 codes use at most 55.
const PER_REFILL: usize = 5;

/// Appends the Huffman section of `data` to `out`.
pub fn encode(data: &[u8], out: &mut Vec<u8>) {
    let mut hist = [0u32; 256];
    for &b in data {
        hist[b as usize] += 1;
    }
    let lens = code_lengths(&hist);
    let codes = canonical_codes(&lens);

    let start = out.len();
    out.extend((0..128).map(|i| lens[2 * i] | lens[2 * i + 1] << 4));
    out.resize(start + HEADER_SIZE, 0);
    let q = data.len().div_ceil(STREAMS);
    for j in 0..STREAMS {
        let stream_start = out.len();
        let (mut acc, mut bits) = (0u64, 0u32);
        for &b in data.iter().skip(j * q).take(q) {
            let (code, len) = codes[b as usize];
            acc |= (code as u64) << bits;
            bits += len as u32;
            while bits >= 8 {
                out.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
        }
        if bits > 0 {
            out.push(acc as u8);
        }
        if j < STREAMS - 1 {
            let size = (out.len() - stream_start) as u32;
            out[start + 128 + 4 * j..start + 132 + 4 * j].copy_from_slice(&size.to_le_bytes());
        }
    }
}

/// Decodes a Huffman section into `out`, whose length is the section's length.
pub fn decode(src: &[u8], out: &mut [u8]) -> Result<(), &'static str> {
    if src.len() < HEADER_SIZE {
        return Err("Huffman section shorter than its header");
    }
    let mut lens = [0u8; 256];
    for (i, &b) in src[..128].iter().enumerate() {
        lens[2 * i] = b & 15;
        lens[2 * i + 1] = b >> 4;
    }
    let table = decode_table(&lens, out.is_empty())?;
    let mut streams: [&[u8]; STREAMS] = [&[]; STREAMS];
    let mut rest = &src[HEADER_SIZE..];
    for (j, stream) in streams.iter_mut().enumerate().take(STREAMS - 1) {
        let size = u32::from_le_bytes(src[128 + 4 * j..132 + 4 * j].try_into().expect("4 bytes")) as usize;
        if size > rest.len() {
            return Err("Huffman stream sizes exceed the section");
        }
        (*stream, rest) = rest.split_at(size);
    }
    streams[STREAMS - 1] = rest;

    let q = out.len().div_ceil(STREAMS);
    let mut parts: [&mut [u8]; STREAMS] = Default::default();
    let mut tail = out;
    for part in parts.iter_mut() {
        let (head, t) = tail.split_at_mut(q.min(tail.len()));
        *part = head;
        tail = t;
    }
    let mut readers = streams.map(BitReader::new);

    // Interleaved loop while every stream has 8 readable bytes and every run 5 symbols left.
    let lim = streams.iter().map(|s| s.len()).min().unwrap_or(0).checked_sub(8);
    let [r0, r1, r2, r3] = &mut readers;
    let [o0, o1, o2, o3] = &mut parts;
    let mut done = 0;
    if let Some(lim) = lim {
        let fast = o3.len() / PER_REFILL;
        let groups = o0
            .chunks_exact_mut(PER_REFILL)
            .zip(o1.chunks_exact_mut(PER_REFILL))
            .zip(o2.chunks_exact_mut(PER_REFILL))
            .zip(o3.chunks_exact_mut(PER_REFILL))
            .take(fast);
        for (((c0, c1), c2), c3) in groups {
            if r0.pos.max(r1.pos).max(r2.pos).max(r3.pos) > lim {
                break;
            }
            r0.refill_fast();
            r1.refill_fast();
            r2.refill_fast();
            r3.refill_fast();
            for i in 0..PER_REFILL {
                c0[i] = r0.symbol(&table);
                c1[i] = r1.symbol(&table);
                c2[i] = r2.symbol(&table);
                c3[i] = r3.symbol(&table);
            }
            done += PER_REFILL;
        }
    }
    for (r, part) in readers.iter_mut().zip(parts) {
        let start = done.min(part.len());
        for b in part[start..].iter_mut() {
            r.refill_slow();
            *b = r.symbol(&table);
        }
        if r.consumed_bits() > r.src.len() * 8 {
            return Err("Huffman stream ends early");
        }
    }
    Ok(())
}

struct BitReader<'a> {
    src: &'a [u8],
    /// Bytes loaded into `acc` (past the end counts zero bytes).
    pos: usize,
    acc: u64,
    /// Valid bits in `acc`.
    bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(src: &'a [u8]) -> BitReader<'a> {
        BitReader { src, pos: 0, acc: 0, bits: 0 }
    }

    /// Loads whole bytes until at least 56 bits are valid; needs `pos + 8 <= src.len()`.
    /// Bits above `bits` may hold the following bytes, which later loads OR in again.
    #[inline(always)]
    fn refill_fast(&mut self) {
        let word = u64::from_le_bytes(self.src[self.pos..self.pos + 8].try_into().expect("8 bytes"));
        self.acc |= word << self.bits;
        self.pos += ((63 - self.bits) >> 3) as usize;
        self.bits |= 56;
    }

    #[inline(always)]
    fn refill_slow(&mut self) {
        while self.bits <= 56 {
            let byte = self.src.get(self.pos).copied().unwrap_or(0);
            self.acc |= (byte as u64) << self.bits;
            self.pos += 1;
            self.bits += 8;
        }
    }

    #[inline(always)]
    fn symbol(&mut self, table: &[u16; TABLE_SIZE]) -> u8 {
        let entry = table[(self.acc & (TABLE_SIZE as u64 - 1)) as usize];
        let len = (entry & 15) as u32;
        self.acc >>= len;
        self.bits -= len;
        (entry >> 8) as u8
    }

    fn consumed_bits(&self) -> usize {
        self.pos * 8 - self.bits as usize
    }
}

/// Lookup table indexed by the next 11 stream bits: symbol << 8 | code length.
fn decode_table(lens: &[u8; 256], empty: bool) -> Result<[u16; TABLE_SIZE], &'static str> {
    let used: Vec<u8> = (0..=255).filter(|&s| lens[s as usize] != 0).collect();
    let mut table = [0u16; TABLE_SIZE];
    match used[..] {
        [] if empty => return Ok(table),
        [] => return Err("Huffman section without codes"),
        [s] if lens[s as usize] == 1 => return Ok([(s as u16) << 8 | 1; TABLE_SIZE]),
        _ => {}
    }
    if used.iter().any(|&s| lens[s as usize] as usize > MAX_LEN) {
        return Err("Huffman code longer than 11 bits");
    }
    let kraft: usize = used.iter().map(|&s| TABLE_SIZE >> lens[s as usize]).sum();
    if kraft != TABLE_SIZE {
        return Err("Huffman code lengths do not form a complete code");
    }
    let codes = canonical_codes(lens);
    for &s in &used {
        let (code, len) = codes[s as usize];
        for i in (code as usize..TABLE_SIZE).step_by(1 << len) {
            table[i] = (s as u16) << 8 | len as u16;
        }
    }
    Ok(table)
}

/// Canonical codes, bit-reversed for least-significant-bit-first streams: (code, length).
fn canonical_codes(lens: &[u8; 256]) -> [(u16, u8); 256] {
    let mut codes = [(0u16, 0u8); 256];
    let mut code = 0u32;
    for len in 1..=MAX_LEN as u8 {
        for s in 0..256 {
            if lens[s] == len {
                codes[s] = ((code.reverse_bits() >> (32 - len as u32)) as u16, len);
                code += 1;
            }
        }
        code <<= 1;
    }
    codes
}

/// Huffman code lengths limited to 11 bits, forming a complete code (one symbol: length 1).
fn code_lengths(hist: &[u32; 256]) -> [u8; 256] {
    let mut lens = [0u8; 256];
    // Most frequent first, so the shortest lengths go to the front.
    let mut syms: Vec<u8> = (0..=255).filter(|&s| hist[s as usize] > 0).collect();
    syms.sort_by_key(|&s| (Reverse(hist[s as usize]), s));
    let n = syms.len();
    match n {
        0 => return lens,
        1 => {
            lens[syms[0] as usize] = 1;
            return lens;
        }
        _ => {}
    }

    // Leaf depths of a Huffman tree. Nodes 0..n are leaves; parents get higher numbers.
    let mut parent = vec![0usize; 2 * n - 1];
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> =
        syms.iter().enumerate().map(|(i, &s)| Reverse((hist[s as usize] as u64, i))).collect();
    let mut next = n;
    while let (Some(Reverse((wa, a))), Some(Reverse((wb, b)))) = (heap.pop(), heap.pop()) {
        parent[a] = next;
        parent[b] = next;
        heap.push(Reverse((wa + wb, next)));
        next += 1;
    }
    let mut depth = vec![0usize; 2 * n - 1];
    for node in (0..2 * n - 2).rev() {
        depth[node] = depth[parent[node]] + 1;
    }

    // Count codes per length, clamp to MAX_LEN, then restore a complete code by moving
    // codes down from shorter lengths (as in miniz's `tdefl_huffman_enforce_max_code_size`).
    let mut count = [0usize; MAX_LEN + 1];
    for &d in &depth[..n] {
        count[d.min(MAX_LEN)] += 1;
    }
    let mut kraft: usize = (1..=MAX_LEN).map(|len| count[len] << (MAX_LEN - len)).sum();
    while kraft > TABLE_SIZE {
        count[MAX_LEN] -= 1;
        if let Some(len) = (1..MAX_LEN).rev().find(|&len| count[len] > 0) {
            count[len] -= 1;
            count[len + 1] += 2;
        }
        kraft -= 1;
    }
    let mut sorted = syms.iter();
    for (len, &c) in count.iter().enumerate().skip(1) {
        for &s in sorted.by_ref().take(c) {
            lens[s as usize] = len as u8;
        }
    }
    lens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8]) -> usize {
        let mut enc = Vec::new();
        encode(data, &mut enc);
        let mut out = vec![0xAA; data.len()];
        decode(&enc, &mut out).unwrap();
        assert_eq!(out, data, "len {}", data.len());
        enc.len()
    }

    fn noise(len: usize, seed: u64, bits: u32) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                // Skewed: geometric-ish distribution over `bits` bits.
                ((x as u8) & ((1u16 << bits) - 1) as u8) >> (x >> 61) as u32 % bits.max(1)
            })
            .collect()
    }

    #[test]
    fn roundtrips() {
        for len in [0, 1, 2, 3, 4, 5, 19, 20, 21, 39, 40, 41, 100, 1000, 4099, 100_000] {
            roundtrip(&noise(len, len as u64 + 7, 8));
            roundtrip(&noise(len, len as u64 + 9, 3));
            roundtrip(&vec![42; len]);
        }
        let all: Vec<u8> = (0..=255).cycle().take(5000).collect();
        assert!(roundtrip(&all) >= 5000, "uniform bytes need 8 bits");
    }

    #[test]
    fn limits_long_codes() {
        // Fibonacci counts give a Huffman tree as deep as there are symbols.
        let mut data = Vec::new();
        let (mut a, mut b) = (1usize, 1usize);
        for s in 0..24u8 {
            data.extend(std::iter::repeat_n(s, a));
            (a, b) = (b, a + b);
        }
        let mut hist = [0u32; 256];
        data.iter().for_each(|&s| hist[s as usize] += 1);
        let lens = code_lengths(&hist);
        assert_eq!(lens.iter().max(), Some(&(MAX_LEN as u8)));
        assert_eq!(lens.iter().filter(|&&l| l > 0).map(|&l| TABLE_SIZE >> l).sum::<usize>(), TABLE_SIZE);
        roundtrip(&data);
    }

    #[test]
    fn compresses_skewed_data() {
        let data = noise(100_000, 3, 4);
        assert!(roundtrip(&data) < 60_000);
    }

    #[test]
    fn rejects_bad_sections() {
        let data = noise(1000, 5, 8);
        let mut enc = Vec::new();
        encode(&data, &mut enc);
        let mut out = vec![0; data.len()];
        assert!(decode(&enc[..100], &mut out).is_err(), "short header");
        let mut bad = enc.clone();
        bad[0] = 0xFF;
        assert!(decode(&bad, &mut out).is_err(), "lengths over 11 / incomplete code");
        let mut bad = enc.clone();
        bad[128..132].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&bad, &mut out).is_err(), "stream size past the end");
        assert!(decode(&enc[..enc.len() - 50], &mut out).is_err(), "truncated last stream");
        let mut bad = vec![0u8; HEADER_SIZE];
        assert!(decode(&bad, &mut out).is_err(), "no codes");
        assert!(decode(&bad, &mut []).is_ok(), "empty section");
        bad[0] = 0x10; // one symbol of length 1
        assert!(decode(&bad, &mut out[..10]).is_err(), "single code without stream bits");
    }
}
