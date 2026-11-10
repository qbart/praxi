//! Huffman sections of the byte-plane form (`docs/spec/PRAXI.md#huffman-section`).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub(crate) const MAX_LEN: usize = 11;
pub(crate) const TABLE_SIZE: usize = 1 << MAX_LEN;
const STREAMS: usize = 4;
/// Code lengths and the sizes of streams 0 to 2.
const HEADER_SIZE: usize = 128 + 12;
/// Table lookups per refill: a refill leaves at least 56 bits, 5 lookups use at most 55.
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
        let size =
            u32::from_le_bytes(src[128 + 4 * j..132 + 4 * j].try_into().expect("4 bytes")) as usize;
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

    // Interleaved loop while every stream has 8 readable bytes and every run room for 5
    // pairs. Run 3 is the shortest.
    let lim = streams
        .iter()
        .map(|s| s.len())
        .min()
        .unwrap_or(0)
        .checked_sub(8);
    let end = parts[STREAMS - 1].len().checked_sub(2 * PER_REFILL);
    let mut written = [0; STREAMS];
    if let (Some(lim), Some(end)) = (lim, end) {
        let [o0, o1, o2, o3] = &mut parts;
        // Local copies, so the reader state can stay in registers across output writes.
        let [mut r0, mut r1, mut r2, mut r3] = readers;
        let [mut w0, mut w1, mut w2, mut w3] = written;
        while r0.pos.max(r1.pos).max(r2.pos).max(r3.pos) <= lim && w0.max(w1).max(w2).max(w3) <= end
        {
            r0.refill_fast();
            r1.refill_fast();
            r2.refill_fast();
            r3.refill_fast();
            for _ in 0..PER_REFILL {
                w0 += r0.pair(&table, &mut o0[w0..w0 + 2]);
                w1 += r1.pair(&table, &mut o1[w1..w1 + 2]);
                w2 += r2.pair(&table, &mut o2[w2..w2 + 2]);
                w3 += r3.pair(&table, &mut o3[w3..w3 + 2]);
            }
        }
        readers = [r0, r1, r2, r3];
        written = [w0, w1, w2, w3];
    }
    for ((r, part), start) in readers.iter_mut().zip(parts).zip(written) {
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

#[derive(Clone, Copy)]
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
        BitReader {
            src,
            pos: 0,
            acc: 0,
            bits: 0,
        }
    }

    /// Loads whole bytes until at least 56 bits are valid; needs `pos + 8 <= src.len()`.
    /// Bits above `bits` may hold the following bytes, which later loads OR in again.
    #[inline(always)]
    fn refill_fast(&mut self) {
        let word = u64::from_le_bytes(
            self.src[self.pos..self.pos + 8]
                .try_into()
                .expect("8 bytes"),
        );
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

    /// Decodes one or two symbols into `out[0]` and `out[1]`; returns how many.
    #[inline(always)]
    fn pair(&mut self, table: &Table, out: &mut [u8]) -> usize {
        let entry = table[(self.acc & (TABLE_SIZE as u64 - 1)) as usize];
        let len = entry >> 16 & 15;
        self.acc >>= len;
        self.bits -= len;
        out.copy_from_slice(&(entry as u16).to_le_bytes());
        (entry >> 24) as usize
    }

    #[inline(always)]
    fn symbol(&mut self, table: &Table) -> u8 {
        let entry = table[(self.acc & (TABLE_SIZE as u64 - 1)) as usize];
        let len = entry >> 20 & 15;
        self.acc >>= len;
        self.bits -= len;
        entry as u8
    }

    fn consumed_bits(&self) -> usize {
        self.pos * 8 - self.bits as usize
    }
}

/// Decode table indexed by the next 11 stream bits. Each entry decodes the first code and,
/// when it fits in the same 11 bits, the second: `symbol1 | symbol2 << 8 | bits << 16 |
/// length1 << 20 | count << 24`.
type Table = [u32; TABLE_SIZE];

