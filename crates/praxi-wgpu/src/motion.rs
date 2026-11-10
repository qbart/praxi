//! The GPU decoder of `motion` tracks (`docs/features/motion-codec.md#gpu-decoder-praxi-wgpu`):
//! compute passes that rebuild the frame planes from coded frames, exactly as
//! `praxi::format::motion::reconstruct`, and convert them to RGBA.

use praxi::format::motion::{CodedFrame, DCT, FILTER_4, FILTER_8, Layout, MB_INTRA, MB_WORDS};
use std::time::Duration;

/// Bytes of the `Params` uniform: 4 planes of 32 bytes, then 3 rows of 16.
const PARAMS_SIZE: usize = 4 * 32 + 3 * 16;
/// Invocations per workgroup of the prediction pass.
const PREDICT_GROUP: u32 = 64;
/// Most workgroups along one dispatch dimension.
const MAX_GROUPS: u32 = 65535;

/// Shared by the three shaders. Plane offsets are in words of 4 pixels.
const COMMON: &str = r#"
struct Plane {
    width: u32,
    height: u32,
    offset: u32,
    first_block: u32,
    shift: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

struct Params {
    planes: array<Plane, 4>,
    plane_count: u32,
    mbs_x: u32,
    width: u32,
    height: u32,
    block_count: u32,
    words: u32,
    _pad0: u32,
    _pad1: u32,
    scales: vec4<i32>,
};

@group(0) @binding(0) var<uniform> params: Params;

fn byte_of(word: u32, k: u32) -> i32 {
    return i32((word >> (k * 8u)) & 255u);
}
"#;

/// Prediction of every plane word from the reference planes (`motion::predict`).
const PREDICT: &str = r#"
@group(0) @binding(1) var<storage, read> reference: array<u32>;
@group(0) @binding(2) var<storage, read_write> planes: array<u32>;
@group(0) @binding(3) var<storage, read> mbs: array<u32>;

var<private> FILTER_8: array<i32, 32> = array<i32, 32>(FILTER_8_VALUES);
var<private> FILTER_4: array<i32, 32> = array<i32, 32>(FILTER_4_VALUES);

// Pixel (x, y) of plane `pl` of the reference, coordinates clamped to the plane.
fn ref_pixel(pl: Plane, x: i32, y: i32) -> i32 {
    let cx = u32(clamp(x, 0, i32(pl.width) - 1));
    let cy = u32(clamp(y, 0, i32(pl.height) - 1));
    let i = pl.offset * 4u + cy * pl.width + cx;
    return byte_of(reference[i >> 2u], i & 3u);
}

fn tap(shift: u32, f: i32, i: i32) -> i32 {
    if shift == 0u {
        return FILTER_8[f * 8 + i];
    }
    return FILTER_4[f * 4 + i];
}

@compute @workgroup_size(PREDICT_GROUP)
fn predict(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let w = gid.x + gid.y * groups.x * PREDICT_GROUPu;
    if w >= params.words {
        return;
    }
    var p = 0u;
    for (var i = 1u; i < params.plane_count; i++) {
        if w >= params.planes[i].offset {
            p = i;
        }
    }
    let pl = params.planes[p];
    let at = (w - pl.offset) * 4u;
    let x = at % pl.width;
    let y = at / pl.width;
    let mb = ((y << pl.shift) / 16u * params.mbs_x + (x << pl.shift) / 16u) * MB_WORDSu;
    if mbs[mb] == MB_INTRAu {
        planes[w] = 0x80808080u;
        return;
    }
    // The quadrant whose vector predicts these pixels: of the 8x8 block in full-resolution
    // planes, of the 4x4 part in 4:2:0 chroma.
    var q: u32;
    if pl.shift == 0u {
        q = (y / 8u) % 2u * 2u + (x / 8u) % 2u;
    } else {
        q = (y % 8u) / 4u * 2u + (x % 8u) / 4u;
    }
    let mv = mbs[mb + 1u + q];
    let mvx = bitcast<i32>(mv << 16u) >> 16u;
    let mvy = bitcast<i32>(mv) >> 16u;
    let fb = 2u + pl.shift;
    let fx = mvx & ((1 << fb) - 1);
    let fy = mvy & ((1 << fb) - 1);
    let ix = i32(x) + (mvx >> fb);
    let iy = i32(y) + (mvy >> fb);
    var word = 0u;
    if fx == 0 && fy == 0 {
        for (var k = 0u; k < 4u; k++) {
            word |= u32(ref_pixel(pl, ix + i32(k), iy)) << (k * 8u);
        }
        planes[w] = word;
        return;
    }
    var taps = 4;
    if pl.shift == 0u {
        taps = 8;
    }
    let o = taps / 2 - 1;
    var sums = vec4<i32>(0);
    for (var j = 0; j < taps; j++) {
        // The reference pixels of this row under the 4 outputs' horizontal taps.
        var row: array<i32, 11>;
        for (var i = 0; i < taps + 3; i++) {
            row[i] = ref_pixel(pl, ix - o + i, iy - o + j);
        }
        var h = vec4<i32>(0);
        for (var i = 0; i < taps; i++) {
            let c = tap(pl.shift, fx, i);
            h += c * vec4<i32>(row[i], row[i + 1], row[i + 2], row[i + 3]);
        }
        sums += tap(pl.shift, fy, j) * h;
    }
    let v = clamp((sums + 2048) >> vec4<u32>(12u), vec4<i32>(0), vec4<i32>(255));
    planes[w] = u32(v.x) | u32(v.y) << 8u | u32(v.z) << 16u | u32(v.w) << 24u;
}
"#;

/// Dequantization, inverse transform and addition of each coded block, one workgroup per
/// block (`motion::reconstruct`).
const RESIDUAL: &str = r#"
@group(0) @binding(1) var<storage, read_write> planes: array<u32>;
@group(0) @binding(2) var<storage, read> blocks: array<u32>;
@group(0) @binding(3) var<storage, read> coeffs: array<u32>;

var<private> DCT: array<i32, 64> = array<i32, 64>(DCT_VALUES);
var<workgroup> c: array<i32, 64>;
var<workgroup> g: array<i32, 64>;

fn dequant(level: i32, scale: i32) -> i32 {
    let v = (abs(level) * scale) >> 2u;
    return clamp(v * sign(level), -32768, 32767);
}

@compute @workgroup_size(64)
fn residual(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(local_invocation_index) t: u32,
) {
    let b = group.x + group.y * groups.x;
    if b >= params.block_count {
        return;
    }
    let head = blocks[2u * b];
    let first = blocks[2u * b + 1u];
    let index = head & 0x1ffffffu;
    var p = 0u;
    for (var i = 1u; i < params.plane_count; i++) {
        if index >= params.planes[i].first_block {
            p = i;
        }
    }
    c[t] = 0;
    workgroupBarrier();
    if t < (head >> 25u) {
        let e = coeffs[first + t];
        c[e & 63u] = dequant(bitcast<i32>(e) >> 16u, params.scales[p]);
    }
    workgroupBarrier();
    // Columns: invocation t computes g[y][u] with y = t / 8, u = t % 8.
    let lo = t % 8u;
    let hi = t / 8u;
    var e = 0;
    for (var v = 0u; v < 8u; v++) {
        e += DCT[v * 8u + hi] * c[v * 8u + lo];
    }
    g[t] = clamp((e + 64) >> 7u, -32768, 32767);
    workgroupBarrier();
    // Rows: invocation t computes r[y][x] with y = t / 8, x = t % 8.
    var r = 0;
    for (var u = 0u; u < 8u; u++) {
        r += DCT[u * 8u + lo] * g[hi * 8u + u];
    }
    c[t] = (r + 2048) >> 12u;
    workgroupBarrier();
    if t < 16u {
        let pl = params.planes[p];
        let n = index - pl.first_block;
        let bx = n % (pl.width / 8u);
        let by = n / (pl.width / 8u);
        let y = t / 2u;
        let x = t % 2u * 4u;
        let w = pl.offset + ((by * 8u + y) * pl.width + bx * 8u + x) / 4u;
        let old = planes[w];
        var word = 0u;
        for (var k = 0u; k < 4u; k++) {
            let v = clamp(byte_of(old, k) + c[y * 8u + x + k], 0, 255);
            word |= u32(v) << (k * 8u);
        }
        planes[w] = word;
    }
}
"#;

/// Conversion of the planes to RGBA (`motion::to_rgba`).
const COLOR: &str = r#"
@group(0) @binding(1) var<storage, read> planes: array<u32>;
@group(0) @binding(2) var rgba: texture_storage_2d<rgba8unorm, write>;

fn pixel(pl: Plane, x: i32, y: i32) -> i32 {
    let cx = u32(clamp(x, 0, i32(pl.width) - 1));
    let cy = u32(clamp(y, 0, i32(pl.height) - 1));
    let i = pl.offset * 4u + cy * pl.width + cx;
    return byte_of(planes[i >> 2u], i & 3u);
}

// The nearer sample (weight 3) and the farther one (weight 1) along one axis.
fn near_far(v: u32) -> vec2<i32> {
    let i = i32(v >> 1u);
    if v % 2u == 0u {
        return vec2<i32>(i, i - 1);
    }
    return vec2<i32>(i, i + 1);
}

fn chroma(pl: Plane, x: u32, y: u32) -> i32 {
    if pl.shift == 0u {
        return pixel(pl, i32(x), i32(y));
    }
    let h = near_far(x);
    let v = near_far(y);
    return (9 * pixel(pl, h.x, v.x) + 3 * pixel(pl, h.y, v.x) + 3 * pixel(pl, h.x, v.y)
        + pixel(pl, h.y, v.y) + 8) >> 4u;
}

@compute @workgroup_size(8, 8)
fn color(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height {
        return;
    }
    let x = gid.x;
    let y = gid.y;
    let luma = pixel(params.planes[0], i32(x), i32(y));
    let co = chroma(params.planes[1], x, y) - 128;
    let cg = chroma(params.planes[2], x, y) - 128;
    let t = luma - cg;
    var a = 255;
    if params.plane_count == 4u {
        a = pixel(params.planes[3], i32(x), i32(y));
    }
    let v = clamp(vec4<i32>(t + co, luma + cg, t - co, a), vec4<i32>(0), vec4<i32>(255));
    textureStore(rgba, vec2<u32>(x, y), vec4<f32>(v) / 255.0);
}
"#;

/// The WGSL source of one pass, constants filled in.
fn source(pass: &str) -> String {
    let list = |v: &mut dyn Iterator<Item = i32>| v.map(|x| x.to_string()).collect::<Vec<_>>().join(", ");
    let pass = pass
        .replace("FILTER_8_VALUES", &list(&mut FILTER_8.iter().flatten().copied()))
        .replace("FILTER_4_VALUES", &list(&mut FILTER_4.iter().flatten().copied()))
        .replace("DCT_VALUES", &list(&mut DCT.iter().flatten().copied()))
        .replace("PREDICT_GROUP", &PREDICT_GROUP.to_string())
        .replace("MB_WORDS", &MB_WORDS.to_string())
        .replace("MB_INTRA", &MB_INTRA.to_string());
    format!("{COMMON}{pass}")
}

/// The device limits the passes need, raised from `base`.
fn raise(base: wgpu::Limits) -> wgpu::Limits {
    const BUFFER: u64 = 128 << 20;
    wgpu::Limits {
        max_storage_buffers_per_shader_stage: base.max_storage_buffers_per_shader_stage.max(3),
        max_storage_textures_per_shader_stage: base.max_storage_textures_per_shader_stage.max(1),
        max_storage_buffer_binding_size: base.max_storage_buffer_binding_size.max(BUFFER),
        max_buffer_size: base.max_buffer_size.max(BUFFER),
        max_compute_workgroup_storage_size: base.max_compute_workgroup_storage_size.max(512),
        max_compute_invocations_per_workgroup: base.max_compute_invocations_per_workgroup.max(64),
        max_compute_workgroup_size_x: base.max_compute_workgroup_size_x.max(64),
        max_compute_workgroup_size_y: base.max_compute_workgroup_size_y.max(8),
        max_compute_workgroup_size_z: base.max_compute_workgroup_size_z.max(1),
        max_compute_workgroups_per_dimension: base.max_compute_workgroups_per_dimension.max(65535),
        ..base
    }
}

/// Whether the adapter can run the motion decoder: compute shaders, and the limits of
/// `docs/features/motion-codec.md#gpu-decoder-praxi-wgpu`.
pub fn supported(adapter: &wgpu::Adapter) -> bool {
    let limits = adapter.limits();
    adapter.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        && raise(wgpu::Limits::downlevel_webgl2_defaults()).check_limits(&limits)
}

/// `base` raised to the limits the motion decoder needs, when the adapter supports it.
pub fn required_limits(adapter: &wgpu::Adapter, base: wgpu::Limits) -> wgpu::Limits {
    if supported(adapter) { raise(base) } else { base }
}

/// Binding types of the passes, in binding order after the uniform.
#[derive(Clone, Copy)]
enum Slot {
    Read,
    ReadWrite,
    Rgba,
}

/// The compute pipelines, shared by the tracks of a renderer.
pub(crate) struct Pipelines {
    predict: (wgpu::ComputePipeline, wgpu::BindGroupLayout),
    residual: (wgpu::ComputePipeline, wgpu::BindGroupLayout),
    color: (wgpu::ComputePipeline, wgpu::BindGroupLayout),
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device) -> Pipelines {
        let pipeline = |name: &str, code: &str, slots: &[Slot]| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(name),
                source: wgpu::ShaderSource::Wgsl(source(code).into()),
            });
            let mut entries = vec![wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }];
            for (i, slot) in slots.iter().enumerate() {
                let ty = match slot {
                    Slot::Read | Slot::ReadWrite => wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: matches!(slot, Slot::Read) },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    Slot::Rgba => wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                };
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding: i as u32 + 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty,
                    count: None,
                });
            }
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(name),
                entries: &entries,
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(name),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            });
            (pipeline, layout)
        };
        use Slot::*;
        Pipelines {
            predict: pipeline("predict", PREDICT, &[Read, ReadWrite, Read]),
            residual: pipeline("residual", RESIDUAL, &[ReadWrite, Read, Read]),
            color: pipeline("color", COLOR, &[Read, Rgba]),
        }
    }
}

