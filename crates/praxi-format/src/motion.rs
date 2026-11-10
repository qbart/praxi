//! The motion codec (`docs/features/motion-codec.md`): payload syntax and entropy coding
//! (`docs/spec/PRAXI.md#motion-payload`), the coded frames handed to GPU decoders, and the
//! reference reconstruction that encoders and tests use.

use crate::huffman::{TABLE_SIZE, canonical_codes, code_lengths, single_table};
use crate::{Chroma, MotionParams, VideoParams};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, mpsc};

/// Largest `qp`.
pub const MAX_QP: u8 = 51;
/// Largest magnitude of a quantized coefficient.
pub const MAX_LEVEL: i32 = 16383;
/// Largest magnitude of a motion vector component, in quarter pels.
pub const MAX_MV: i32 = 8191;
/// Words per block of [`CodedFrame::blocks`]: index and count, first coefficient.
pub const BLOCK_WORDS: usize = 2;
/// Words per macroblock of [`CodedFrame::mbs`]: the kind and four vectors.
pub const MB_WORDS: usize = 5;

/// Huffman tables of a frame, in the order of the payload's table mask.
pub const TABLE_MB: usize = 0;
pub const TABLE_MV: usize = 1;
pub const TABLE_DC_Y: usize = 2;
pub const TABLE_AC_Y: usize = 3;
pub const TABLE_DC_C: usize = 4;
pub const TABLE_AC_C: usize = 5;
pub const TABLE_DC_A: usize = 6;
pub const TABLE_AC_A: usize = 7;
pub const TABLES: usize = 8;

/// Macroblock kinds.
pub const MB_SKIP: u32 = 0;
pub const MB_INTER: u32 = 1;
pub const MB_INTRA: u32 = 2;
/// Four vectors, one per 8x8 luma quadrant.
pub const MB_SPLIT: u32 = 3;

/// A motion vector in quarter pels.
pub type Mv = (i32, i32);

/// Natural (row-major) coefficient index of each zigzag position.
pub const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// The integer DCT: row `k` is basis function `k`.
pub const DCT: [[i32; 8]; 8] = [
    [64, 64, 64, 64, 64, 64, 64, 64],
    [89, 75, 50, 18, -18, -50, -75, -89],
    [83, 36, -36, -83, -83, -36, 36, 83],
    [75, -18, -89, -50, 50, 89, 18, -75],
    [64, -64, -64, 64, 64, -64, -64, 64],
    [50, -89, 18, 75, -75, -18, 89, -50],
    [36, -83, 83, -36, -36, 83, -83, 36],
    [18, -50, 75, -89, 89, -75, 50, -18],
];

/// Dequantization scale by `qp % 6`.
pub const DEQUANT: [i32; 6] = [40, 45, 51, 57, 64, 72];

/// One plane of a frame inside a frame buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plane {
    /// Padded size in pixels, a multiple of 8.
    pub width: u32,
    pub height: u32,
    /// Offset of the plane's first pixel in the frame buffer.
    pub offset: usize,
    /// Index of the plane's first 8x8 block.
    pub first_block: u32,
    /// `log2` of the subsampling: 1 for 4:2:0 chroma, else 0.
    pub shift: u32,
}

impl Plane {
    pub fn blocks_x(&self) -> u32 {
        self.width / 8
    }

    /// Transform blocks per macroblock along each axis.
    pub fn mb_blocks(&self) -> u32 {
        2 >> self.shift
    }
}

/// Plane geometry of a motion stream: Y, Co, Cg and, with alpha, A, one after another in a
/// frame buffer of `size` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub mbs_x: u32,
    pub mbs_y: u32,
    pub planes: Vec<Plane>,
    pub size: usize,
    pub blocks: u32,
}

impl Layout {
    pub fn new(params: &VideoParams) -> Layout {
        let (mbs_x, mbs_y) = (params.width.div_ceil(16), params.height.div_ceil(16));
        let chroma_shift = match params.motion.chroma {
            Chroma::C420 => 1,
            Chroma::C444 => 0,
        };
        let mut shifts = vec![0, chroma_shift, chroma_shift];
        if params.alpha() {
            shifts.push(0);
        }
        let (mut offset, mut first_block) = (0, 0);
        let planes = shifts
            .into_iter()
            .map(|shift| {
                let p = Plane {
                    width: (mbs_x * 16) >> shift,
                    height: (mbs_y * 16) >> shift,
                    offset,
                    first_block,
                    shift,
                };
                offset += (p.width * p.height) as usize;
                first_block += p.width * p.height / 64;
                p
            })
            .collect();
        Layout {
            width: params.width,
            height: params.height,
            mbs_x,
            mbs_y,
            planes,
            size: offset,
            blocks: first_block,
        }
    }

    pub fn mb_count(&self) -> usize {
        (self.mbs_x * self.mbs_y) as usize
    }

    pub fn alpha(&self) -> bool {
        self.planes.len() == 4
    }

    /// The MB rows of slice `s` of `n`.
    pub fn slice_rows(&self, s: u32, n: u32) -> std::ops::Range<u32> {
        s * self.mbs_y / n..(s + 1) * self.mbs_y / n
    }

    /// The transform blocks of macroblock (`mx`, `my`) in syntax order: plane, then block
    /// rows, then block columns. Yields (plane, block x, block y).
    pub fn mb_blocks(&self, mx: u32, my: u32) -> impl Iterator<Item = (usize, u32, u32)> + '_ {
        self.planes.iter().enumerate().flat_map(move |(p, plane)| {
            let n = plane.mb_blocks();
            (0..n * n).map(move |i| (p, mx * n + i % n, my * n + i / n))
        })
    }
}

/// The output of entropy decoding: what a GPU decoder (or [`reconstruct`]) needs to rebuild
/// a frame from the previous one. Layout in `docs/features/motion-codec.md#coded-frames`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodedFrame {
    pub keyframe: bool,
    /// [`MB_WORDS`] words per macroblock, row-major: the kind (`MB_*`), then the motion
    /// vectors of its four 8x8 luma quadrants in row-major order, in quarter pels, x in the
    /// low and y in the high 16 bits (two's complement). All four are equal unless the kind
    /// is [`MB_SPLIT`]; they are zero for intra macroblocks.
    pub mbs: Vec<u32>,
    /// [`BLOCK_WORDS`] words per transform block with nonzero coefficients, in decoding
    /// order: its index in bits 0-24 and its coefficient count (1-64) in bits 25-31, then
    /// the position of its first coefficient in `coeffs`.
    pub blocks: Vec<u32>,
    /// The nonzero quantized coefficients (levels) of the blocks, block after block: the
    /// natural (row-major) position in bits 0-5, the level in the high 16 bits (two's
    /// complement). Reconstruction dequantizes them with [`CodedFrame::scales`].
    pub coeffs: Vec<u32>,
    /// Dequantization factor per plane, [`dequant_scale`] of the plane's `qp`.
    pub scales: [i32; 4],
}

impl CodedFrame {
    /// Kind and quadrant vectors of macroblock `i`.
    pub fn mb(&self, i: usize) -> (u32, [Mv; 4]) {
        let w = &self.mbs[i * MB_WORDS..(i + 1) * MB_WORDS];
        (
            w[0],
            [1, 2, 3, 4].map(|q| (w[q] as i16 as i32, (w[q] >> 16) as i16 as i32)),
        )
    }

    /// Appends a macroblock record.
    pub fn push_mb(&mut self, kind: u32, mvs: &[Mv; 4]) {
        self.mbs.push(kind);
        self.mbs.extend(
            mvs.iter()
                .map(|mv| (mv.0 as u16 as u32) | (mv.1 as u16 as u32) << 16),
        );
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len() / BLOCK_WORDS
    }

    /// Index and row-major levels of coded block `i`.
    pub fn block(&self, i: usize) -> (u32, [i32; 64]) {
        let (head, first) = (self.blocks[2 * i], self.blocks[2 * i + 1] as usize);
        let mut c = [0; 64];
        for &e in &self.coeffs[first..first + (head >> 25) as usize] {
            c[(e & 63) as usize] = (e >> 16) as i16 as i32;
        }
        (head & BLOCK_INDEX_MASK, c)
    }

    /// Appends a coded block from its index and row-major levels; nothing if they are all
    /// zero.
    pub fn push_block(&mut self, index: u32, coeffs: &[i32; 64]) {
        let first = self.coeffs.len();
        for (k, &c) in coeffs.iter().enumerate() {
            if c != 0 {
                self.coeffs.push(k as u32 | (c as u16 as u32) << 16);
            }
        }
        self.end_block(index, first);
    }

    /// Records block `index` whose coefficients were appended to `coeffs` from `first` on.
    #[inline]
    fn end_block(&mut self, index: u32, first: usize) {
        let count = (self.coeffs.len() - first) as u32;
        if count > 0 {
            self.blocks.extend([index | count << 25, first as u32]);
        }
    }

    pub fn clear(&mut self) {
        self.mbs.clear();
        self.blocks.clear();
        self.coeffs.clear();
    }
}

/// Bits of [`CodedFrame::blocks`] holding the block index.
pub const BLOCK_INDEX_MASK: u32 = (1 << 25) - 1;

/// The `qp` of plane `p` for a frame `qp`.
pub fn plane_qp(qp: u8, plane: usize, params: &MotionParams) -> u8 {
    let offset = match plane {
        0 => 0,
        3 => params.alpha_qp_offset,
        _ => params.chroma_qp_offset,
    };
    (qp as i32 + offset as i32).clamp(0, MAX_QP as i32) as u8
}

/// The dequantization factor of a `qp`.
pub fn dequant_scale(qp: u8) -> i32 {
    DEQUANT[qp as usize % 6] << (qp / 6)
}

/// A dequantized coefficient: the level times the factor of its plane, over 4, rounded
/// toward zero and clamped to 16 bits.
pub fn dequant(level: i32, scale: i32) -> i32 {
    let v = (level.abs() * scale) >> 2;
    (v * level.signum()).clamp(-32768, 32767)
}

