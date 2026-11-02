//! The motion codec (`docs/features/motion-codec.md`): payload syntax and entropy coding
//! (`docs/spec/PRAXI.md#motion-payload`), the coded frames handed to GPU decoders, and the
//! reference reconstruction that encoders and tests use.

use crate::huffman::{TABLE_SIZE, canonical_codes, code_lengths, single_table};
use crate::{Chroma, MotionParams, VideoParams};

/// Largest `qp`.
pub const MAX_QP: u8 = 51;
/// Largest magnitude of a quantized coefficient.
pub const MAX_LEVEL: i32 = 16383;
/// Largest magnitude of a motion vector component, in quarter pels.
pub const MAX_MV: i32 = 8191;
/// Words per block of [`CodedFrame::blocks`]: the block index and 64 coefficients.
pub const BLOCK_WORDS: usize = 33;

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
    /// Two words per macroblock, row-major: the kind (`MB_*`), then the motion vector in
    /// quarter pels, x in the low and y in the high 16 bits (two's complement).
    pub mbs: Vec<u32>,
    /// [`BLOCK_WORDS`] words per transform block with coefficients: its index, then its 64
    /// dequantized coefficients in row-major order, two per word (even index in the low 16
    /// bits, two's complement).
    pub blocks: Vec<u32>,
}

impl CodedFrame {
    pub fn mb(&self, i: usize) -> (u32, i32, i32) {
        let mv = self.mbs[2 * i + 1];
        (self.mbs[2 * i], mv as i16 as i32, (mv >> 16) as i16 as i32)
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len() / BLOCK_WORDS
    }

    /// Index and coefficients of coded block `i`.
    pub fn block(&self, i: usize) -> (u32, [i32; 64]) {
        let w = &self.blocks[i * BLOCK_WORDS..(i + 1) * BLOCK_WORDS];
        let mut c = [0; 64];
        for (k, &word) in w[1..].iter().enumerate() {
            c[2 * k] = word as i16 as i32;
            c[2 * k + 1] = (word >> 16) as i16 as i32;
        }
        (w[0], c)
    }

    /// Appends a coded block: its index and row-major coefficients.
    pub fn push_block(&mut self, index: u32, coeffs: &[i32; 64]) {
        self.blocks.push(index);
        self.blocks.extend(
            coeffs
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| (c[0] as u16 as u32) | (c[1] as u16 as u32) << 16),
        );
    }
}

/// The `qp` of plane `p` for a frame `qp`.
pub fn plane_qp(qp: u8, plane: usize, params: &MotionParams) -> u8 {
    let offset = match plane {
        0 => 0,
        3 => params.alpha_qp_offset,
        _ => params.chroma_qp_offset,
    };
    (qp as i32 + offset as i32).clamp(0, MAX_QP as i32) as u8
}

