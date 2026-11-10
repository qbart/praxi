//! Encoder of the motion codec (`docs/features/motion-codec.md#encoder`).

use praxi_format::motion::{
    self, Layout, MAX_LEVEL, MAX_MV, MB_INTER, MB_INTRA, MB_SKIP, MB_SPLIT, Mv, Plane,
    TABLE_MB, TABLE_MV, Token, ZIGZAG, block_tokens, class, dequantize,
    inverse_transform, mv_predictor, plane_qp, predict_block,
    predict_transform_block, set_field,
};
use praxi_format::{MotionParams, VideoParams};

/// Conjugate gradient iterations of chroma downsampling; more change nothing visible.
const DOWNSAMPLE_ITERATIONS: usize = 5;
/// Share of intra macroblocks above which a delta frame is coded as a keyframe instead.
const SCENE_CUT: f64 = 0.6;
/// Rounding offsets of quantization, in steps.
const ROUND_INTRA: f32 = 1.0 / 3.0;
const ROUND_INTER: f32 = 1.0 / 6.0;
/// Integer search radius around the best candidate, in pixels.
const SEARCH_STEPS: [i32; 5] = [16, 8, 4, 2, 1];
/// Integer search radius of 8x8 quadrants around the macroblock's vector.
const SUB_STEPS: [i32; 3] = [4, 2, 1];

/// Search results of a macroblock.
#[derive(Debug, Clone, Copy, Default)]
struct Searched {
    mv: Mv,
    quadrants: [Mv; 4],
}

/// Macroblock counts of the frames encoded so far.
#[derive(Debug, Clone, Copy, Default)]
pub struct MotionStats {
    pub intra: u64,
    pub inter: u64,
    pub skip: u64,
    /// Inter macroblocks with a vector per 8x8 quadrant.
    pub split: u64,
    /// Delta frames turned into keyframes at scene cuts.
    pub scene_cuts: u64,
}

pub(crate) struct MotionEncoder {
    layout: Layout,
    params: MotionParams,
    qp: u8,
    slices: u32,
    threads: usize,
    src: Vec<u8>,
    reference: Vec<u8>,
    /// Vectors of the previous frame per macroblock, search candidates.
    previous_mvs: Vec<Mv>,
    writer: motion::Encoder,
    pub stats: MotionStats,
}

/// The coding of one macroblock.
struct MbCode {
    kind: u32,
    /// Vectors of the four quadrants.
    mvs: [Mv; 4],
    /// Zigzag-order levels per transform block in syntax order, and whether the block is
    /// coded.
    blocks: Vec<([i32; 64], bool)>,
}

/// Costs in RGB squared error units: luma errors spread to all three channels, chroma
/// errors to two (Co) or three (Cg), times the pixels a chroma sample covers.
fn plane_weights(layout: &Layout) -> [f64; 4] {
    let k = (1 << (2 * layout.planes[1].shift)) as f64;
    [3.0, 2.0 * k, 3.0 * k, 1.0]
}

impl MotionEncoder {
    pub fn new(params: &VideoParams, qp: u8, slices: u32, threads: usize) -> MotionEncoder {
        let layout = Layout::new(params);
        let slices = slices.clamp(1, layout.mbs_y.min(255));
        let previous_mvs = vec![(0, 0); layout.mb_count()];
        MotionEncoder {
            src: vec![0; layout.size],
            reference: vec![0; layout.size],
            layout,
            params: params.motion,
            qp: qp.min(motion::MAX_QP),
            slices,
            threads: threads.max(1),
            previous_mvs,
            writer: motion::Encoder::new(),
            stats: MotionStats::default(),
        }
    }