/// A dequantized coefficient at `qp`.
pub fn dequantize(level: i32, qp: u8) -> i32 {
    dequant(level, dequant_scale(qp))
}

/// The plane of transform block `index`.
pub fn block_plane(layout: &Layout, index: u32) -> usize {
    layout
        .planes
        .iter()
        .rposition(|p| p.first_block <= index)
        .expect("block index")
}

/// The inverse transform of row-major coefficients: residuals, row-major.
pub fn inverse_transform(c: &[i32; 64]) -> [i32; 64] {
    let mut g = [0i32; 64];
    for u in 0..8 {
        for y in 0..8 {
            let e: i32 = (0..8).map(|v| DCT[v][y] * c[v * 8 + u]).sum();
            g[y * 8 + u] = ((e + 64) >> 7).clamp(-32768, 32767);
        }
    }
    let mut r = [0i32; 64];
    for y in 0..8 {
        for x in 0..8 {
            let e: i32 = (0..8).map(|u| DCT[u][x] * g[y * 8 + u]).sum();
            r[y * 8 + x] = (e + 2048) >> 12;
        }
    }
    r
}

/// Fraction bits of motion vectors in plane `plane`: quarter pels, eighth pels in 4:2:0
/// chroma.
pub fn mv_fraction_bits(plane: &Plane) -> u32 {
    2 + plane.shift
}

/// Interpolation filters of full-resolution planes by quarter-pel fraction: 8 taps at
/// offsets -3 to 4 from the integer position, summing to 64.
pub const FILTER_8: [[i32; 8]; 4] = [
    [0, 0, 0, 64, 0, 0, 0, 0],
    [-1, 4, -10, 58, 17, -5, 1, 0],
    [-1, 4, -11, 40, 40, -11, 4, -1],
    [0, 1, -5, 17, 58, -10, 4, -1],
];

/// Interpolation filters of 4:2:0 chroma by eighth-pel fraction: 4 taps at offsets -1 to 2.
pub const FILTER_4: [[i32; 4]; 8] = [
    [0, 64, 0, 0],
    [-2, 58, 10, -2],
    [-4, 54, 16, -2],
    [-6, 46, 28, -4],
    [-4, 36, 36, -4],
    [-4, 28, 46, -6],
    [-2, 16, 54, -4],
    [-2, 10, 58, -2],
];

/// The filter taps of `plane` for fraction `f`.
fn filter(plane: &Plane, f: i32) -> &'static [i32] {
    if plane.shift == 0 {
        &FILTER_8[f as usize]
    } else {
        &FILTER_4[f as usize]
    }
}