/// The bind groups of one orientation of the plane buffers.
struct BindGroups {
    predict: wgpu::BindGroup,
    residual: wgpu::BindGroup,
    color: wgpu::BindGroup,
}

/// The GPU state of one `motion` track.
pub(crate) struct Decoder {
    layout: Layout,
    params: wgpu::Buffer,
    /// The uniform without the per-frame block count and scales.
    params_bytes: [u8; PARAMS_SIZE],
    /// Two frame buffers: the frame being built and its reference.
    planes: [wgpu::Buffer; 2],
    /// Which of `planes` holds the latest frame.
    current: usize,
    mbs: wgpu::Buffer,
    blocks: wgpu::Buffer,
    coeffs: wgpu::Buffer,
    view: wgpu::TextureView,
    /// Index `i` builds into `planes[i]` from the other plane buffer.
    groups: [BindGroups; 2],
    timing: Option<Timing>,
}

/// Most frames timed between two reads of the times.
const TIMED_FRAMES: usize = 64;
/// Timestamps per timed frame: start and end of the prediction, residual and color passes.
const FRAME_QUERIES: usize = 6;

/// Timestamp queries of the frames reconstructed since the times were last read.
struct Timing {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// Keyframe flag and whether it was converted to RGBA, per timed frame.
    frames: Vec<(bool, bool)>,
}