    /// Encodes a frame of `w * h` RGBA pixels into `out`; returns whether it is a keyframe
    /// (asked for, or chosen at a scene cut).
    pub fn encode(
        &mut self,
        rgba: &[u8],
        w: usize,
        h: usize,
        keyframe: bool,
        out: &mut Vec<u8>,
    ) -> bool {
        load_planes(&self.layout, rgba, w, h, &mut self.src);
        let mut keyframe = keyframe;
        let searched = if keyframe { Vec::new() } else { self.search() };
        let mut coded = self.decide(keyframe, &searched);
        if !keyframe {
            let intra = coded
                .iter()
                .flat_map(|s| &s.0)
                .filter(|m| m.kind == MB_INTRA)
                .count();
            if intra as f64 > SCENE_CUT * self.layout.mb_count() as f64 {
                keyframe = true;
                self.stats.scene_cuts += 1;
                coded = self.decide(true, &[]);
            }
        }
        let mut recon = vec![0u8; self.layout.size];
        let mut tokens = Vec::with_capacity(coded.len());
        let mut mvs = vec![(0, 0); self.layout.mb_count()];
        let mut frame = motion::CodedFrame::default();
        for (s, (mbs, slice_tokens)) in coded.into_iter().enumerate() {
            let rows = self.layout.slice_rows(s as u32, self.slices);
            for (i, mb) in mbs.iter().enumerate() {
                let index = (rows.start * self.layout.mbs_x) as usize + i;
                match mb.kind {
                    MB_INTRA => self.stats.intra += 1,
                    MB_INTER => self.stats.inter += 1,
                    MB_SPLIT => self.stats.split += 1,
                    _ => self.stats.skip += 1,
                }
                mvs[index] = mb.mvs[0];
                frame.push_mb(mb.kind, &mb.mvs);
                let (mx, my) = (
                    index as u32 % self.layout.mbs_x,
                    index as u32 / self.layout.mbs_x,
                );
                for ((p, bx, by), (levels, coded)) in self.layout.mb_blocks(mx, my).zip(&mb.blocks)
                {
                    if !coded || levels.iter().all(|&l| l == 0) {
                        continue;
                    }
                    let plane = &self.layout.planes[p];
                    let mut natural = [0i32; 64];
                    for (k, &l) in levels.iter().enumerate() {
                        natural[ZIGZAG[k] as usize] = l;
                    }
                    frame.push_block(plane.first_block + by * plane.blocks_x() + bx, &natural);
                }
            }
            tokens.push(slice_tokens);
        }
        frame.keyframe = keyframe;
        for (p, scale) in frame.scales.iter_mut().enumerate().take(self.layout.planes.len()) {
            *scale = motion::dequant_scale(plane_qp(self.qp, p, &self.params));
        }
        // Rebuild exactly as decoders will, and predict the next frame from that.
        motion::reconstruct(&self.layout, &frame, &self.reference, &mut recon);
        self.reference = recon;
        self.previous_mvs = mvs;
        self.writer.write(keyframe, self.qp, &tokens, out);
        keyframe
    }

    /// Integer and sub-pel motion search of every macroblock against the reference luma.
    fn search(&self) -> Vec<Searched> {
        let layout = &self.layout;
        let lambda = lambda(self.qp).sqrt();
        let mut result = vec![Searched::default(); layout.mb_count()];
        let rows_per_thread = (layout.mbs_y as usize).div_ceil(self.threads).max(1);
        std::thread::scope(|s| {
            for (t, part) in result
                .chunks_mut(rows_per_thread * layout.mbs_x as usize)
                .enumerate()
            {
                let (src, reference, previous) = (&self.src, &self.reference, &self.previous_mvs);
                s.spawn(move || {
                    let first = t * rows_per_thread * layout.mbs_x as usize;
                    for i in 0..part.len() {
                        let index = first + i;
                        let (mx, my) = (index as u32 % layout.mbs_x, index as u32 / layout.mbs_x);
                        // The left neighbour, searched just before.
                        let pred = if mx > 0 { part[i - 1].mv } else { (0, 0) };
                        part[i] = search_mb(layout, src, reference, previous, mx, my, pred, lambda);
                    }
                });
            }
        });
        result
    }