fn decode_table(lens: &[u8; 256], empty: bool) -> Result<Table, &'static str> {
    let single = match single_table(lens)? {
        Some(single) => single,
        None if empty => return Ok([0; TABLE_SIZE]),
        None => return Err("Huffman section without codes"),
    };
    // The bits after the first code index the single table again; its entry is valid when
    // the second code fits in those bits.
    let mut table = [0u32; TABLE_SIZE];
    for (i, entry) in table.iter_mut().enumerate() {
        let (first, len1) = ((single[i] >> 8) as u32, (single[i] & 15) as u32);
        let next = single[i >> len1];
        let (second, len2) = ((next >> 8) as u32, (next & 15) as u32);
        *entry = if len1 + len2 <= MAX_LEN as u32 {
            first | second << 8 | (len1 + len2) << 16 | len1 << 20 | 2 << 24
        } else {
            first | len1 << 16 | len1 << 20 | 1 << 24
        };
    }
    Ok(table)
}

/// Lookup table indexed by the next 11 stream bits: `symbol << 8 | length` of the first code,
/// or `None` when no value has a code. Checks that the lengths form a complete code.
pub(crate) fn single_table(lens: &[u8; 256]) -> Result<Option<[u16; TABLE_SIZE]>, &'static str> {
    let used = || (0..=255u8).filter(|&s| lens[s as usize] != 0);
    let mut single = [0u16; TABLE_SIZE];
    match (used().next(), used().nth(1)) {
        (None, _) => return Ok(None),
        (Some(s), None) if lens[s as usize] == 1 => single = [(s as u16) << 8 | 1; TABLE_SIZE],
        _ => {
            if used().any(|s| lens[s as usize] as usize > MAX_LEN) {
                return Err("Huffman code longer than 11 bits");
            }
            let kraft: usize = used().map(|s| TABLE_SIZE >> lens[s as usize]).sum();
            if kraft != TABLE_SIZE {
                return Err("Huffman code lengths do not form a complete code");
            }
            let codes = canonical_codes(lens);
            for s in used() {
                let (code, len) = codes[s as usize];
                for i in (code as usize..TABLE_SIZE).step_by(1 << len) {
                    single[i] = (s as u16) << 8 | len as u16;
                }
            }
        }
    }
    Ok(Some(single))
}

/// Canonical codes, bit-reversed for least-significant-bit-first streams: (code, length).
pub(crate) fn canonical_codes(lens: &[u8; 256]) -> [(u16, u8); 256] {
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
pub(crate) fn code_lengths(hist: &[u32; 256]) -> [u8; 256] {
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
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = syms
        .iter()
        .enumerate()
        .map(|(i, &s)| Reverse((hist[s as usize] as u64, i)))
        .collect();
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
                // Skewed toward small values: `bits` random bits, shifted right by 0 to 7.
                (x as u8 & ((1u16 << bits) - 1) as u8) >> ((x >> 61) as u32)
            })
            .collect()
    }

    #[test]
    fn roundtrips() {
        for len in [
            0, 1, 2, 3, 4, 5, 19, 20, 21, 39, 40, 41, 100, 1000, 4099, 100_000,
        ] {
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
        assert_eq!(
            lens.iter()
                .filter(|&&l| l > 0)
                .map(|&l| TABLE_SIZE >> l)
                .sum::<usize>(),
            TABLE_SIZE
        );
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
        assert!(
            decode(&bad, &mut out).is_err(),
            "lengths over 11 / incomplete code"
        );
        let mut bad = enc.clone();
        bad[128..132].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&bad, &mut out).is_err(), "stream size past the end");
        assert!(
            decode(&enc[..enc.len() - 50], &mut out).is_err(),
            "truncated last stream"
        );
        let mut bad = vec![0u8; HEADER_SIZE];
        assert!(decode(&bad, &mut out).is_err(), "no codes");
        assert!(decode(&bad, &mut []).is_ok(), "empty section");
        bad[0] = 0x10; // one symbol of length 1
        assert!(
            decode(&bad, &mut out[..10]).is_err(),
            "single code without stream bits"
        );
    }
}