impl Timing {
    fn new(device: &wgpu::Device) -> Timing {
        let size = (TIMED_FRAMES * FRAME_QUERIES * 8) as u64;
        let buffer = |label, usage| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false });
        Timing {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("motion timing"),
                ty: wgpu::QueryType::Timestamp,
                count: (TIMED_FRAMES * FRAME_QUERIES) as u32,
            }),
            resolve: buffer("motion timing resolve", wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC),
            readback: buffer("motion timing readback", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST),
            frames: Vec::new(),
        }
    }
}

/// GPU time of one reconstructed frame.
pub(crate) struct FrameTime {
    pub keyframe: bool,
    pub predict: Duration,
    pub residual: Duration,
    pub color: Option<Duration>,
}

/// Byte size of a storage buffer of `words` words, at least one word.
fn words_size(words: usize) -> u64 {
    (words.max(1) * 4) as u64
}

fn storage_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

impl Decoder {
    /// `texture` is the track's `Rgba8Unorm` texture, created with `STORAGE_BINDING`.
    pub(crate) fn new(device: &wgpu::Device, pipelines: &Pipelines, layout: Layout, texture: &wgpu::Texture) -> Decoder {
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("motion params"),
            size: PARAMS_SIZE as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut params_bytes = [0u8; PARAMS_SIZE];
        let mut put = |at: usize, v: u32| params_bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
        for (i, p) in layout.planes.iter().enumerate() {
            for (k, v) in [p.width, p.height, (p.offset / 4) as u32, p.first_block, p.shift].into_iter().enumerate() {
                put(32 * i + 4 * k, v);
            }
        }
        put(128, layout.planes.len() as u32);
        put(132, layout.mbs_x);
        put(136, layout.width);
        put(140, layout.height);
        put(148, (layout.size / 4) as u32);
        let planes = [0, 1].map(|_| storage_buffer(device, "motion planes", layout.size as u64));
        let mbs = storage_buffer(device, "motion macroblocks", words_size(layout.mb_count() * MB_WORDS));
        let blocks = storage_buffer(device, "motion blocks", words_size(0));
        let coeffs = storage_buffer(device, "motion coefficients", words_size(0));
        let view = texture.create_view(&Default::default());
        let groups = bind_groups(device, pipelines, &params, &planes, &mbs, &blocks, &coeffs, &view);
        Decoder { layout, params, params_bytes, planes, current: 0, mbs, blocks, coeffs, view, groups, timing: None }
    }