    /// Mode decision and coding of every macroblock, slices in parallel. Returns per slice
    /// the macroblocks and their tokens.
    fn decide(&self, keyframe: bool, searched: &[Searched]) -> Vec<(Vec<MbCode>, Vec<Token>)> {
        let layout = &self.layout;
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..self.slices)
                .map(|slice| {
                    s.spawn(move || {
                        let rows = layout.slice_rows(slice, self.slices);
                        let mut ctx = SliceCoder::new(self, keyframe, rows.start);
                        let mut mbs = Vec::new();
                        for my in rows {
                            for mx in 0..layout.mbs_x {
                                let found =
                                    searched.get((my * layout.mbs_x + mx) as usize).copied();
                                mbs.push(ctx.code_mb(mx, my, found));
                            }
                        }
                        (mbs, ctx.tokens)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("slice thread"))
                .collect()
        })
    }
}

/// Lagrange multiplier of mode decisions, per RGB squared error unit.
fn lambda(qp: u8) -> f64 {
    3.0 * 0.85 * 2f64.powf((qp as f64 - 12.0) / 3.0)
}

/// Approximate bits of a vector difference.
fn mv_bits(d: (i32, i32)) -> f64 {
    (2 + 2 * class(d.0) + 2 * class(d.1)) as f64
}

/// The source planes, padded by repeating the last column and row; chroma averaged over 2x2
/// pixels in 4:2:0.
fn load_planes(layout: &Layout, rgba: &[u8], w: usize, h: usize, out: &mut [u8]) {
    let [py, pco, pcg] = [layout.planes[0], layout.planes[1], layout.planes[2]];
    let (pw, ph) = (py.width as usize, py.height as usize);
    let mut co = vec![0f32; pw * ph];
    let mut cg = vec![0f32; pw * ph];
    for y in 0..ph {
        let sy = y.min(h - 1);
        for x in 0..pw {
            let sx = x.min(w - 1);
            let px = &rgba[(sy * w + sx) * 4..(sy * w + sx) * 4 + 4];
            let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
            out[py.offset + y * pw + x] = ((r + 2 * g + b + 2) >> 2) as u8;
            co[y * pw + x] = (r - b) as f32 / 2.0 + 128.0;
            cg[y * pw + x] = (2 * g - r - b) as f32 / 4.0 + 128.0;
            if let Some(pa) = layout.planes.get(3) {
                out[pa.offset + y * pw + x] = px[3];
            }
        }
    }
    for (plane, full) in [(pco, &co), (pcg, &cg)] {
        let pixels = &mut out[plane.offset..][..(plane.width * plane.height) as usize];
        if plane.shift == 0 {
            for (o, &v) in pixels.iter_mut().zip(full.iter()) {
                *o = v.round().clamp(0.0, 255.0) as u8;
            }
        } else {
            downsample(full, pw, ph, pixels);
        }
    }
}