/// A dequantized coefficient.
pub fn dequantize(level: i32, qp: u8) -> i32 {
    let v = (level.abs() * DEQUANT[qp as usize % 6]) << (qp / 6) >> 2;
    (v * level.signum()).clamp(-32768, 32767)
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

/// Motion-compensated prediction of pixel (`x`, `y`) of `plane` (the plane's pixels in
/// `reference`), bilinear, coordinates clamped to the plane.
#[inline]
pub fn predict(reference: &[u8], plane: &Plane, x: i32, y: i32, mvx: i32, mvy: i32) -> u8 {
    let fb = mv_fraction_bits(plane);
    let s = 1 << fb;
    let (ix, iy, fx, fy) = (
        x + (mvx >> fb),
        y + (mvy >> fb),
        mvx & (s - 1),
        mvy & (s - 1),
    );
    let (w, h) = (plane.width as i32, plane.height as i32);
    let at =
        |x: i32, y: i32| reference[(y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize] as i32;
    let v = (s - fx) * (s - fy) * at(ix, iy)
        + fx * (s - fy) * at(ix + 1, iy)
        + (s - fx) * fy * at(ix, iy + 1)
        + fx * fy * at(ix + 1, iy + 1);
    ((v + (1 << (2 * fb - 1))) >> (2 * fb)) as u8
}

/// Rebuilds a frame into `out` (a frame buffer of `layout.size` bytes) from the previous
/// frame `reference` (ignored for keyframes, which hold only intra macroblocks).
pub fn reconstruct(layout: &Layout, frame: &CodedFrame, reference: &[u8], out: &mut [u8]) {
    for my in 0..layout.mbs_y {
        for mx in 0..layout.mbs_x {
            let (kind, mvx, mvy) = frame.mb((my * layout.mbs_x + mx) as usize);
            for plane in &layout.planes {
                let size = 16 >> plane.shift;
                let (x0, y0) = ((mx * size) as i32, (my * size) as i32);
                let src = &reference[plane.offset..];
                for y in y0..y0 + size as i32 {
                    let row = plane.offset + (y as u32 * plane.width) as usize;
                    for x in x0..x0 + size as i32 {
                        out[row + x as usize] = if kind == MB_INTRA {
                            128
                        } else {
                            predict(src, plane, x, y, mvx, mvy)
                        };
                    }
                }
            }
        }
    }
    for i in 0..frame.block_count() {
        let (index, coeffs) = frame.block(i);
        add_residual(layout, index, &inverse_transform(&coeffs), out);
    }
}

/// Adds the residual of transform block `index` to the frame buffer, clamping to 0..255.
pub fn add_residual(layout: &Layout, index: u32, residual: &[i32; 64], out: &mut [u8]) {
    let plane = layout
        .planes
        .iter()
        .rev()
        .find(|p| p.first_block <= index)
        .expect("block index");
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

/// Motion vector predictor for macroblock (`mx`, `my`) of a slice starting at row
/// `first_row`, from the vectors of earlier macroblocks (intra ones are zero): the left,
/// upper and upper-right (upper-left at the right edge) neighbours inside the slice. One
/// available neighbour gives its vector, none gives zero, otherwise the median with
/// missing neighbours as zero.
pub fn mv_predictor(
    mvs: &[(i32, i32)],
    mbs_x: u32,
    mx: u32,
    my: u32,
    first_row: u32,
) -> (i32, i32) {
    let at = |x: u32, y: u32| mvs[(y * mbs_x + x) as usize];
    let up = my > first_row;
    let left = (mx > 0).then(|| at(mx - 1, my));
    let above = up.then(|| at(mx, my - 1));
    let diag = if !up {
        None
    } else if mx + 1 < mbs_x {
        Some(at(mx + 1, my - 1))
    } else if mx > 0 {
        Some(at(mx - 1, my - 1))
    } else {
        None
    };
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

/// Turns motion payloads into coded frames. Keeps the Huffman tables, which delta frames may
/// inherit from the previous frame.
pub struct Decoder {
    layout: Layout,
    params: MotionParams,
    tables: [Option<Box<[u16; TABLE_SIZE]>>; TABLES],
    mvs: Vec<(i32, i32)>,
    /// A keyframe was decoded since the start, so delta frames have tables and a reference.
    primed: bool,
}

impl Decoder {
    pub fn new(params: &VideoParams) -> Decoder {
        let layout = Layout::new(params);
        let mvs = vec![(0, 0); layout.mb_count()];
        Decoder {
            layout,
            params: params.motion,
            tables: Default::default(),
            mvs,
            primed: false,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Decodes one payload into `out`.
    pub fn decode(
        &mut self,
        keyframe: bool,
        payload: &[u8],
        out: &mut CodedFrame,
    ) -> Result<(), String> {
        if !keyframe && !self.primed {
            return Err("delta frame without a preceding keyframe".into());
        }
        if payload.len() < HEADER_SIZE {
            return Err("motion payload shorter than its header".into());
        }
        let (qp, slices, mask) = (payload[0], payload[1] as u32, payload[2]);
        if qp > MAX_QP {
            return Err(format!("qp {qp} out of range"));
        }
        if slices == 0 || slices > self.layout.mbs_y {
            return Err(format!(
                "{slices} slices for {} macroblock rows",
                self.layout.mbs_y
            ));
        }
        if !self.layout.alpha() && mask & (1 << TABLE_DC_A | 1 << TABLE_AC_A) != 0 {
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
        out.keyframe = keyframe;
        out.mbs.clear();
        out.mbs.resize(2 * self.layout.mb_count(), 0);
        out.blocks.clear();
        for s in 0..slices {
            let size = u32::from_le_bytes(
                sizes[4 * s as usize..4 * s as usize + 4]
                    .try_into()
                    .expect("4 bytes"),
            ) as usize;
            let Some(data) = payload.get(at..at + size) else {
                return Err(format!("slice {s} runs past the payload"));
            };
            at += size;
            self.decode_slice(keyframe, qp, s, slices, data, out)
                .map_err(|e| format!("slice {s}: {e}"))?;
        }
        if at != payload.len() {
            return Err("motion payload has bytes after its last slice".into());
        }
        self.primed = true;
        Ok(())
    }

    fn decode_slice(
        &mut self,
        keyframe: bool,
        qp: u8,
        s: u32,
        slices: u32,
        data: &[u8],
        out: &mut CodedFrame,
    ) -> Result<(), String> {
        let layout = &self.layout;
        let tables = &self.tables;
        let mut r = BitReader::new(data);
        let mut dc_pred = [0i32; 4];
        let qps: Vec<u8> = (0..layout.planes.len())
            .map(|p| plane_qp(qp, p, &self.params))
            .collect();
        let rows = layout.slice_rows(s, slices);
        let first_row = rows.start;
        let mut coeffs = [0i32; 64];
        for my in rows {
            for mx in 0..layout.mbs_x {
                let i = (my * layout.mbs_x + mx) as usize;
                let sym = r.symbol(tables, TABLE_MB)?;
                let kind = sym & 3;
                let (cbp, chroma, alpha) = (sym >> 2 & 15, sym >> 6 & 1 != 0, sym >> 7 != 0);
                if kind > MB_INTRA || keyframe && kind != MB_INTRA || kind == MB_SKIP && sym != 0 {
                    return Err(format!("invalid macroblock symbol {sym:#04x}"));
                }
                if alpha && !layout.alpha() {
                    return Err("alpha blocks without an alpha plane".into());
                }
                let mut mv = (0, 0);
                if kind != MB_INTRA {
                    let pred = mv_predictor(&self.mvs, layout.mbs_x, mx, my, first_row);
                    mv = pred;
                    if kind == MB_INTER {
                        let dx = r.value(tables, TABLE_MV)?;
                        let dy = r.value(tables, TABLE_MV)?;
                        mv = (pred.0 + dx, pred.1 + dy);
                        if mv.0.abs() > MAX_MV || mv.1.abs() > MAX_MV {
                            return Err(format!("motion vector {mv:?} out of range"));
                        }
                    }
                }
                self.mvs[i] = mv;
                out.mbs[2 * i] = kind;
                out.mbs[2 * i + 1] = (mv.0 as u16 as u32) | (mv.1 as u16 as u32) << 16;
                if kind == MB_SKIP {
                    continue;
                }
                let mut luma = 0;
                for (p, bx, by) in layout.mb_blocks(mx, my) {
                    let coded = match p {
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
                    let (dc_table, ac_table) = plane_tables(p);
                    let mut dc = r.value(tables, dc_table)?;
                    if kind == MB_INTRA {
                        dc += dc_pred[p];
                        dc_pred[p] = dc;
                    }
                    coeffs.fill(0);
                    let mut nonzero = dc != 0;
                    coeffs[0] = check_level(dc)?;
                    let mut k = 1;
                    while k < 64 {
                        let sym = r.symbol(tables, ac_table)?;
                        let (run, size) = (sym >> 4, sym & 15);
                        if size == 0 {
                            match run {
                                0 => break,
                                15 if k + 16 < 64 => {
                                    k += 16;
                                    continue;
                                }
                                _ => return Err(format!("invalid coefficient symbol {sym:#04x}")),
                            }
                        }
                        k += run as usize;
                        if k >= 64 {
                            return Err("coefficients run past the block".into());
                        }
                        coeffs[ZIGZAG[k] as usize] =
                            check_level(from_extra_bits(r.bits(size)?, size))?;
                        nonzero = true;
                        k += 1;
                    }
                    if nonzero {
                        let plane = &layout.planes[p];
                        for c in coeffs.iter_mut() {
                            *c = dequantize(*c, qps[p]);
                        }
                        out.push_block(plane.first_block + by * plane.blocks_x() + bx, &coeffs);
                    }
                }
            }
        }
        if r.consumed_bits() > data.len() * 8 {
            return Err("slice data ends early".into());
        }
        Ok(())
    }
}

fn check_level(level: i32) -> Result<i32, String> {
    if level.abs() > MAX_LEVEL {
        return Err(format!("coefficient level {level} out of range"));
    }
    Ok(level)
}

/// Least-significant-bit-first bit reader; bytes past the end read as zero.
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
            while self.bits <= 56 {
                let byte = self.src.get(self.pos).copied().unwrap_or(0);
                self.acc |= (byte as u64) << self.bits;
                self.pos += 1;
                self.bits += 8;
            }
        }
    }

    #[inline(always)]
    fn symbol(
        &mut self,
        tables: &[Option<Box<[u16; TABLE_SIZE]>>; TABLES],
        t: usize,
    ) -> Result<u32, String> {
        let Some(table) = &tables[t] else {
            return Err(format!("symbol from empty table {t}"));
        };
        if self.bits < 16 {
            self.refill();
        }
        let entry = table[(self.acc & (TABLE_SIZE as u64 - 1)) as usize];
        let len = (entry & 15) as u32;
        self.acc >>= len;
        self.bits -= len;
        Ok((entry >> 8) as u32)
    }

    /// `n` raw bits, at most 15.
    #[inline(always)]
    fn bits(&mut self, n: u32) -> Result<u32, String> {
        if self.bits < 16 {
            self.refill();
        }
        let v = (self.acc & ((1 << n) - 1)) as u32;
        self.acc >>= n;
        self.bits -= n;
        Ok(v)
    }

    /// A class symbol from table `t` and its extra bits.
    #[inline(always)]
    fn value(
        &mut self,
        tables: &[Option<Box<[u16; TABLE_SIZE]>>; TABLES],
        t: usize,
    ) -> Result<i32, String> {
        let c = self.symbol(tables, t)?;
        if c > 15 {
            return Err(format!("value class {c} out of range"));
        }
        Ok(from_extra_bits(self.bits(c)?, c))
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
            let mut w = BitWriter {
                out,
                acc: 0,
                bits: 0,
            };
            for t in tokens {
                let (code, len) =
                    codes[t.table()].as_ref().expect("table of a used symbol")[t.symbol()];
                w.put(code as u64, len as u32);
                let (n, extra) = t.extra();
                w.put(extra as u64, n);
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
        assert_eq!(predict(&pixels, &plane, 2, 3, 2, 0), 25, "half pel");
        assert_eq!(predict(&pixels, &plane, 2, 3, -8, 5), 0, "integer part");
        assert_eq!(
            predict(&pixels, &plane, 7, 0, 4, 0),
            70,
            "clamped at the edge"
        );
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
        assert_eq!(
            (coeffs[0], coeffs[1], coeffs[29]),
            (dequantize(12, 20), dequantize(-3, 20), dequantize(1, 20))
        );

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
        assert_eq!(frame.mb(0), (MB_INTER, -5, 6));
        assert_eq!(
            frame.mb(1),
            (MB_SKIP, -5, 6),
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