    /// Starts or stops timing the passes; the device must have `TIMESTAMP_QUERY`.
    pub(crate) fn set_timing(&mut self, device: &wgpu::Device, enabled: bool) {
        if enabled != self.timing.is_some() {
            self.timing = enabled.then(|| Timing::new(device));
        }
    }

    /// The GPU times of the frames timed since the last call, waiting for the GPU.
    pub(crate) fn read_times(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<FrameTime> {
        let Some(timing) = &mut self.timing else { return Vec::new() };
        let frames = std::mem::take(&mut timing.frames);
        if frames.is_empty() {
            return Vec::new();
        }
        let count = frames.len() * FRAME_QUERIES;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("motion timing") });
        encoder.resolve_query_set(&timing.queries, 0..count as u32, &timing.resolve, 0);
        encoder.copy_buffer_to_buffer(&timing.resolve, 0, &timing.readback, 0, (count * 8) as u64);
        queue.submit([encoder.finish()]);
        let slice = timing.readback.slice(..(count * 8) as u64);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).expect("GPU wait");
        let ticks: Vec<u64> = bytemuck::cast_slice(&slice.get_mapped_range().expect("mapped timestamps")).to_vec();
        timing.readback.unmap();
        let period = queue.get_timestamp_period() as f64;
        let span = |i: usize| Duration::from_nanos((ticks[i + 1].saturating_sub(ticks[i]) as f64 * period) as u64);
        frames
            .iter()
            .enumerate()
            .map(|(f, &(keyframe, shown))| {
                let at = f * FRAME_QUERIES;
                FrameTime { keyframe, predict: span(at), residual: span(at + 2), color: shown.then(|| span(at + 4)) }
            })
            .collect()
    }

    /// The plane buffer holding the latest frame, in the layout of `motion::Layout`.
    #[cfg(test)]
    pub(crate) fn planes(&self) -> &wgpu::Buffer {
        &self.planes[self.current]
    }

    /// Reconstructs `frame` from the previous frame, then converts it to RGBA when `show`.
    pub(crate) fn decode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipelines: &Pipelines,
        frame: &CodedFrame,
        show: bool,
    ) {
        let mut grew = false;
        for (buffer, words) in [(&mut self.blocks, frame.blocks.len()), (&mut self.coeffs, frame.coeffs.len())] {
            if buffer.size() < words_size(words) {
                *buffer = storage_buffer(device, "motion coded frame", words_size(words.next_power_of_two()));
                grew = true;
            }
        }
        if grew {
            self.groups = bind_groups(
                device,
                pipelines,
                &self.params,
                &self.planes,
                &self.mbs,
                &self.blocks,
                &self.coeffs,
                &self.view,
            );
        }
        let block_count = frame.block_count() as u32;
        let mut bytes = self.params_bytes;
        bytes[144..148].copy_from_slice(&block_count.to_le_bytes());
        for (i, s) in frame.scales.iter().enumerate() {
            bytes[160 + 4 * i..164 + 4 * i].copy_from_slice(&s.to_le_bytes());
        }
        queue.write_buffer(&self.params, 0, &bytes);
        for (buffer, data) in [(&self.mbs, &frame.mbs), (&self.blocks, &frame.blocks), (&self.coeffs, &frame.coeffs)] {
            if !data.is_empty() {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(data));
            }
        }
        let next = 1 - self.current;
        let groups = &self.groups[next];
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("motion") });
        // Timed frames run each stage in its own pass, with timestamps at its start and end.
        let timed = self.timing.as_mut().filter(|t| t.frames.len() < TIMED_FRAMES).map(|t| {
            t.frames.push((frame.keyframe, show));
            (&t.queries, ((t.frames.len() - 1) * FRAME_QUERIES) as u32)
        });
        let writes = |stage: u32| {
            timed.map(|(query_set, base)| wgpu::ComputePassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(base + 2 * stage),
                end_of_pass_write_index: Some(base + 2 * stage + 1),
            })
        };
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("motion"), timestamp_writes: writes(0) });
        pass.set_pipeline(&pipelines.predict.0);
        pass.set_bind_group(0, &groups.predict, &[]);
        let (x, y) = split((self.layout.size / 4) as u32, PREDICT_GROUP);
        pass.dispatch_workgroups(x, y, 1);
        if timed.is_some() {
            drop(pass);
            pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("motion residual"), timestamp_writes: writes(1) });
        }
        if block_count > 0 {
            pass.set_pipeline(&pipelines.residual.0);
            pass.set_bind_group(0, &groups.residual, &[]);
            let (x, y) = split(block_count, 1);
            pass.dispatch_workgroups(x, y, 1);
        }
        if show {
            if timed.is_some() {
                drop(pass);
                pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("motion color"), timestamp_writes: writes(2) });
            }
            pass.set_pipeline(&pipelines.color.0);
            pass.set_bind_group(0, &groups.color, &[]);
            pass.dispatch_workgroups(self.layout.width.div_ceil(8), self.layout.height.div_ceil(8), 1);
        }
        drop(pass);
        queue.submit([encoder.finish()]);
        self.current = next;
    }
}