/// Half-resolution chroma whose bilinear upsampling (as decoders do it) is closest to the
/// full-resolution chroma `full` in squared error: conjugate gradient on the normal
/// equations, from the 2x2 average.
fn downsample(full: &[f32], pw: usize, ph: usize, out: &mut [u8]) {
    let (cw, ch) = (pw / 2, ph / 2);
    // Per luma coordinate: the two chroma samples it blends and their weights (of 4).
    let taps = |v: usize, n: usize| -> [(usize, f32); 2] {
        let i = (v >> 1) as isize;
        let (a, b) = if v & 1 == 0 { ((i - 1, 1.0), (i, 3.0)) } else { ((i, 3.0), (i + 1, 1.0)) };
        let c = |j: isize| j.clamp(0, n as isize - 1) as usize;
        [(c(a.0), a.1 / 4.0), (c(b.0), b.1 / 4.0)]
    };
    let xt: Vec<_> = (0..pw).map(|x| taps(x, cw)).collect();
    let yt: Vec<_> = (0..ph).map(|y| taps(y, ch)).collect();
    let up = |c: &[f32], u: &mut [f32]| {
        for y in 0..ph {
            for x in 0..pw {
                let mut v = 0.0;
                for &(cy, wy) in &yt[y] {
                    for &(cx, wx) in &xt[x] {
                        v += wy * wx * c[cy * cw + cx];
                    }
                }
                u[y * pw + x] = v;
            }
        }
    };
    let adjoint = |u: &[f32], c: &mut [f32]| {
        c.fill(0.0);
        for y in 0..ph {
            for x in 0..pw {
                let v = u[y * pw + x];
                for &(cy, wy) in &yt[y] {
                    for &(cx, wx) in &xt[x] {
                        c[cy * cw + cx] += wy * wx * v;
                    }
                }
            }
        }
    };
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(a, b)| (a * b) as f64).sum::<f64>();
    let mut c = vec![0f32; cw * ch];
    for y in 0..ch {
        for x in 0..cw {
            let at = |dx, dy| full[(2 * y + dy) * pw + 2 * x + dx];
            c[y * cw + x] = (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4.0;
        }
    }
    let mut u = vec![0f32; pw * ph];
    let (mut r, mut ap) = (vec![0f32; cw * ch], vec![0f32; cw * ch]);
    up(&c, &mut u);
    for (u, f) in u.iter_mut().zip(full) {
        *u = f - *u;
    }
    adjoint(&u, &mut r);
    let mut p = r.clone();
    let mut rr = dot(&r, &r);
    for _ in 0..DOWNSAMPLE_ITERATIONS {
        if rr < 1e-6 {
            break;
        }
        up(&p, &mut u);
        adjoint(&u, &mut ap);
        let alpha = (rr / dot(&p, &ap)) as f32;
        for i in 0..c.len() {
            c[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }
        let next = dot(&r, &r);
        let beta = (next / rr) as f32;
        rr = next;
        for i in 0..p.len() {
            p[i] = r[i] + beta * p[i];
        }
    }
    for (o, v) in out.iter_mut().zip(&c) {
        *o = v.round().clamp(0.0, 255.0) as u8;
    }
}

/// Sum of absolute transformed differences of the `size` x `size` luma block at (`x0`,
/// `y0`) against its prediction: a better guess of the residual's bits than plain absolute
/// differences, which pick poor vectors on fine texture.
#[allow(clippy::too_many_arguments)]
fn satd(layout: &Layout, src: &[u8], reference: &[u8], x0: i32, y0: i32, size: usize, mv: Mv) -> u32 {
    let plane = &layout.planes[0];
    let w = plane.width as usize;
    let mut pred = [0u8; 256];
    predict_block(reference, plane, x0, y0, size, mv, &mut pred);
    let mut total = 0;
    for by in (0..size).step_by(8) {
        for bx in (0..size).step_by(8) {
            let mut d = [0i32; 64];
            for y in 0..8 {
                let row = &src[(y0 as usize + by + y) * w + x0 as usize + bx..][..8];
                for x in 0..8 {
                    d[y * 8 + x] = row[x] as i32 - pred[(by + y) * size + bx + x] as i32;
                }
            }
            total += hadamard8(&d);
        }
    }
    total
}

/// Sum of absolute Hadamard-transformed differences of an 8x8 block, scaled to the size of
/// absolute differences.
fn hadamard8(d: &[i32; 64]) -> u32 {
    let mut m = *d;
    for row in m.as_chunks_mut::<8>().0 {
        fwht8(row);
    }
    let mut total = 0;
    for x in 0..8 {
        let mut col = [0i32; 8];
        for y in 0..8 {
            col[y] = m[y * 8 + x];
        }
        fwht8(&mut col);
        total += col.iter().map(|v| v.unsigned_abs()).sum::<u32>();
    }
    total / 8
}

fn fwht8(v: &mut [i32]) {
    let mut h = 1;
    while h < 8 {
        for i in (0..8).step_by(2 * h) {
            for j in i..i + h {
                let (a, b) = (v[j], v[j + h]);
                v[j] = a + b;
                v[j + h] = a - b;
            }
        }
        h *= 2;
    }
}

/// Motion search of the `size` x `size` luma block at (`x0`, `y0`): the best of
/// `candidates` at integer positions, square steps of `steps` pixels, then half- and
/// quarter-pel refinement. Cost is SATD plus `lambda` times the bits of the difference to
/// `pred`.
#[allow(clippy::too_many_arguments)]
fn search_block(
    layout: &Layout,
    src: &[u8],
    reference: &[u8],
    x0: i32,
    y0: i32,
    size: usize,
    candidates: &[Mv],
    pred: Mv,
    lambda: f64,
    steps: &[i32],
) -> Mv {
    let src = &src[layout.planes[0].offset..];
    let reference = &reference[layout.planes[0].offset..];
    let cost = |mv: Mv, limit: f64| {
        let bits = mv_bits((mv.0 - pred.0, mv.1 - pred.1)) * lambda;
        if bits >= limit {
            return f64::MAX;
        }
        satd(layout, src, reference, x0, y0, size, mv) as f64 + bits
    };
    let mut best = (0, 0);
    let mut best_cost = f64::MAX;
    for &c in candidates {
        // Integer positions only during the integer search.
        let c = ((c.0 + 2) >> 2 << 2, (c.1 + 2) >> 2 << 2);
        let j = cost(c, best_cost);
        if j < best_cost {
            (best, best_cost) = (c, j);
        }
    }
    let ok = |mv: Mv| mv.0.abs() <= MAX_MV && mv.1.abs() <= MAX_MV;
    let refine = |best: &mut Mv, best_cost: &mut f64, step: i32| {
        for _ in 0..8 {
            let centre = *best;
            for (dx, dy) in [
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ] {
                let mv = (centre.0 + dx * step, centre.1 + dy * step);
                if !ok(mv) {
                    continue;
                }
                let j = cost(mv, *best_cost);
                if j < *best_cost {
                    (*best, *best_cost) = (mv, j);
                }
            }
            if *best == centre {
                break;
            }
        }
    };
    for &step in steps {
        refine(&mut best, &mut best_cost, step * 4);
    }
    refine(&mut best, &mut best_cost, 2);
    refine(&mut best, &mut best_cost, 1);
    best
}

/// The 16x16 vector of a macroblock and the vectors of its 8x8 quadrants.
#[allow(clippy::too_many_arguments)]
fn search_mb(
    layout: &Layout,
    src: &[u8],
    reference: &[u8],
    previous: &[Mv],
    mx: u32,
    my: u32,
    pred: Mv,
    lambda: f64,
) -> Searched {
    let at = |x: u32, y: u32| previous[(y * layout.mbs_x + x) as usize];
    let mut candidates = vec![(0, 0), pred, at(mx, my)];
    if mx > 0 {
        candidates.push(at(mx - 1, my));
    }
    if my > 0 {
        candidates.push(at(mx, my - 1));
    }
    if mx + 1 < layout.mbs_x {
        candidates.push(at(mx + 1, my));
    }
    if my + 1 < layout.mbs_y {
        candidates.push(at(mx, my + 1));
    }
    let (x0, y0) = ((mx * 16) as i32, (my * 16) as i32);
    let mv = search_block(
        layout, src, reference, x0, y0, 16, &candidates, pred, lambda, &SEARCH_STEPS,
    );
    let quadrants = [0, 1, 2, 3].map(|q| {
        let (qx, qy) = (x0 + q % 2 * 8, y0 + q / 2 * 8);
        search_block(layout, src, reference, qx, qy, 8, &[mv], mv, lambda, &SUB_STEPS)
    });
    Searched { mv, quadrants }
}

/// Forward transform matched to [`inverse_transform`]: 16 times the transform with the
/// integer basis scaled to unit DC norm.
fn forward_transform(r: &[i32; 64]) -> [f32; 64] {
    let c = |k: usize, n: usize| motion::DCT[k][n] as f32 / (64.0 * 8f32.sqrt());
    let mut tmp = [0f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            tmp[y * 8 + u] = (0..8).map(|x| c(u, x) * r[y * 8 + x] as f32).sum();
        }
    }
    let mut out = [0f32; 64];
    for v in 0..8 {
        for u in 0..8 {
            out[v * 8 + u] = 16.0 * (0..8).map(|y| c(v, y) * tmp[y * 8 + u]).sum::<f32>();
        }
    }
    out
}