/// Motion-compensated prediction of pixel (`x`, `y`) of `plane` (the plane's pixels in
/// `reference`): the separable filter of the vector's fraction, coordinates clamped to the
/// plane, `(sum + 2048) >> 12` clamped to 0..255. The normative definition;
/// [`predict_block`] computes the same faster.
pub fn predict(reference: &[u8], plane: &Plane, x: i32, y: i32, mvx: i32, mvy: i32) -> u8 {
    let fb = mv_fraction_bits(plane);
    let s = 1 << fb;
    let (ix, iy) = (x + (mvx >> fb), y + (mvy >> fb));
    let (hx, hy) = (filter(plane, mvx & (s - 1)), filter(plane, mvy & (s - 1)));
    let o = hx.len() as i32 / 2 - 1;
    let (w, h) = (plane.width as i32, plane.height as i32);
    let at =
        |x: i32, y: i32| reference[(y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize] as i32;
    let mut v = 0;
    for (j, &cy) in hy.iter().enumerate() {
        let t: i32 = hx
            .iter()
            .enumerate()
            .map(|(i, &cx)| cx * at(ix - o + i as i32, iy - o + j as i32))
            .sum();
        v += cy * t;
    }
    ((v + 2048) >> 12).clamp(0, 255) as u8
}

/// [`predict`] of the `size` x `size` block at (`x0`, `y0`) into `out` (row-major).
pub fn predict_block(
    reference: &[u8],
    plane: &Plane,
    x0: i32,
    y0: i32,
    size: usize,
    mv: (i32, i32),
    out: &mut [u8],
) {
    let fb = mv_fraction_bits(plane);
    let s = 1 << fb;
    let (ix, iy) = (x0 + (mv.0 >> fb), y0 + (mv.1 >> fb));
    let (fx, fy) = (mv.0 & (s - 1), mv.1 & (s - 1));
    let (w, h) = (plane.width as i32, plane.height as i32);
    let row = |y: i32| &reference[(y.clamp(0, h - 1) * w) as usize..][..w as usize];
    let inside = ix >= 4 && iy >= 4 && ix + size as i32 + 4 <= w && iy + size as i32 + 4 <= h;
    if fx == 0 && fy == 0 {
        for y in 0..size {
            let r = row(iy + y as i32);
            for x in 0..size {
                out[y * size + x] = if inside {
                    r[ix as usize + x]
                } else {
                    r[(ix + x as i32).clamp(0, w - 1) as usize]
                };
            }
        }
        return;
    }
    let (hx, hy) = (filter(plane, fx), filter(plane, fy));
    let taps = hx.len();
    let o = taps as i32 / 2 - 1;
    // Horizontal pass over the rows the vertical taps need.
    let mut tmp = [0i32; (16 + 7) * 16];
    for j in 0..size + taps - 1 {
        let r = row(iy - o + j as i32);
        for x in 0..size {
            let x = x as i32;
            let mut t = 0;
            for (i, &c) in hx.iter().enumerate() {
                let sx = ix + x - o + i as i32;
                let p = if inside {
                    r[sx as usize]
                } else {
                    r[sx.clamp(0, w - 1) as usize]
                };
                t += c * p as i32;
            }
            tmp[j * size + x as usize] = t;
        }
    }
    for y in 0..size {
        for x in 0..size {
            let v: i32 = hy
                .iter()
                .enumerate()
                .map(|(j, &c)| c * tmp[(y + j) * size + x])
                .sum();
            out[y * size + x] = ((v + 2048) >> 12).clamp(0, 255) as u8;
        }
    }
}

/// Prediction of transform block (`bx`, `by`) of `plane` (the plane's pixels in
/// `reference`) in a macroblock with quadrant vectors `mvs`: each quadrant of the
/// macroblock predicts its pixels with its own vector.
pub fn predict_transform_block(
    reference: &[u8],
    plane: &Plane,
    bx: u32,
    by: u32,
    mvs: &[Mv; 4],
    out: &mut [u8; 64],
) {
    let (x0, y0) = ((bx * 8) as i32, (by * 8) as i32);
    if plane.shift == 0 {
        let q = (by % 2 * 2 + bx % 2) as usize;
        predict_block(reference, plane, x0, y0, 8, mvs[q], out);
        return;
    }
    // A 4:2:0 chroma block covers the whole macroblock: one 4x4 part per quadrant.
    let mut part = [0u8; 16];
    for (q, mv) in mvs.iter().enumerate() {
        let (qx, qy) = (q % 2 * 4, q / 2 * 4);
        predict_block(
            reference,
            plane,
            x0 + qx as i32,
            y0 + qy as i32,
            4,
            *mv,
            &mut part,
        );
        for y in 0..4 {
            out[(qy + y) * 8 + qx..][..4].copy_from_slice(&part[y * 4..][..4]);
        }
    }
}

/// Rebuilds a frame into `out` (a frame buffer of `layout.size` bytes) from the previous
/// frame `reference` (ignored for keyframes, which hold only intra macroblocks).
pub fn reconstruct(layout: &Layout, frame: &CodedFrame, reference: &[u8], out: &mut [u8]) {
    let mut block = [0u8; 64];
    for my in 0..layout.mbs_y {
        for mx in 0..layout.mbs_x {
            let (kind, mvs) = frame.mb((my * layout.mbs_x + mx) as usize);
            for (p, bx, by) in layout.mb_blocks(mx, my) {
                let plane = &layout.planes[p];
                if kind == MB_INTRA {
                    block.fill(128);
                } else {
                    predict_transform_block(
                        &reference[plane.offset..],
                        plane,
                        bx,
                        by,
                        &mvs,
                        &mut block,
                    );
                }
                for y in 0..8 {
                    let row = plane.offset + ((by * 8 + y) * plane.width + bx * 8) as usize;
                    out[row..row + 8].copy_from_slice(&block[y as usize * 8..][..8]);
                }
            }
        }
    }
    for i in 0..frame.block_count() {
        let (index, mut coeffs) = frame.block(i);
        let scale = frame.scales[block_plane(layout, index)];
        for c in coeffs.iter_mut() {
            *c = dequant(*c, scale);
        }
        add_residual(layout, index, &inverse_transform(&coeffs), out);
    }
}

/// Adds the residual of transform block `index` to the frame buffer, clamping to 0..255.
pub fn add_residual(layout: &Layout, index: u32, residual: &[i32; 64], out: &mut [u8]) {
    let plane = &layout.planes[block_plane(layout, index)];
    let b = index - plane.first_block;
    let (bx, by) = (b % plane.blocks_x(), b / plane.blocks_x());
    for y in 0..8 {
        let row = plane.offset + ((by * 8 + y) * plane.width + bx * 8) as usize;
        for x in 0..8 {
            let p = &mut out[row + x as usize];
            *p = (*p as i32 + residual[(y * 8 + x) as usize]).clamp(0, 255) as u8;
        }
    }
}

/// Converts a frame buffer to `width * height * 4` bytes of RGBA (sRGB-encoded, straight
/// alpha, 255 without an alpha plane).
pub fn to_rgba(layout: &Layout, frame: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(layout.width as usize * layout.height as usize * 4);
    let [py, pco, pcg] = [layout.planes[0], layout.planes[1], layout.planes[2]];
    for y in 0..layout.height {
        for x in 0..layout.width {
            let luma = frame[py.offset + (y * py.width + x) as usize] as i32;
            let co = chroma_sample(frame, &pco, x, y) - 128;
            let cg = chroma_sample(frame, &pcg, x, y) - 128;
            let t = luma - cg;
            let a = layout
                .planes
                .get(3)
                .map_or(255, |pa| frame[pa.offset + (y * pa.width + x) as usize]);
            out.extend_from_slice(&[
                (t + co).clamp(0, 255) as u8,
                (luma + cg).clamp(0, 255) as u8,
                (t - co).clamp(0, 255) as u8,
                a,
            ]);
        }
    }
}

/// A chroma value at luma position (`x`, `y`): the sample itself in 4:4:4, bilinear with
/// weights 9/3/3/1 in 4:2:0 (samples centered in their 2x2 luma pixels).
#[inline]
pub fn chroma_sample(frame: &[u8], plane: &Plane, x: u32, y: u32) -> i32 {
    let at = |x: i32, y: i32| {
        let (x, y) = (
            x.clamp(0, plane.width as i32 - 1),
            y.clamp(0, plane.height as i32 - 1),
        );
        frame[plane.offset + (y * plane.width as i32 + x) as usize] as i32
    };
    if plane.shift == 0 {
        return at(x as i32, y as i32);
    }
    // The nearer sample gets weight 3, the other 1, per axis.
    let taps = |v: u32| {
        let i = (v >> 1) as i32;
        if v & 1 == 0 {
            (i - 1, 1, i, 3)
        } else {
            (i, 3, i + 1, 1)
        }
    };
    let (xa, wxa, xb, wxb) = taps(x);
    let (ya, wya, yb, wyb) = taps(y);
    (wxa * wya * at(xa, ya)
        + wxb * wya * at(xb, ya)
        + wxa * wyb * at(xa, yb)
        + wxb * wyb * at(xb, yb)
        + 8)
        >> 4
}

/// The class of a value: the bits of its magnitude (0 for 0).
#[inline]
pub fn class(v: i32) -> u32 {
    32 - v.unsigned_abs().leading_zeros()
}

/// The extra bits of a value of class `c` (JPEG convention: negative values are offset).
#[inline]
pub fn extra_bits(v: i32, c: u32) -> u32 {
    if v >= 0 {
        v as u32
    } else {
        (v + (1 << c) - 1) as u32
    }
}

#[inline]
fn from_extra_bits(bits: u32, c: u32) -> i32 {
    if c == 0 {
        0
    } else if bits >> (c - 1) != 0 {
        bits as i32
    } else {
        bits as i32 - (1 << c) + 1
    }
}

/// Median of three.
fn median(a: i32, b: i32, c: i32) -> i32 {
    a.max(b).min(a.min(b).max(c))
}

/// Motion vector predictor of a partition of macroblock (`mx`, `my`) in a slice starting
/// at macroblock row `first_row`: the 16x16 partition (`sub` 0, `width` 2) or 8x8 quadrant
/// `sub` (`width` 1). `field` holds the vectors decoded so far per 8x8 luma block, `2 *
/// mbs_x` per row (intra macroblocks are zero). Neighbours, at 8x8 granularity: A left, B
/// above, C above-right of the partition, or D above-left when C is not decoded yet; only
/// blocks of the slice decoded before the partition count. One available neighbour gives
/// its vector, none gives zero, otherwise the median with missing neighbours as zero.
pub fn mv_predictor(
    field: &[Mv],
    mbs_x: u32,
    first_row: u32,
    mx: u32,
    my: u32,
    sub: u32,
    width: u32,
) -> Mv {
    let bw = 2 * mbs_x as i32;
    let current = (my * mbs_x + mx) as i32;
    let (bx, by) = ((2 * mx + sub % 2) as i32, (2 * my + sub / 2) as i32);
    let at = |x: i32, y: i32| -> Option<Mv> {
        if x < 0 || x >= bw || y < 2 * first_row as i32 {
            return None;
        }
        let mb = y / 2 * mbs_x as i32 + x / 2;
        let decoded = mb < current || mb == current && ((y % 2 * 2 + x % 2) as u32) < sub;
        decoded.then(|| field[(y * bw + x) as usize])
    };
    let left = at(bx - 1, by);
    let above = at(bx, by - 1);
    let diag = at(bx + width as i32, by - 1).or_else(|| at(bx - 1, by - 1));
    match (left, above, diag) {
        (Some(v), None, None) | (None, Some(v), None) | (None, None, Some(v)) => v,
        (a, b, c) => {
            let (a, b, c) = (
                a.unwrap_or_default(),
                b.unwrap_or_default(),
                c.unwrap_or_default(),
            );
            (median(a.0, b.0, c.0), median(a.1, b.1, c.1))
        }
    }
}

/// Sets the vectors of quadrants `subs` of macroblock (`mx`, `my`) in an 8x8 vector field.
pub fn set_field(
    field: &mut [Mv],
    mbs_x: u32,
    mx: u32,
    my: u32,
    subs: std::ops::Range<u32>,
    mv: Mv,
) {
    for sub in subs {
        let (bx, by) = (2 * mx + sub % 2, 2 * my + sub / 2);
        field[(by * 2 * mbs_x + bx) as usize] = mv;
    }
}

/// DC and AC tables of plane `p`.
pub fn plane_tables(p: usize) -> (usize, usize) {
    match p {
        0 => (TABLE_DC_Y, TABLE_AC_Y),
        3 => (TABLE_DC_A, TABLE_AC_A),
        _ => (TABLE_DC_C, TABLE_AC_C),
    }
}

/// Size of the fixed part of the frame header.
const HEADER_SIZE: usize = 4;
/// Bytes of one table's code lengths.
const LENGTHS_SIZE: usize = 128;

/// Most threads a [`Decoder`] uses by default, leaving cores to the game.
pub const DEFAULT_THREADS: usize = 8;

/// Most frames decoding ahead at once ([`Decoder::decode_ahead`]).
pub const MAX_AHEAD: usize = 4;

/// Turns motion payloads into coded frames. Keeps the Huffman tables, which delta frames may
/// inherit from the previous frame, and the threads that decode slices in parallel.
pub struct Decoder {
    /// What the slices of a frame share. Only the decoder holds it between frames.
    frame: Arc<Frame>,
    params: MotionParams,
    /// Coded frame and vector field of each slice, reused between frames.
    parts: Vec<Part>,
    /// Most threads decoding the slices of one frame, the calling thread included.
    threads: usize,
    pool: Option<Pool>,
    /// A keyframe was decoded since the start, so delta frames have tables and a reference.
    primed: bool,
    /// The frames decoding on the workers ahead of time, in decoding order.
    ahead: VecDeque<Ahead>,
    /// Slots of finished decode-aheads, reused by the next ones.
    spare: Vec<Slot>,
}

/// What the slices of a decode-ahead read, swapped with the decoder's frame when it
/// finishes, and where they put their outputs.
struct Slot {
    frame: Arc<Frame>,
    assembly: Arc<Mutex<Assembly>>,
}

/// The output of a decode-ahead, put together by the threads that decode its slices: each
/// appends the finished slices that follow the output so far, so the frame is complete
/// when the last slice is.
#[derive(Default)]
struct Assembly {
    parts: Vec<Part>,
    /// Which slices are done.
    back: Vec<bool>,
    /// Slices appended to `out`.
    appended: usize,
    out: CodedFrame,
    first_error: Option<(usize, String)>,
}

impl Assembly {
    fn start(&mut self, slices: usize) {
        self.parts.resize_with(slices, Part::default);
        self.back.clear();
        self.back.resize(slices, false);
        self.appended = 0;
        self.first_error = None;
    }

    fn add(&mut self, s: usize, part: Part, result: Result<(), String>) {
        if let Err(e) = result
            && self
                .first_error
                .as_ref()
                .is_none_or(|(first, _)| s < *first)
        {
            self.first_error = Some((s, e));
        }
        self.parts[s] = part;
        self.back[s] = true;
        while self.first_error.is_none()
            && self.appended < self.parts.len()
            && self.back[self.appended]
        {
            if self.appended == 0 {
                std::mem::swap(&mut self.parts[0].out, &mut self.out);
            } else {
                append(&mut self.out, &self.parts[self.appended].out);
            }
            self.appended += 1;
        }
    }
}

/// A frame started by [`Decoder::decode_ahead`].
struct Ahead {
    tag: u64,
    keyframe: bool,
    qp: u8,
    results: mpsc::Receiver<Job>,
    slot: Slot,
}

/// The parts of a frame that its slices read.
struct Frame {
    layout: Layout,
    tables: [Option<Box<[u16; TABLE_SIZE]>>; TABLES],
    keyframe: bool,
    /// The payload, copied when slices decode on other threads.
    payload: Vec<u8>,
    /// Byte range of each slice in `payload`.
    slices: Vec<std::ops::Range<usize>>,
}

impl Frame {
    fn new(layout: Layout) -> Frame {
        Frame {
            layout,
            tables: Default::default(),
            keyframe: false,
            payload: Vec::new(),
            slices: Vec::new(),
        }
    }

    fn tables(&self) -> [&[u16; TABLE_SIZE]; TABLES] {
        std::array::from_fn(|t| self.tables[t].as_deref().unwrap_or(&EMPTY_TABLE))
    }

    /// Reads the header, tables and slice sizes of `payload`; returns its `qp`. A keyframe
    /// replaces every table, a delta frame keeps the tables it does not send.
    fn parse(&mut self, keyframe: bool, payload: &[u8]) -> Result<u8, String> {
        if payload.len() < HEADER_SIZE {
            return Err("motion payload shorter than its header".into());
        }
        let layout = &self.layout;
        let (qp, slices, mask) = (payload[0], payload[1] as u32, payload[2]);
        if qp > MAX_QP {
            return Err(format!("qp {qp} out of range"));
        }
        if slices == 0 || slices > layout.mbs_y {
            return Err(format!(
                "{slices} slices for {} macroblock rows",
                layout.mbs_y
            ));
        }
        if !layout.alpha() && mask & (1 << TABLE_DC_A | 1 << TABLE_AC_A) != 0 {
            return Err("alpha tables without an alpha plane".into());
        }
        let mut at = HEADER_SIZE;
        for t in 0..TABLES {
            if mask & 1 << t == 0 {
                if keyframe {
                    self.tables[t] = None;
                }
                continue;
            }
            let Some(bytes) = payload.get(at..at + LENGTHS_SIZE) else {
                return Err("motion payload ends in its tables".into());
            };
            at += LENGTHS_SIZE;
            let mut lens = [0u8; 256];
            for (i, &b) in bytes.iter().enumerate() {
                lens[2 * i] = b & 15;
                lens[2 * i + 1] = b >> 4;
            }
            self.tables[t] = single_table(&lens)?.map(Box::new);
        }
        let Some(sizes) = payload.get(at..at + 4 * slices as usize) else {
            return Err("motion payload ends in its slice sizes".into());
        };
        at += 4 * slices as usize;
        self.slices.clear();
        for s in 0..slices as usize {
            let len =
                u32::from_le_bytes(sizes[4 * s..4 * s + 4].try_into().expect("4 bytes")) as usize;
            if payload.len() - at < len {
                return Err(format!("slice {s} runs past the payload"));
            }
            self.slices.push(at..at + len);
            at += len;
        }
        if at != payload.len() {
            return Err("motion payload has bytes after its last slice".into());
        }
        self.keyframe = keyframe;
        Ok(qp)
    }

    /// Decodes slice `s` of `payload` into `part`.
    fn decode_slice(&self, payload: &[u8], s: usize, part: &mut Part) -> Result<(), String> {
        let rows = self.layout.slice_rows(s as u32, self.slices.len() as u32);
        part.field
            .resize(4 * rows.len() * self.layout.mbs_x as usize, (0, 0));
        let data = &payload[self.slices[s].clone()];
        decode_slice(
            &self.layout,
            &self.tables(),
            self.keyframe,
            rows,
            data,
            &mut part.field,
            &mut part.out,
        )
        .map_err(|e| format!("slice {s}: {e}"))
    }
}

/// The output of one slice. Each slice has its own rows of the 8x8 vector field, since
/// predictions never look outside the slice.
#[derive(Default)]
struct Part {
    out: CodedFrame,
    field: Vec<Mv>,
}

/// A slice to decode on any thread; it comes back through `done`.
struct Job {
    s: usize,
    frame: Arc<Frame>,
    part: Part,
    result: Result<(), String>,
    /// Where the output goes for a decode-ahead; otherwise it comes back with the job.
    assembly: Option<Arc<Mutex<Assembly>>>,
    done: mpsc::Sender<Job>,
}

impl Job {
    fn run(mut self) {
        let frame = Arc::clone(&self.frame);
        self.result = frame.decode_slice(&frame.payload, self.s, &mut self.part);
        drop(frame);
        if let Some(assembly) = &self.assembly {
            let result = std::mem::replace(&mut self.result, Ok(()));
            assembly.lock().expect("assembly lock").add(
                self.s,
                std::mem::take(&mut self.part),
                result,
            );
        }
        // The decoder waits for every job, so it is still listening.
        let _ = self.done.clone().send(self);
    }
}

/// Worker threads waiting for slices.
struct Pool {
    shared: Arc<Shared>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    work: Condvar,
}

#[derive(Default)]
struct Queue {
    jobs: std::collections::VecDeque<Job>,
    exit: bool,
}

impl Shared {
    /// The next queued slice of `frame`.
    fn pop_for(&self, frame: &Arc<Frame>) -> Option<Job> {
        let mut queue = self.queue.lock().expect("queue lock");
        let i = queue
            .jobs
            .iter()
            .position(|j| Arc::ptr_eq(&j.frame, frame))?;
        queue.jobs.remove(i)
    }

    /// Queues the slices of `frame`, taking their outputs from `parts`, and wakes `workers`.
    fn push(
        &self,
        frame: &Arc<Frame>,
        parts: &mut [Part],
        assembly: Option<&Arc<Mutex<Assembly>>>,
        done: &mpsc::Sender<Job>,
        workers: usize,
    ) {
        {
            let mut queue = self.queue.lock().expect("queue lock");
            for (s, part) in parts.iter_mut().enumerate() {
                queue.jobs.push_back(Job {
                    s,
                    frame: Arc::clone(frame),
                    part: std::mem::take(part),
                    result: Ok(()),
                    assembly: assembly.cloned(),
                    done: done.clone(),
                });
            }
        }
        for _ in 0..workers {
            self.work.notify_one();
        }
    }
}

impl Pool {
    fn new() -> Pool {
        Pool {
            shared: Arc::default(),
            workers: Vec::new(),
        }
    }

    /// Starts workers until there are `n`.
    fn grow(&mut self, n: usize) {
        while self.workers.len() < n {
            let shared = Arc::clone(&self.shared);
            self.workers.push(std::thread::spawn(move || {
                loop {
                    let mut queue = shared.queue.lock().expect("queue lock");
                    let job = loop {
                        if queue.exit {
                            return;
                        }
                        if let Some(job) = queue.jobs.pop_front() {
                            break job;
                        }
                        queue = shared.work.wait(queue).expect("queue lock");
                    };
                    drop(queue);
                    job.run();
                }
            }));
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shared.queue.lock().expect("queue lock").exit = true;
        self.shared.work.notify_all();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

impl Decoder {
    pub fn new(params: &VideoParams) -> Decoder {
        Decoder {
            frame: Arc::new(Frame::new(Layout::new(params))),
            params: params.motion,
            parts: Vec::new(),
            threads: std::thread::available_parallelism()
                .map_or(1, |n| n.get().min(DEFAULT_THREADS)),
            pool: None,
            primed: false,
            ahead: VecDeque::new(),
            spare: Vec::new(),
        }
    }

    /// Sets the most threads decoding the slices of one frame, the calling thread included;
    /// 1 decodes on the calling thread only. Worker threads start when first needed.
    /// Cancels the frames decoding ahead.
    pub fn set_threads(&mut self, threads: usize) {
        self.cancel_ahead();
        self.threads = threads.max(1);
        if self
            .pool
            .as_ref()
            .is_some_and(|p| p.workers.len() >= self.threads)
        {
            self.pool = None;
        }
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    pub fn layout(&self) -> &Layout {
        &self.frame.layout
    }

    /// Decodes one payload into `out`.
    pub fn decode(
        &mut self,
        keyframe: bool,
        payload: &[u8],
        out: &mut CodedFrame,
    ) -> Result<(), String> {
        self.cancel_ahead();
        if !keyframe && !self.primed {
            return Err("delta frame without a preceding keyframe".into());
        }
        let frame =
            Arc::get_mut(&mut self.frame).expect("slice jobs hold no frame between decodes");
        let qp = frame.parse(keyframe, payload)?;
        let slices = frame.slices.len();
        out.keyframe = keyframe;
        self.set_scales(qp, out);
        // Slice 0 decodes into `out`'s buffers, the others into their parts, appended to
        // `out` in order.
        self.parts.resize_with(slices, Part::default);
        std::mem::swap(&mut self.parts[0].out, out);
        let threads = self.threads.min(slices);
        let result = if threads == 1 {
            (0..slices).try_for_each(|s| self.frame.decode_slice(payload, s, &mut self.parts[s]))
        } else {
            self.decode_parallel(payload, threads, out)
        };
        if threads == 1 || result.is_err() {
            std::mem::swap(&mut self.parts[0].out, out);
        }
        result?;
        if threads == 1 {
            for part in &self.parts[1..] {
                append(out, &part.out);
            }
        }
        self.primed = true;
        Ok(())
    }

    /// Decodes the slices on the calling thread and `threads - 1` workers, each taking the
    /// next slice in order, and appends them to `out` (which holds slice 0's buffers in
    /// `parts[0]`). Returns the error of the first failing slice.
    fn decode_parallel(
        &mut self,
        payload: &[u8],
        threads: usize,
        out: &mut CodedFrame,
    ) -> Result<(), String> {
        let frame =
            Arc::get_mut(&mut self.frame).expect("slice jobs hold no frame between decodes");
        frame.payload.clear();
        frame.payload.extend_from_slice(payload);
        let pool = self.pool.get_or_insert_with(Pool::new);
        pool.grow(threads - 1);
        let (done, results) = mpsc::channel();
        let slices = self.parts.len();
        pool.shared
            .push(&self.frame, &mut self.parts, None, &done, threads - 1);
        drop(done);
        // Finished slices are appended while others still decode, as soon as the slices
        // before them are in.
        let (mut back, mut appended) = (vec![false; slices], 0);
        let mut first_error: Option<(usize, String)> = None;
        let mut collect = |job: Job, parts: &mut [Part], out: &mut CodedFrame| {
            if let Err(e) = job.result
                && first_error.as_ref().is_none_or(|(s, _)| job.s < *s)
            {
                first_error = Some((job.s, e));
            }
            parts[job.s] = job.part;
            back[job.s] = true;
            while first_error.is_none() && appended < slices && back[appended] {
                if appended == 0 {
                    std::mem::swap(&mut parts[0].out, out);
                } else {
                    append(out, &parts[appended].out);
                }
                appended += 1;
            }
        };
        // Slices of a running decode-ahead are left to the workers.
        let mut received = 0;
        while let Some(job) = pool.shared.pop_for(&self.frame) {
            job.run();
            for job in results.try_iter() {
                collect(job, &mut self.parts, out);
                received += 1;
            }
        }
        for _ in received..slices {
            let job = results.recv().expect("a slice thread panicked");
            collect(job, &mut self.parts, out);
        }
        match first_error {
            // Slice 0's buffers go back to `parts[0]`, where `decode` expects them on errors.
            Some((_, e)) => {
                if appended > 0 {
                    std::mem::swap(&mut self.parts[0].out, out);
                }
                Err(e)
            }
            None => Ok(()),
        }
    }

    fn set_scales(&self, qp: u8, out: &mut CodedFrame) {
        for (p, scale) in out
            .scales
            .iter_mut()
            .enumerate()
            .take(self.frame.layout.planes.len())
        {
            *scale = dequant_scale(plane_qp(qp, p, &self.params));
        }
    }

    /// Starts entropy-decoding `payload` on the worker threads and returns without waiting;
    /// [`Decoder::finish_ahead`] with the same `tag` collects it. Frames queue in decoding
    /// order, up to [`MAX_AHEAD`]; a delta frame inherits the tables of the frame queued
    /// before it, or of the latest decoded frame. Returns `false`, starting nothing, with one
    /// thread, for a delta frame before any keyframe, when the queue is full, or for an
    /// invalid header; frames after a refused one must not be queued.
    pub fn decode_ahead(&mut self, tag: u64, keyframe: bool, payload: &[u8]) -> bool {
        if self.threads == 1
            || self.ahead.len() >= MAX_AHEAD
            || !keyframe && !self.primed && self.ahead.is_empty()
        {
            return false;
        }
        let mut slot = self.spare.pop().unwrap_or_else(|| Slot {
            frame: Arc::new(Frame::new(self.frame.layout.clone())),
            assembly: Arc::default(),
        });
        let frame = Arc::get_mut(&mut slot.frame).expect("spare slots are not shared");
        if !keyframe {
            let latest = self.ahead.back().map_or(&self.frame, |a| &a.slot.frame);
            for (table, latest) in frame.tables.iter_mut().zip(&latest.tables) {
                table.clone_from(latest);
            }
        }
        let Ok(qp) = frame.parse(keyframe, payload) else {
            self.spare.push(slot);
            return false;
        };
        frame.payload.clear();
        frame.payload.extend_from_slice(payload);
        let slices = frame.slices.len();
        let mut assembly = slot.assembly.lock().expect("assembly lock");
        assembly.start(slices);
        let pool = self.pool.get_or_insert_with(Pool::new);
        pool.grow(self.threads - 1);
        let (done, results) = mpsc::channel();
        let workers = (self.threads - 1).min(slices);
        pool.shared.push(
            &slot.frame,
            &mut assembly.parts,
            Some(&slot.assembly),
            &done,
            workers,
        );
        drop(assembly);
        self.ahead.push_back(Ahead {
            tag,
            keyframe,
            qp,
            results,
            slot,
        });
        true
    }

    /// Frames decoding ahead.
    pub fn ahead(&self) -> usize {
        self.ahead.len()
    }

    /// Collects the first frame decoding ahead into `out` if it was started with `tag`,
    /// decoding the slices no worker has started on the calling thread; the result equals
    /// `decode(keyframe, payload, out)`. `None` when the first frame has another tag or none
    /// is decoding ahead. An error cancels the frames after it.
    pub fn finish_ahead(&mut self, tag: u64, out: &mut CodedFrame) -> Option<Result<(), String>> {
        if self.ahead.front().is_none_or(|a| a.tag != tag) {
            return None;
        }
        let Ahead {
            keyframe,
            qp,
            mut slot,
            ..
        } = {
            let ahead = self.ahead.pop_front().expect("checked above");
            if let Err(e) = self.wait_ahead(&ahead, true) {
                self.spare.push(ahead.slot);
                self.cancel_ahead();
                // The delta frames after a broken frame have no tables to decode with.
                self.primed = false;
                return Some(Err(e));
            }
            ahead
        };
        let mut assembly = slot.assembly.lock().expect("assembly lock");
        std::mem::swap(&mut assembly.out, out);
        std::mem::swap(&mut assembly.parts, &mut self.parts);
        drop(assembly);
        std::mem::swap(&mut self.frame, &mut slot.frame);
        self.spare.push(slot);
        out.keyframe = keyframe;
        self.set_scales(qp, out);
        self.primed = true;
        Some(Ok(()))
    }

    /// Discards every frame decoding ahead: slices no worker has started are dropped, the
    /// others waited for.
    fn cancel_ahead(&mut self) {
        while let Some(ahead) = self.ahead.pop_front() {
            let _ = self.wait_ahead(&ahead, false);
            self.spare.push(ahead.slot);
        }
    }

    /// Decodes the queued slices of `ahead` on the calling thread (or drops them unless
    /// `run`) and waits for the others. Returns the error of the first failing slice.
    fn wait_ahead(&self, ahead: &Ahead, run: bool) -> Result<(), String> {
        let pool = self.pool.as_ref().expect("a decode-ahead runs on the pool");
        let mut left = ahead.slot.frame.slices.len();
        while let Some(job) = pool.shared.pop_for(&ahead.slot.frame) {
            if run {
                job.run();
            } else {
                left -= 1;
            }
        }
        for _ in 0..left {
            ahead.results.recv().expect("a slice thread panicked");
        }
        let first_error = ahead
            .slot
            .assembly
            .lock()
            .expect("assembly lock")
            .first_error
            .take();
        match first_error {
            Some((_, e)) if run => Err(e),
            _ => Ok(()),
        }
    }
}

/// Appends the coded frame of a later slice to `out`.
fn append(out: &mut CodedFrame, part: &CodedFrame) {
    out.mbs.extend_from_slice(&part.mbs);
    let base = out.coeffs.len() as u32;
    out.coeffs.extend_from_slice(&part.coeffs);
    out.blocks.reserve(part.blocks.len());
    for b in part.blocks.as_chunks::<BLOCK_WORDS>().0 {
        out.blocks.extend([b[0], b[1] + base]);
    }
}

/// Decodes the macroblock `rows` of one slice from `data` into `out`; `field` holds the
/// slice's rows of the 8x8 vector field.
fn decode_slice(
    layout: &Layout,
    tables: &[&[u16; TABLE_SIZE]; TABLES],
    keyframe: bool,
    rows: std::ops::Range<u32>,
    data: &[u8],
    field: &mut [Mv],
    out: &mut CodedFrame,
) -> Result<(), String> {
    out.clear();
    let mut r = BitReader::new(data);
    let mut dc_pred = [0i32; 4];
    let planes: Vec<_> = (0..layout.planes.len())
        .map(|p| {
            let (dc, ac) = plane_tables(p);
            (tables[dc], tables[ac])
        })
        .collect();
    let first_row = rows.start;
    let mbs_x = layout.mbs_x;
    let mut buf = [0u32; 64];
    for my in rows {
        for mx in 0..mbs_x {
            let sym = r.symbol(tables[TABLE_MB])?;
            let kind = sym & 3;
            let (cbp, chroma, alpha) = (sym >> 2 & 15, sym >> 6 & 1 != 0, sym >> 7 != 0);
            if keyframe && kind != MB_INTRA || kind == MB_SKIP && sym != 0 {
                return Err(format!("invalid macroblock symbol {sym:#04x}"));
            }
            if alpha && !layout.alpha() {
                return Err("alpha blocks without an alpha plane".into());
            }
            let mut mvs = [(0, 0); 4];
            match kind {
                MB_SPLIT => {
                    for (sub, mv) in (0..4).zip(mvs.iter_mut()) {
                        let pred = mv_predictor(field, mbs_x, 0, mx, my - first_row, sub, 1);
                        *mv = r.mv(tables[TABLE_MV], pred)?;
                        set_field(field, mbs_x, mx, my - first_row, sub..sub + 1, *mv);
                    }
                }
                MB_INTRA => set_field(field, mbs_x, mx, my - first_row, 0..4, (0, 0)),
                _ => {
                    let pred = mv_predictor(field, mbs_x, 0, mx, my - first_row, 0, 2);
                    let mv = if kind == MB_INTER {
                        r.mv(tables[TABLE_MV], pred)?
                    } else {
                        pred
                    };
                    mvs = [mv; 4];
                    set_field(field, mbs_x, mx, my - first_row, 0..4, mv);
                }
            }
            out.push_mb(kind, &mvs);
            if kind == MB_SKIP {
                continue;
            }
            for (p, plane) in layout.planes.iter().enumerate() {
                let n = plane.mb_blocks();
                for i in 0..n * n {
                    let coded = match p {
                        0 => cbp >> i & 1 != 0,
                        3 => alpha,
                        _ => chroma,
                    };
                    if !coded {
                        continue;
                    }
                    let (dc_table, ac_table) = planes[p];
                    let (bx, by) = (mx * n + i % n, my * n + i / n);
                    let index = plane.first_block + by * plane.blocks_x() + bx;
                    let first = out.coeffs.len();
                    let mut dc = r.value(dc_table)?;
                    if kind == MB_INTRA {
                        dc += dc_pred[p];
                        dc_pred[p] = dc;
                    }
                    let mut count = 0;
                    if dc != 0 {
                        buf[0] = (check_level(dc)? as u16 as u32) << 16;
                        count = 1;
                    }
                    count = ac(&mut r, ac_table, &mut buf, count)?;
                    out.coeffs.extend_from_slice(&buf[..count]);
                    out.end_block(index, first);
                }
            }
        }
    }
    if r.consumed_bits() > data.len() * 8 {
        return Err("slice data ends early".into());
    }
    Ok(())
}

/// The table of an absent Huffman table: every entry invalid (length 0).
static EMPTY_TABLE: [u16; TABLE_SIZE] = [0; TABLE_SIZE];
#[inline(always)]
fn check_level(level: i32) -> Result<i32, String> {
    if level.abs() > MAX_LEVEL {
        return Err(level_error(level));
    }
    Ok(level)
}

#[cold]
fn level_error(level: i32) -> String {
    format!("coefficient level {level} out of range")
}

/// The AC levels of a block, appended to `out[..n]` as [`CodedFrame::coeffs`] entries
/// from `c` with the AC table `table`; returns the new count.
#[inline(always)]
fn ac(
    c: &mut BitReader,
    table: &[u16; TABLE_SIZE],
    out: &mut [u32; 64],
    mut n: usize,
) -> Result<usize, String> {
    let mut k = 1;
    loop {
        // A token takes at most 11 + 15 bits. Refilling every token is cheaper than a
        // data-dependent branch.
        c.refill();
        let entry = table[(c.acc & (TABLE_SIZE as u64 - 1)) as usize];
        let len = (entry & 15) as u32;
        let (run, size) = ((entry >> 12) as usize, (entry >> 8 & 15) as u32);
        c.acc >>= len;
        c.bits -= len;
        if size == 0 {
            if len == 0 {
                return Err(empty_table_error());
            }
            match run {
                0 => return Ok(n),
                15 if k + 16 < 64 => {
                    k += 16;
                    continue;
                }
                _ => return Err(coefficient_error(entry >> 8)),
            }
        }
        k += run;
        if k >= 64 {
            return Err("coefficients run past the block".into());
        }
        let bits = (c.acc & ((1 << size) - 1)) as i32;
        c.acc >>= size;
        c.bits -= size;
        // Values with a clear top bit are negative (JPEG convention).
        let negative = (bits >> (size - 1)) ^ 1;
        let level = bits - negative * ((1 << size) - 1);
        if size == 15 {
            check_level(level)?;
        }
        out[n & 63] = ZIGZAG[k] as u32 | (level as u16 as u32) << 16;
        n += 1;
        k += 1;
        // A block ending at the last position has no end-of-block token.
        if k == 64 {
            return Ok(n);
        }
    }
}

#[cold]
fn coefficient_error(sym: u16) -> String {
    format!("invalid coefficient symbol {sym:#04x}")
}

#[cold]
fn empty_table_error() -> String {
    "symbol from an empty table".into()
}

/// Least-significant-bit-first bit reader; bytes past the end read as zero.
#[derive(Clone, Copy)]
struct BitReader<'a> {
    src: &'a [u8],
    pos: usize,
    acc: u64,
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

    /// Fills the accumulator to at least 56 bits.
    #[inline(always)]
    fn refill(&mut self) {
        if self.pos + 8 <= self.src.len() {
            let word = u64::from_le_bytes(
                self.src[self.pos..self.pos + 8]
                    .try_into()
                    .expect("8 bytes"),
            );
            self.acc |= word << self.bits;
            self.pos += ((63 - self.bits) >> 3) as usize;
            self.bits |= 56;
        } else {
            self.refill_end();
        }
    }

    /// [`BitReader::refill`] in the last 8 bytes, kept out of the decoding loops.
    #[cold]
    #[inline(never)]
    fn refill_end(&mut self) {
        while self.bits <= 56 {
            let byte = self.src.get(self.pos).copied().unwrap_or(0);
            self.acc |= (byte as u64) << self.bits;
            self.pos += 1;
            self.bits += 8;
        }
    }

    /// A symbol of a Huffman table.
    #[inline(always)]
    fn symbol(&mut self, table: &[u16; TABLE_SIZE]) -> Result<u32, String> {
        if self.bits < 16 {
            self.refill();
        }
        let entry = table[(self.acc & (TABLE_SIZE as u64 - 1)) as usize];
        let len = (entry & 15) as u32;
        if len == 0 {
            return Err(empty_table_error());
        }
        self.acc >>= len;
        self.bits -= len;
        Ok((entry >> 8) as u32)
    }

    /// `n` raw bits, at most 15.
    #[inline(always)]
    fn bits(&mut self, n: u32) -> u32 {
        if self.bits < 16 {
            self.refill();
        }
        let v = (self.acc & ((1 << n) - 1)) as u32;
        self.acc >>= n;
        self.bits -= n;
        v
    }

    /// A class symbol and its extra bits.
    #[inline(always)]
    fn value(&mut self, table: &[u16; TABLE_SIZE]) -> Result<i32, String> {
        let c = self.symbol(table)?;
        if c > 15 {
            return Err(format!("value class {c} out of range"));
        }
        Ok(from_extra_bits(self.bits(c), c))
    }

    /// A vector difference, added to the predictor `pred`.
    fn mv(&mut self, table: &[u16; TABLE_SIZE], pred: Mv) -> Result<Mv, String> {
        let dx = self.value(table)?;
        let dy = self.value(table)?;
        let mv = (pred.0 + dx, pred.1 + dy);
        if mv.0.abs() > MAX_MV || mv.1.abs() > MAX_MV {
            return Err(format!("motion vector {mv:?} out of range"));
        }
        Ok(mv)
    }

    fn consumed_bits(&self) -> usize {
        self.pos * 8 - self.bits as usize
    }
}

/// A symbol with its extra bits, as collected by encoders before the tables are known:
/// table in bits 0-3, symbol in 4-11, extra bit count in 12-15, extra bits in 16-31.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token(u32);

impl Token {
    pub fn new(table: usize, symbol: u32, extra_count: u32, extra: u32) -> Token {
        debug_assert!(
            table < TABLES && symbol < 256 && extra_count <= 15 && extra < 1 << extra_count.max(1)
        );
        Token(table as u32 | symbol << 4 | extra_count << 12 | extra << 16)
    }

    /// A class symbol and the extra bits of `v`.
    pub fn value(table: usize, v: i32) -> Token {
        let c = class(v);
        Token::new(table, c, c, extra_bits(v, c))
    }

    pub fn table(self) -> usize {
        (self.0 & 15) as usize
    }

    pub fn symbol(self) -> usize {
        (self.0 >> 4 & 255) as usize
    }

    fn extra(self) -> (u32, u32) {
        (self.0 >> 12 & 15, self.0 >> 16)
    }
}

/// The tokens of one transform block: DC (`dc`, already predicted for intra blocks) and AC
/// levels in zigzag order (`zigzag[1..]`).
pub fn block_tokens(p: usize, dc: i32, zigzag: &[i32; 64], out: &mut Vec<Token>) {
    let (dc_table, ac_table) = plane_tables(p);
    out.push(Token::value(dc_table, dc));
    let last = (1..64).rev().find(|&k| zigzag[k] != 0);
    let Some(last) = last else {
        out.push(Token::new(ac_table, 0, 0, 0));
        return;
    };
    let mut run = 0;
    for &level in &zigzag[1..=last] {
        if level == 0 {
            run += 1;
            continue;
        }
        while run >= 16 {
            out.push(Token::new(ac_table, 0xF0, 0, 0));
            run -= 16;
        }
        let c = class(level);
        out.push(Token::new(ac_table, run << 4 | c, c, extra_bits(level, c)));
        run = 0;
    }
    if last < 63 {
        out.push(Token::new(ac_table, 0, 0, 0));
    }
}

/// Code lengths of the tables a frame uses.
pub type Lengths = [u8; 256];

/// Writes motion payloads from tokens; keeps the tables of the previous frame so delta
/// frames can inherit them.
#[derive(Default)]
pub struct Encoder {
    previous: [Option<Lengths>; TABLES],
}

impl Encoder {
    pub fn new() -> Encoder {
        Encoder::default()
    }

    /// Appends the payload of a frame to `out`: `slices` holds the tokens of each slice.
    pub fn write(&mut self, keyframe: bool, qp: u8, slices: &[Vec<Token>], out: &mut Vec<u8>) {
        let mut hist = [[0u32; 256]; TABLES];
        for t in slices.iter().flatten() {
            hist[t.table()][t.symbol()] += 1;
        }
        let mut mask = 0u8;
        let mut lengths: [Option<Lengths>; TABLES] = [None; TABLES];
        for t in 0..TABLES {
            let used = hist[t].iter().any(|&n| n > 0);
            if !used {
                // Keyframes: an absent table is empty. Delta frames inherit it.
                lengths[t] = if keyframe { None } else { self.previous[t] };
                continue;
            }
            let new = code_lengths(&hist[t]);
            let cost = |lens: &Lengths| -> Option<u64> {
                hist[t]
                    .iter()
                    .zip(lens)
                    .map(|(&n, &l)| {
                        if n == 0 {
                            Some(0)
                        } else {
                            (l > 0).then_some(n as u64 * l as u64)
                        }
                    })
                    .sum()
            };
            let inherit = !keyframe
                && self.previous[t].as_ref().and_then(cost).is_some_and(|old| {
                    old <= cost(&new).expect("covers") + 8 * LENGTHS_SIZE as u64
                });
            if inherit {
                lengths[t] = self.previous[t];
            } else {
                lengths[t] = Some(new);
                mask |= 1 << t;
            }
        }
        out.extend_from_slice(&[qp, slices.len() as u8, mask, 0]);
        for t in (0..TABLES).filter(|t| mask & 1 << t != 0) {
            let lens = lengths[t].expect("present table");
            out.extend((0..128).map(|i| lens[2 * i] | lens[2 * i + 1] << 4));
        }
        let codes: Vec<Option<[(u16, u8); 256]>> = lengths
            .iter()
            .map(|l| l.as_ref().map(canonical_codes))
            .collect();
        let sizes_at = out.len();
        out.resize(sizes_at + 4 * slices.len(), 0);
        for (s, tokens) in slices.iter().enumerate() {
            let start = out.len();
            let mut w = BitWriter::new(out);
            for t in tokens {
                let (code, len) =
                    codes[t.table()].as_ref().expect("table of a used symbol")[t.symbol()];
                w.put(code as u64, len as u32);
                let (n, bits) = t.extra();
                w.put(bits as u64, n);
            }
            w.flush();
            let size = (out.len() - start) as u32;
            out[sizes_at + 4 * s..sizes_at + 4 * s + 4].copy_from_slice(&size.to_le_bytes());
        }
        self.previous = lengths;
    }
}

struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    bits: u32,
}

impl BitWriter<'_> {
    fn new(out: &mut Vec<u8>) -> BitWriter<'_> {
        BitWriter {
            out,
            acc: 0,
            bits: 0,
        }
    }

    #[inline(always)]
    fn put(&mut self, v: u64, n: u32) {
        self.acc |= v << self.bits;
        self.bits += n;
        while self.bits >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.bits -= 8;
        }
    }

    fn flush(&mut self) {
        if self.bits > 0 {
            self.out.push(self.acc as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{COLOR_FLAG_ALPHA, TextureFormat};

    fn params(w: u32, h: u32, alpha: bool) -> VideoParams {
        VideoParams::new(
            w,
            h,
            TextureFormat::Motion,
            if alpha { COLOR_FLAG_ALPHA } else { 0 },
        )
    }

    #[test]
    fn layout_pads_to_macroblocks() {
        let l = Layout::new(&params(20, 10, true));
        assert_eq!((l.mbs_x, l.mbs_y), (2, 1));
        let sizes: Vec<_> = l
            .planes
            .iter()
            .map(|p| (p.width, p.height, p.first_block))
            .collect();
        assert_eq!(sizes, [(32, 16, 0), (16, 8, 8), (16, 8, 10), (32, 16, 12)]);
        assert_eq!((l.size, l.blocks), (32 * 16 * 2 + 16 * 8 * 2, 20));
        let blocks: Vec<_> = l.mb_blocks(1, 0).collect();
        assert_eq!(
            blocks[..5],
            [(0, 2, 0), (0, 3, 0), (0, 2, 1), (0, 3, 1), (1, 1, 0)]
        );
        assert_eq!(blocks.len(), 10);
    }

    #[test]
    fn transform_inverts_the_forward_transform() {
        // 16x the forward transform of a block decodes back to the block.
        let block: Vec<i32> = (0..64).map(|i| ((i * 37) % 255) - 128).collect();
        // The integer matrix scaled to unit DC norm, as encoders use it.
        let c = |k: usize, n: usize| DCT[k][n] as f64 / (64.0 * 8f64.sqrt());
        let mut coeffs = [0i32; 64];
        for v in 0..8 {
            for u in 0..8 {
                let sum: f64 = (0..64)
                    .map(|i| c(v, i / 8) * c(u, i % 8) * block[i] as f64)
                    .sum();
                coeffs[v * 8 + u] = (16.0 * sum).round() as i32;
            }
        }
        let r = inverse_transform(&coeffs);
        for i in 0..64 {
            assert!(
                (r[i] - block[i]).abs() <= 1,
                "{i}: {} vs {}",
                r[i],
                block[i]
            );
        }
    }

    #[test]
    fn dequantize_steps_double_every_six() {
        assert_eq!(dequantize(1, 4), 16);
        assert_eq!(dequantize(1, 10), 32);
        assert_eq!(dequantize(-3, 0), -30);
        assert_eq!(dequantize(MAX_LEVEL, MAX_QP), 32767);
    }

    #[test]
    fn values_roundtrip_through_extra_bits() {
        for v in -1000..1000 {
            let c = class(v);
            assert_eq!(from_extra_bits(extra_bits(v, c), c), v);
        }
    }

    #[test]
    fn prediction_interpolates() {
        let plane = Plane {
            width: 8,
            height: 8,
            offset: 0,
            first_block: 0,
            shift: 0,
        };
        let pixels: Vec<u8> = (0..64).map(|i| (i % 8 * 10) as u8).collect();
        assert_eq!(predict(&pixels, &plane, 2, 3, 0, 0), 20);
        assert_eq!(
            predict(&pixels, &plane, 2, 3, 2, 0),
            25,
            "half pel of a ramp"
        );
        assert_eq!(predict(&pixels, &plane, 2, 3, -8, 5), 0, "integer part");
        assert_eq!(
            predict(&pixels, &plane, 7, 0, 4, 0),
            70,
            "clamped at the edge"
        );
    }

    #[test]
    fn block_prediction_matches_pixel_prediction() {
        let mut seed = 1u32;
        let mut rand = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed >> 8
        };
        for shift in [0, 1] {
            let plane = Plane {
                width: 48,
                height: 32,
                offset: 0,
                first_block: 0,
                shift,
            };
            let pixels: Vec<u8> = (0..48 * 32).map(|_| rand() as u8).collect();
            let size = 16 >> shift;
            for _ in 0..200 {
                let mv = (rand() as i32 % 161 - 80, rand() as i32 % 161 - 80);
                let (x0, y0) = ((rand() % 3) as i32 * size, (rand() % 2) as i32 * size);
                let mut block = [0u8; 256];
                predict_block(&pixels, &plane, x0, y0, size as usize, mv, &mut block);
                for y in 0..size {
                    for x in 0..size {
                        assert_eq!(
                            block[(y * size + x) as usize],
                            predict(&pixels, &plane, x0 + x, y0 + y, mv.0, mv.1),
                            "{mv:?} at {x0},{y0} + {x},{y}, shift {shift}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn chroma_upsampling_weights() {
        let plane = Plane {
            width: 2,
            height: 2,
            offset: 0,
            first_block: 0,
            shift: 1,
        };
        let c = [0u8, 160, 0, 160];
        let row: Vec<i32> = (0..4).map(|x| chroma_sample(&c, &plane, x, 0)).collect();
        assert_eq!(row, [0, 40, 120, 160]);
    }

    #[test]
    fn vector_prediction_at_8x8() {
        // 2x2 macroblocks; macroblock (0, 0) is split, (1, 0) has one vector.
        let mut field = vec![(0, 0); 16];
        for (sub, mv) in (0..4).zip([(1, 0), (2, 0), (3, 0), (4, 0)]) {
            set_field(&mut field, 2, 0, 0, sub..sub + 1, mv);
        }
        // Quadrant 3: left 3, above 2, above right not decoded, so above left 1.
        assert_eq!(mv_predictor(&field, 2, 0, 0, 0, 3, 1), (2, 0));
        // Macroblock (1, 0): only the left block, quadrant 1 of (0, 0).
        assert_eq!(mv_predictor(&field, 2, 0, 1, 0, 0, 2), (2, 0));
        set_field(&mut field, 2, 1, 0, 0..4, (10, 0));
        // Macroblock (0, 1): above 3, above right 10, no left.
        assert_eq!(mv_predictor(&field, 2, 0, 0, 1, 0, 2), (3, 0));
        // In a slice starting at row 1, nothing above counts.
        assert_eq!(mv_predictor(&field, 2, 1, 0, 1, 0, 2), (0, 0));
    }

    /// Random levels of every size, runs and split vectors survive coding.
    #[test]
    fn parallel_decoding_reports_the_first_failing_slice() {
        let p = params(48, 96, false);
        let layout = Layout::new(&p);
        let mut tokens = vec![Vec::new(); 4];
        for (s, t) in tokens.iter_mut().enumerate() {
            // Slices 1 and 3 stay empty, so they end early.
            if s % 2 == 0 {
                for _ in 0..layout.slice_rows(s as u32, 4).len() as u32 * layout.mbs_x {
                    t.push(Token::new(TABLE_MB, MB_INTRA, 0, 0));
                }
            }
        }
        let mut payload = Vec::new();
        Encoder::new().write(true, 30, &tokens, &mut payload);
        for threads in [1, 2, 4] {
            let mut dec = Decoder::new(&p);
            dec.set_threads(threads);
            let err = dec
                .decode(true, &payload, &mut CodedFrame::default())
                .unwrap_err();
            assert!(err.starts_with("slice 1:"), "{threads} threads: {err}");
        }
        // Decoded ahead, the error comes from `finish_ahead`.
        let mut dec = Decoder::new(&p);
        dec.set_threads(4);
        assert!(dec.decode_ahead(9, true, &payload));
        assert!(dec.decode_ahead(10, true, &payload));
        let err = dec
            .finish_ahead(9, &mut CodedFrame::default())
            .unwrap()
            .unwrap_err();
        assert!(err.starts_with("slice 1:"), "{err}");
        assert_eq!(dec.ahead(), 0, "the frames after an error are cancelled");
    }

    #[test]
    fn decode_ahead_is_cancelled_by_decodes() {
        let p = params(16, 32, false);
        let layout = Layout::new(&p);
        let tokens = vec![vec![
            Token::new(TABLE_MB, MB_INTRA, 0, 0);
            layout.mb_count()
        ]];
        let mut payload = Vec::new();
        Encoder::new().write(true, 30, &tokens, &mut payload);
        let mut dec = Decoder::new(&p);
        dec.set_threads(1);
        assert!(
            !dec.decode_ahead(1, true, &payload),
            "nothing runs ahead on one thread"
        );
        dec.set_threads(2);
        assert!(
            !dec.decode_ahead(1, false, &payload),
            "a delta frame needs a keyframe first"
        );
        assert!(
            !dec.decode_ahead(1, true, &payload[..2]),
            "invalid headers are left to decode"
        );
        assert!(dec.decode_ahead(1, true, &payload));
        assert!(
            dec.finish_ahead(2, &mut CodedFrame::default()).is_none(),
            "another tag"
        );
        let mut frame = CodedFrame::default();
        dec.decode(true, &payload, &mut frame).unwrap();
        assert!(
            dec.finish_ahead(1, &mut CodedFrame::default()).is_none(),
            "cancelled by the decode"
        );
        assert!(dec.decode_ahead(3, true, &payload));
        dec.set_threads(3);
        assert!(
            dec.finish_ahead(3, &mut CodedFrame::default()).is_none(),
            "cancelled by set_threads"
        );
        assert!(dec.decode_ahead(4, true, &payload));
        let mut ahead = CodedFrame::default();
        dec.finish_ahead(4, &mut ahead).unwrap().unwrap();
        assert_eq!(ahead, frame);
        // Frames queue in order, up to MAX_AHEAD, and are collected in order.
        for tag in 0..MAX_AHEAD as u64 {
            assert!(dec.decode_ahead(tag, tag == 0, &payload), "frame {tag}");
        }
        assert!(!dec.decode_ahead(9, false, &payload), "the queue is full");
        assert_eq!(dec.ahead(), MAX_AHEAD);
        assert!(
            dec.finish_ahead(1, &mut CodedFrame::default()).is_none(),
            "not the first frame"
        );
        dec.finish_ahead(0, &mut ahead).unwrap().unwrap();
        assert_eq!(ahead, frame);
        dec.decode(true, &payload, &mut CodedFrame::default())
            .unwrap();
        assert_eq!(dec.ahead(), 0, "cancelled by the decode");
    }

    #[test]
    fn random_payloads_roundtrip() {
        let mut seed = 7u32;
        let mut rand = move |n: u32| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) % n
        };
        let p = params(48, 96, true);
        let layout = Layout::new(&p);
        let mut enc = Encoder::new();
        // Any thread count decodes the same frame.
        let mut decoders: Vec<_> = [1, 2, 3, 8]
            .into_iter()
            .map(|t| {
                let mut d = Decoder::new(&p);
                d.set_threads(t);
                d
            })
            .collect();
        let mut frames = Vec::new();
        for frame_no in 0..6 {
            let keyframe = frame_no % 4 == 0;
            let slices = [3, 6, 4, 5, 4, 6][frame_no];
            let mut tokens = vec![Vec::new(); slices];
            let mut expected = CodedFrame::default();
            let mut field = vec![(0, 0); 4 * layout.mb_count()];
            for s in 0..slices as u32 {
                let mut dc_pred = [0i32; 4];
                let rows = layout.slice_rows(s, slices as u32);
                for my in rows.clone() {
                    for mx in 0..layout.mbs_x {
                        let t = &mut tokens[s as usize];
                        let kind = if keyframe { MB_INTRA } else { rand(4) };
                        let (cbp, chroma, alpha) = if kind == MB_SKIP {
                            (0, false, false)
                        } else {
                            (rand(16), rand(2) == 1, rand(2) == 1)
                        };
                        let sym = kind | cbp << 2 | (chroma as u32) << 6 | (alpha as u32) << 7;
                        t.push(Token::new(TABLE_MB, sym, 0, 0));
                        let mut mvs = [(0, 0); 4];
                        let mut vector = |t: &mut Vec<Token>, pred: Mv| {
                            let mv = (rand(200) as i32 - 100, rand(200) as i32 - 100);
                            t.push(Token::value(TABLE_MV, mv.0 - pred.0));
                            t.push(Token::value(TABLE_MV, mv.1 - pred.1));
                            mv
                        };
                        match kind {
                            MB_SPLIT => {
                                for sub in 0..4 {
                                    let pred = mv_predictor(
                                        &field,
                                        layout.mbs_x,
                                        rows.start,
                                        mx,
                                        my,
                                        sub,
                                        1,
                                    );
                                    mvs[sub as usize] = vector(t, pred);
                                    set_field(
                                        &mut field,
                                        layout.mbs_x,
                                        mx,
                                        my,
                                        sub..sub + 1,
                                        mvs[sub as usize],
                                    );
                                }
                            }
                            MB_INTRA => set_field(&mut field, layout.mbs_x, mx, my, 0..4, (0, 0)),
                            _ => {
                                let pred =
                                    mv_predictor(&field, layout.mbs_x, rows.start, mx, my, 0, 2);
                                let mv = if kind == MB_INTER {
                                    vector(t, pred)
                                } else {
                                    pred
                                };
                                mvs = [mv; 4];
                                set_field(&mut field, layout.mbs_x, mx, my, 0..4, mv);
                            }
                        }
                        expected.push_mb(kind, &mvs);
                        if kind == MB_SKIP {
                            continue;
                        }
                        let mut luma = 0;
                        for (pl, bx, by) in layout.mb_blocks(mx, my) {
                            let coded = match pl {
                                0 => {
                                    luma += 1;
                                    cbp >> (luma - 1) & 1 != 0
                                }
                                3 => alpha,
                                _ => chroma,
                            };
                            if !coded {
                                continue;
                            }
                            let mut zigzag = [0i32; 64];
                            for z in zigzag.iter_mut() {
                                if rand(4) == 0 {
                                    let size = rand(15);
                                    *z = (rand(1 << size) as i32 + 1)
                                        * if rand(2) == 0 { 1 } else { -1 };
                                }
                            }
                            if rand(3) == 0 {
                                zigzag[1..63].fill(0);
                            }
                            let dc = zigzag[0];
                            let mut sent_dc = dc;
                            if kind == MB_INTRA {
                                sent_dc -= dc_pred[pl];
                                dc_pred[pl] = dc;
                            }
                            block_tokens(pl, sent_dc, &zigzag, t);
                            let mut natural = [0i32; 64];
                            for (k, &l) in zigzag.iter().enumerate() {
                                natural[ZIGZAG[k] as usize] = l;
                            }
                            let plane = &layout.planes[pl];
                            expected.push_block(
                                plane.first_block + by * plane.blocks_x() + bx,
                                &natural,
                            );
                        }
                    }
                }
            }
            let mut payload = Vec::new();
            enc.write(keyframe, 30, &tokens, &mut payload);
            expected.keyframe = keyframe;
            frames.push((payload, expected));
        }
        // Every frame decoded ahead, delta frames inheriting tables from the frame before:
        // one at a time, and in batches of three queued before the first is collected.
        let mut ahead = Decoder::new(&p);
        ahead.set_threads(3);
        let mut batched = Decoder::new(&p);
        batched.set_threads(4);
        for (frame_no, (payload, expected)) in frames.iter().enumerate() {
            let keyframe = expected.keyframe;
            let mut frame = CodedFrame::default();
            decoders[0].decode(keyframe, payload, &mut frame).unwrap();
            for dec in &mut decoders[1..] {
                let mut other = CodedFrame::default();
                dec.decode(keyframe, payload, &mut other).unwrap();
                assert_eq!(other, frame, "frame {frame_no}");
            }
            assert!(ahead.decode_ahead(frame_no as u64, keyframe, payload));
            let mut other = CodedFrame::default();
            ahead
                .finish_ahead(frame_no as u64, &mut other)
                .unwrap()
                .unwrap();
            assert_eq!(other, frame, "frame {frame_no} decoded ahead");
            if frame_no % 3 == 0 {
                for (n, (payload, expected)) in frames.iter().enumerate().skip(frame_no).take(3) {
                    assert!(
                        batched.decode_ahead(n as u64, expected.keyframe, payload),
                        "frame {n} queued"
                    );
                }
            }
            batched
                .finish_ahead(frame_no as u64, &mut other)
                .unwrap()
                .unwrap();
            assert_eq!(other, frame, "frame {frame_no} decoded ahead in a batch");
            assert_eq!(frame.scales[..3], [dequant_scale(30); 3]);
            assert_eq!(frame.mbs, expected.mbs, "frame {frame_no}");
            assert_eq!(
                frame.block_count(),
                expected.block_count(),
                "frame {frame_no}"
            );
            for i in 0..frame.block_count() {
                assert_eq!(
                    frame.block(i),
                    expected.block(i),
                    "frame {frame_no} block {i}"
                );
            }
        }
    }

    /// A frame of tokens for one 16x16 keyframe and one delta frame, decoded back.
    #[test]
    fn payloads_roundtrip() {
        let p = params(16, 32, false);
        let layout = Layout::new(&p);
        let mut levels = [0i32; 64];
        levels[1] = -3;
        levels[40] = 1;
        let mut key = Vec::new();
        let zero = [0i32; 64];
        for mb in 0..layout.mb_count() {
            key.push(Token::new(TABLE_MB, MB_INTRA | 0b0001 << 2 | 1 << 6, 0, 0));
            // DC 12, predicted from 0, then from 12.
            block_tokens(0, if mb == 0 { 12 } else { 0 }, &levels, &mut key);
            block_tokens(1, 0, &zero, &mut key);
            block_tokens(2, 0, &zero, &mut key);
        }
        let mut enc = Encoder::new();
        let mut payload = Vec::new();
        enc.write(true, 20, &[key], &mut payload);
        let mut dec = Decoder::new(&p);
        let mut frame = CodedFrame::default();
        dec.decode(true, &payload, &mut frame).unwrap();
        assert_eq!(frame.block_count(), 2);
        let (index, coeffs) = frame.block(1);
        assert_eq!(index, 4, "block 0 of the second macroblock");
        // Zigzag position 40 is row 3, column 5.
        assert_eq!((coeffs[0], coeffs[1], coeffs[29]), (12, -3, 1));

        // Delta frame: an inter macroblock with a vector, then a skip.
        let tokens = vec![
            Token::new(TABLE_MB, MB_INTER, 0, 0),
            Token::value(TABLE_MV, -5),
            Token::value(TABLE_MV, 6),
            Token::new(TABLE_MB, MB_SKIP, 0, 0),
        ];
        payload.clear();
        enc.write(false, 20, &[tokens], &mut payload);
        assert_eq!(
            payload[2] & (1 << TABLE_DC_Y),
            0,
            "unused tables are inherited"
        );
        dec.decode(false, &payload, &mut frame).unwrap();
        assert_eq!(frame.mb(0), (MB_INTER, [(-5, 6); 4]));
        assert_eq!(
            frame.mb(1),
            (MB_SKIP, [(-5, 6); 4]),
            "skip takes the predicted vector, here the upper one"
        );
        assert_eq!(frame.block_count(), 0);

        let mut out = vec![0u8; layout.size];
        let reference = vec![77u8; layout.size];
        reconstruct(&layout, &frame, &reference, &mut out);
        assert!(out.iter().all(|&v| v == 77));
        assert!(
            dec.decode(false, &payload[..payload.len() - 1], &mut frame)
                .is_err()
        );
    }
}