/// The bind groups of both orientations of `planes`.
#[allow(clippy::too_many_arguments)]
fn bind_groups(
    device: &wgpu::Device,
    pipelines: &Pipelines,
    params: &wgpu::Buffer,
    planes: &[wgpu::Buffer; 2],
    mbs: &wgpu::Buffer,
    blocks: &wgpu::Buffer,
    coeffs: &wgpu::Buffer,
    view: &wgpu::TextureView,
) -> [BindGroups; 2] {
    let group = |label, layout: &wgpu::BindGroupLayout, resources: &[wgpu::BindingResource<'_>]| {
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() }];
        for (i, r) in resources.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry { binding: i as u32 + 1, resource: r.clone() });
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(label), layout, entries: &entries })
    };
    [0, 1].map(|i| {
        let (reference, current) = (&planes[1 - i], &planes[i]);
        BindGroups {
            predict: group(
                "predict",
                &pipelines.predict.1,
                &[reference.as_entire_binding(), current.as_entire_binding(), mbs.as_entire_binding()],
            ),
            residual: group(
                "residual",
                &pipelines.residual.1,
                &[current.as_entire_binding(), blocks.as_entire_binding(), coeffs.as_entire_binding()],
            ),
            color: group("color", &pipelines.color.1, &[current.as_entire_binding(), wgpu::BindingResource::TextureView(view)]),
        }
    })
}

/// Workgroup counts along x and y covering `items` items of `per_group`.
fn split(items: u32, per_group: u32) -> (u32, u32) {
    let groups = items.div_ceil(per_group).max(1);
    if groups <= MAX_GROUPS {
        (groups, 1)
    } else {
        (MAX_GROUPS, groups.div_ceil(MAX_GROUPS))
    }
}