/// Zigzag-order levels of a residual block.
fn quantize(r: &[i32; 64], qp: u8, round: f32) -> [i32; 64] {
    let step = (motion::DEQUANT[qp as usize % 6] << (qp / 6)) as f32 / 4.0;
    let c = forward_transform(r);
    let mut levels = [0i32; 64];
    for (k, l) in levels.iter_mut().enumerate() {
        let v = c[ZIGZAG[k] as usize];
        let q = ((v.abs() / step + round) as i32).min(MAX_LEVEL);
        *l = if v < 0.0 { -q } else { q };
    }
    levels
}

/// Approximate bits of a block's tokens.
fn block_bits(dc: i32, levels: &[i32; 64]) -> f64 {
    let mut bits = 1 + 2 * class(dc);
    let mut run = 0;
    for &l in &levels[1..] {
        if l == 0 {
            run += 1;
        } else {
            bits += 2 + 2 * class(l) + run / 2;
            run = 0;
        }
    }
    (bits + 2) as f64
}

/// Codes the macroblocks of one slice in order, keeping the slice's predictions.
struct SliceCoder<'a> {
    enc: &'a MotionEncoder,
    keyframe: bool,
    first_row: u32,
    lambda: f64,
    weights: [f64; 4],
    /// Vectors per 8x8 luma block.
    mvs: Vec<Mv>,
    dc_pred: [i32; 4],
    tokens: Vec<Token>,
}

/// A candidate coding of a macroblock with its reconstruction and cost.
struct Candidate {
    code: MbCode,
    cost: f64,
}

impl<'a> SliceCoder<'a> {
    fn new(enc: &'a MotionEncoder, keyframe: bool, first_row: u32) -> SliceCoder<'a> {
        SliceCoder {
            enc,
            keyframe,
            first_row,
            lambda: lambda(enc.qp),
            weights: plane_weights(&enc.layout),
            mvs: vec![(0, 0); 4 * enc.layout.mb_count()],
            dc_pred: [0; 4],
            tokens: Vec::new(),
        }
    }

    fn code_mb(&mut self, mx: u32, my: u32, searched: Option<Searched>) -> MbCode {
        let pred_mv = self.predictor(mx, my, 0, 2);
        let mut best = self.candidate(mx, my, MB_INTRA, [(0, 0); 4], 0.0);
        if !self.keyframe {
            let skip = self.candidate(mx, my, MB_SKIP, [pred_mv; 4], 0.0);
            if skip.cost < best.cost {
                best = skip;
            }
            if let Some(found) = searched {
                let bits = mv_bits((found.mv.0 - pred_mv.0, found.mv.1 - pred_mv.1));
                let inter = self.candidate(mx, my, MB_INTER, [found.mv; 4], bits);
                if inter.cost < best.cost {
                    best = inter;
                }
                if found.quadrants.iter().any(|&q| q != found.mv) {
                    let bits = self.split_bits(mx, my, &found.quadrants);
                    let split = self.candidate(mx, my, MB_SPLIT, found.quadrants, bits);
                    if split.cost < best.cost {
                        best = split;
                    }
                }
            }
        }
        let mut code = best.code;
        let nothing_coded = code.blocks.iter().all(|(_, coded)| !coded);
        if code.kind == MB_INTER && code.mvs[0] == pred_mv && nothing_coded {
            code.kind = MB_SKIP;
        }
        self.emit(mx, my, &code, pred_mv);
        code
    }

    fn predictor(&self, mx: u32, my: u32, sub: u32, width: u32) -> Mv {
        let mbs_x = self.enc.layout.mbs_x;
        mv_predictor(&self.mvs, mbs_x, self.first_row, mx, my, sub, width)
    }

    /// Approximate bits of the vectors of a split macroblock. Leaves them in the vector
    /// field, which [`SliceCoder::emit`] overwrites.
    fn split_bits(&mut self, mx: u32, my: u32, mvs: &[Mv; 4]) -> f64 {
        let mut bits = 0.0;
        for (sub, mv) in (0..4).zip(mvs) {
            let pred = self.predictor(mx, my, sub, 1);
            bits += mv_bits((mv.0 - pred.0, mv.1 - pred.1));
            set_field(&mut self.mvs, self.enc.layout.mbs_x, mx, my, sub..sub + 1, *mv);
        }
        bits
    }

    /// Codes the macroblock as `kind` with quadrant vectors `mvs` whose differences cost
    /// `mv_bits`, and returns its cost.
    fn candidate(&self, mx: u32, my: u32, kind: u32, mvs: [Mv; 4], mv_bits: f64) -> Candidate {
        let enc = self.enc;
        let layout = &enc.layout;
        let mut cost = 0.0;
        let mut bits = 2.0 + mv_bits;
        let mut blocks = Vec::with_capacity(16);
        let mut dc_pred = self.dc_pred;
        for (p, bx, by) in layout.mb_blocks(mx, my) {
            let plane = &layout.planes[p];
            let (pred, src) = self.block_pixels(plane, bx, by, kind, &mvs);
            let weight = self.weights[p];
            let pred_err: f64 = src
                .iter()
                .zip(&pred)
                .map(|(&s, &q)| ((s - q) * (s - q)) as f64)
                .sum::<f64>()
                * weight;
            if kind == MB_SKIP {
                cost += pred_err;
                blocks.push(([0; 64], false));
                continue;
            }
            let mut residual = [0i32; 64];
            for i in 0..64 {
                residual[i] = src[i] - pred[i];
            }
            let qp = plane_qp(enc.qp, p, &enc.params);
            let levels = quantize(
                &residual,
                qp,
                if kind == MB_INTRA {
                    ROUND_INTRA
                } else {
                    ROUND_INTER
                },
            );
            if levels.iter().all(|&l| l == 0) {
                cost += pred_err;
                blocks.push((levels, false));
                continue;
            }
            let mut natural = [0i32; 64];
            for (k, &l) in levels.iter().enumerate() {
                natural[ZIGZAG[k] as usize] = dequantize(l, qp);
            }
            let r = inverse_transform(&natural);
            let err: f64 = (0..64)
                .map(|i| {
                    let v = (pred[i] + r[i]).clamp(0, 255);
                    ((src[i] - v) * (src[i] - v)) as f64
                })
                .sum::<f64>()
                * weight;
            let dc = if kind == MB_INTRA {
                levels[0] - dc_pred[p]
            } else {
                levels[0]
            };
            let coded_bits = block_bits(dc, &levels);
            // Leave inter blocks uncoded when the bits are not worth the error they remove.
            if kind != MB_INTRA && pred_err <= err + self.lambda * coded_bits {
                cost += pred_err;
                blocks.push((levels.map(|_| 0), false));
                continue;
            }
            if kind == MB_INTRA {
                dc_pred[p] = levels[0];
            }
            cost += err;
            bits += coded_bits;
            blocks.push((levels, true));
        }
        Candidate {
            code: MbCode { kind, mvs, blocks },
            cost: cost + self.lambda * bits,
        }
    }

    /// Prediction and source pixels of a transform block.
    fn block_pixels(
        &self,
        plane: &Plane,
        bx: u32,
        by: u32,
        kind: u32,
        mvs: &[Mv; 4],
    ) -> ([i32; 64], [i32; 64]) {
        let enc = self.enc;
        let mut pred = [128i32; 64];
        let mut src = [0i32; 64];
        let mut block = [0u8; 64];
        if kind != MB_INTRA {
            let reference = &enc.reference[plane.offset..];
            predict_transform_block(reference, plane, bx, by, mvs, &mut block);
            pred = block.map(|v| v as i32);
        }
        for y in 0..8 {
            for x in 0..8 {
                let (px, py) = (bx * 8 + x, by * 8 + y);
                src[(y * 8 + x) as usize] =
                    enc.src[plane.offset + (py * plane.width + px) as usize] as i32;
            }
        }
        (pred, src)
    }

    /// Appends the tokens of a decided macroblock and updates the vector field and DC
    /// predictions.
    fn emit(&mut self, mx: u32, my: u32, code: &MbCode, pred_mv: Mv) {
        let layout = &self.enc.layout;
        let mut cbp = 0;
        let (mut chroma, mut alpha) = (false, false);
        for ((p, _, _), (_, coded)) in layout.mb_blocks(mx, my).zip(&code.blocks) {
            if !coded {
                continue;
            }
            match p {
                0 => {}
                3 => alpha = true,
                _ => chroma = true,
            }
        }
        for (i, (_, coded)) in code.blocks.iter().take(4).enumerate() {
            cbp |= (*coded as u32) << i;
        }
        let sym = code.kind | cbp << 2 | (chroma as u32) << 6 | (alpha as u32) << 7;
        self.tokens.push(Token::new(
            TABLE_MB,
            if code.kind == MB_SKIP { 0 } else { sym },
            0,
            0,
        ));
        let mbs_x = layout.mbs_x;
        let mut mv_tokens = |pred: Mv, mv: Mv| {
            self.tokens.push(Token::value(TABLE_MV, mv.0 - pred.0));
            self.tokens.push(Token::value(TABLE_MV, mv.1 - pred.1));
        };
        match code.kind {
            MB_SPLIT => {
                for sub in 0..4 {
                    let pred = mv_predictor(&self.mvs, mbs_x, self.first_row, mx, my, sub, 1);
                    let mv = code.mvs[sub as usize];
                    mv_tokens(pred, mv);
                    set_field(&mut self.mvs, mbs_x, mx, my, sub..sub + 1, mv);
                }
            }
            kind => {
                if kind == MB_INTER {
                    mv_tokens(pred_mv, code.mvs[0]);
                }
                set_field(&mut self.mvs, mbs_x, mx, my, 0..4, code.mvs[0]);
            }
        }
        if code.kind == MB_SKIP {
            return;
        }
        for ((p, _, _), (levels, coded)) in layout.mb_blocks(mx, my).zip(&code.blocks) {
            let sent = match p {
                0 => *coded,
                3 => alpha,
                _ => chroma,
            };
            if !sent {
                continue;
            }
            // Blocks of a sent chroma or alpha group that were left uncoded go as zeros.
            let levels = if *coded { *levels } else { [0; 64] };
            let mut dc = levels[0];
            if code.kind == MB_INTRA && *coded {
                dc -= self.dc_pred[p];
                self.dc_pred[p] = levels[0];
            } else if code.kind == MB_INTRA {
                dc -= self.dc_pred[p];
                self.dc_pred[p] = 0;
            }
            block_tokens(p, dc, &levels, &mut self.tokens);
        }
    }
}
