//! Texture upload and drawing of praxi video for `wgpu` engines
//! (`docs/features/engine-integration.md#praxi-wgpu`).

mod motion;

use praxi::{Capabilities, Player, TextureFormat};
use std::io::{Read, Seek};
use std::time::Duration;

pub use motion::{required_limits, supported as supports_motion};

/// BC formats in order of preference, then ASTC.
const BC: [TextureFormat; 5] =
    [TextureFormat::Bc7, TextureFormat::Bc3, TextureFormat::Bc1, TextureFormat::Bc5, TextureFormat::Bc4];
const ASTC: [TextureFormat; 4] =
    [TextureFormat::Astc4x4, TextureFormat::Astc5x5, TextureFormat::Astc6x6, TextureFormat::Astc8x8];
/// HDR formats of each family (`docs/features/hdr.md#praxi-wgpu`).
const BC_HDR: [TextureFormat; 1] = [TextureFormat::Bc6h];
const ASTC_HDR: [TextureFormat; 2] = [TextureFormat::Astc4x4Hdr, TextureFormat::Astc6x6Hdr];

const SHADER: &str = r#"
struct Params {
    uv_scale: vec2<f32>,
    uv_max: vec2<f32>,
    opacity: f32,
    // 1 when the texture holds sRGB-encoded bytes in a non-sRGB format.
    decode_srgb: f32,
    // HDR tracks: 1 for BT.2020 primaries (converted to BT.709).
    bt2020: f32,
    // HDR tracks drawn into an SDR target: the tonemap white; 0 draws linear values.
    tonemap_white: f32,
    // 0: by target format (tonemap or linear), 1: scRGB, 2: BT.2100 PQ.
    output: f32,
    // scRGB and PQ: the encoded linear value of 1.0 (docs/features/hdr.md#hdr-output).
    scale: f32,
    pad: vec2<f32>,
};

// Columns of the BT.2020 to BT.709 matrix (docs/spec/PRAXI.md#hdr-color).
const BT2020_TO_BT709 = mat3x3<f32>(
    vec3<f32>(1.6605, -0.1246, -0.0182),
    vec3<f32>(-0.5876, 1.1329, -0.1006),
    vec3<f32>(-0.0728, -0.0083, 1.1187),
);
// Columns of the BT.709 to BT.2020 matrix.
const BT709_TO_BT2020 = mat3x3<f32>(
    vec3<f32>(0.6274, 0.0691, 0.0164),
    vec3<f32>(0.3293, 0.9195, 0.0880),
    vec3<f32>(0.0433, 0.0114, 0.8956),
);

// SMPTE ST 2084 inverse EOTF of luminance relative to 10000 nits.
fn pq(l: vec3<f32>) -> vec3<f32> {
    let y = select(pow(l, vec3<f32>(2610.0 / 16384.0)), vec3<f32>(0.0), l <= vec3<f32>(0.0));
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 4096.0 * 32.0;
    let c3 = 2392.0 / 4096.0 * 32.0;
    return pow((c1 + c2 * y) / (1.0 + c3 * y), vec3<f32>(2523.0 / 4096.0 * 128.0));
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// A quad covering the viewport, drawn as a 4 vertex triangle strip.
@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    var out: VsOut;
    out.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_video(in: VsOut) -> @location(0) vec4<f32> {
    // Clamp to the last texel centre so filtering never reads block padding.
    let c = textureSample(tex, samp, min(in.uv * params.uv_scale, params.uv_max));
    var rgb = c.rgb;
    if params.decode_srgb > 0.5 {
        rgb = select(pow((rgb + 0.055) / 1.055, vec3<f32>(2.4)), rgb / 12.92, rgb <= vec3<f32>(0.04045));
    }
    if params.output > 1.5 {
        // HDR10: BT.2020, PQ-encoded.
        if params.bt2020 < 0.5 {
            rgb = BT709_TO_BT2020 * rgb;
        }
        return vec4<f32>(pq(min(max(rgb, vec3<f32>(0.0)) * params.scale, vec3<f32>(1.0))), c.a * params.opacity);
    }
    if params.bt2020 > 0.5 {
        rgb = BT2020_TO_BT709 * rgb;
    }
    if params.output > 0.5 {
        // scRGB keeps values outside 0..1: wide-gamut colors and highlights.
        return vec4<f32>(rgb * params.scale, c.a * params.opacity);
    }
    rgb = max(rgb, vec3<f32>(0.0));
    let white = params.tonemap_white;
    if white > 0.0 {
        // Extended Reinhard on luminance (docs/features/hdr.md#tonemap-to-sdr).
        let y = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        let scale = select(1.0, (1.0 + y / (white * white)) / (1.0 + y), y > 0.0);
        rgb = clamp(rgb * scale, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return vec4<f32>(rgb, c.a * params.opacity);
}
"#;

/// Bytes of the `Params` uniform (`opacity` at 16, `decode_srgb` at 20, `bt2020` at 24,
/// `tonemap_white` at 28, `output` at 32, `scale` at 36).
const PARAMS_SIZE: usize = 48;
/// Byte offset of `tonemap_white` in `Params`.
const TONEMAP_WHITE_OFFSET: u64 = 28;
/// Byte offset of `output` in `Params`, followed by `scale`.
const OUTPUT_OFFSET: u64 = 32;

/// How a [`VideoRenderer`] encodes tracks for its target (`docs/features/hdr.md#hdr-output`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Output {
    /// From the target format: HDR tracks tonemapped into SDR targets, linear BT.709 values
    /// (negatives clamped) into float targets.
    #[default]
    Auto,
    /// For `SurfaceColorSpace::Bt2100Pq` (HDR10): BT.2020, PQ-encoded. Reference white (1.0)
    /// is shown at `white_nits`, or at each track's reference white (203 for SDR tracks).
    Pq { white_nits: Option<f32> },
    /// For `SurfaceColorSpace::ExtendedSrgbLinear` (scRGB): linear BT.709, values outside
    /// `0..=1` kept. Reference white is drawn as `white_nits / unit_nits`, `white_nits` as
    /// for `Pq`; `unit_nits` is the nits of scRGB 1.0.
    ScRgb { white_nits: Option<f32>, unit_nits: f32 },
}

impl Output {
    /// The output for a surface configured with `color_space` on a display described by
    /// `info`; `white_nits` as for [`Output::Pq`]. scRGB 1.0 is 80 nits where the display
    /// reports absolute luminance (Windows), otherwise the system's SDR white, taken as 203.
    pub fn for_surface(color_space: wgpu::SurfaceColorSpace, info: &wgpu::DisplayHdrInfo, white_nits: Option<f32>) -> Output {
        match color_space {
            wgpu::SurfaceColorSpace::Bt2100Pq => Output::Pq { white_nits },
            wgpu::SurfaceColorSpace::ExtendedSrgbLinear => Output::ScRgb {
                white_nits,
                unit_nits: if info.luminance.is_some() { 80.0 } else { DEFAULT_WHITE_NITS },
            },
            _ => Output::Auto,
        }
    }

    /// The `output` and `scale` uniforms of a track whose reference white is `track_white`
    /// nits.
    fn uniforms(self, track_white: f32) -> [f32; 2] {
        match self {
            Output::Auto => [0.0, 0.0],
            Output::ScRgb { white_nits, unit_nits } => [1.0, white_nits.unwrap_or(track_white) / unit_nits],
            Output::Pq { white_nits } => [2.0, white_nits.unwrap_or(track_white) / 10000.0],
        }
    }
}

/// Reference white of SDR tracks on an HDR output, in nits (ITU-R BT.2408).
const DEFAULT_WHITE_NITS: f32 = praxi::format::color::DEFAULT_REFERENCE_WHITE as f32;

/// The HDR surface to configure, the first the surface offers: `Rgb10a2Unorm` with
/// `Bt2100Pq` (HDR10), then `Rgba16Float` with `ExtendedSrgbLinear` (scRGB). `None` when it
/// offers neither (`docs/features/hdr.md#hdr-output`).
pub fn hdr_surface(caps: &wgpu::SurfaceCapabilities) -> Option<(wgpu::TextureFormat, wgpu::SurfaceColorSpace)> {
    [
        (wgpu::TextureFormat::Rgb10a2Unorm, wgpu::SurfaceColorSpace::Bt2100Pq),
        (wgpu::TextureFormat::Rgba16Float, wgpu::SurfaceColorSpace::ExtendedSrgbLinear),
    ]
    .into_iter()
    .find(|&(format, space)| space.to_color_spaces().is_some_and(|s| caps.color_spaces(format).contains(s)))
}

/// Whether the display reports itself as SDR, so automatic choices skip HDR output
/// (`docs/features/hdr.md#hdr-output`). Unknown is not SDR.
pub fn is_sdr_display(info: &wgpu::DisplayHdrInfo) -> bool {
    info.coarse.and_then(|c| c.high_dynamic_range) == Some(false)
}

/// Texture formats the adapter can sample, most preferred first: SDR formats, then HDR ones,
/// so an SDR variant wins and HDR-only files still play (tonemapped when drawn).
pub fn supported_formats(features: wgpu::Features) -> Vec<TextureFormat> {
    let (bc, astc, astc_hdr) = families(features);
    let mut formats = Vec::new();
    if bc {
        formats.extend(BC);
    }
    if astc {
        formats.extend(ASTC);
    }
    if bc {
        formats.extend(BC_HDR);
    }
    if astc_hdr {
        formats.extend(ASTC_HDR);
    }
    formats
}

/// Like [`supported_formats`], but HDR formats first in each family
/// (`docs/features/hdr.md#variant-selection`).
pub fn supported_formats_hdr(features: wgpu::Features) -> Vec<TextureFormat> {
    let (bc, astc, astc_hdr) = families(features);
    let mut formats = Vec::new();
    if bc {
        formats.extend(BC_HDR.into_iter().chain(BC));
    }
    if astc_hdr {
        formats.extend(ASTC_HDR);
    }
    if astc {
        formats.extend(ASTC);
    }
    formats
}

/// Whether the features allow BC, ASTC and ASTC HDR textures.
fn families(features: wgpu::Features) -> (bool, bool, bool) {
    let astc = features.contains(wgpu::Features::TEXTURE_COMPRESSION_ASTC);
    (
        features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
        astc,
        astc && features.contains(wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR),
    )
}

/// The player capabilities for an adapter, restricted to `only` when given: the texture
/// formats it samples, then `motion` when it can run the motion decoder.
pub fn capabilities(adapter: &wgpu::Adapter, only: Option<TextureFormat>) -> Result<Capabilities, String> {
    capabilities_from(adapter, supported_formats(adapter.features()), only)
}

/// Like [`capabilities`], preferring HDR variants ([`supported_formats_hdr`]).
pub fn capabilities_hdr(adapter: &wgpu::Adapter, only: Option<TextureFormat>) -> Result<Capabilities, String> {
    capabilities_from(adapter, supported_formats_hdr(adapter.features()), only)
}

fn capabilities_from(
    adapter: &wgpu::Adapter,
    mut supported: Vec<TextureFormat>,
    only: Option<TextureFormat>,
) -> Result<Capabilities, String> {
    if supports_motion(adapter) {
        supported.push(TextureFormat::Motion);
    }
    match only {
        Some(TextureFormat::Motion) if !supported.contains(&TextureFormat::Motion) => {
            Err("the GPU cannot run the motion decoder's compute shaders".into())
        }
        Some(f) if !supported.contains(&f) => Err(format!("the GPU cannot sample {f}")),
        Some(f) => Ok(Capabilities::new([f])),
        None if supported.is_empty() => Err("the GPU supports neither BC nor ASTC textures nor compute shaders".into()),
        None => Ok(Capabilities::new(supported)),
    }
}

/// The compression features to request from `adapter`.
pub fn required_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features()
        & (wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::TEXTURE_COMPRESSION_ASTC
            | wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR)
}

pub fn wgpu_format(format: TextureFormat, srgb: bool) -> wgpu::TextureFormat {
    use wgpu::TextureFormat as W;
    let astc = |block| W::Astc {
        block,
        channel: if srgb { wgpu::AstcChannel::UnormSrgb } else { wgpu::AstcChannel::Unorm },
    };
    match format {
        TextureFormat::Bc1 if srgb => W::Bc1RgbaUnormSrgb,
        TextureFormat::Bc1 => W::Bc1RgbaUnorm,
        TextureFormat::Bc3 if srgb => W::Bc3RgbaUnormSrgb,
        TextureFormat::Bc3 => W::Bc3RgbaUnorm,
        TextureFormat::Bc4 => W::Bc4RUnorm,
        TextureFormat::Bc5 => W::Bc5RgUnorm,
        TextureFormat::Bc7 if srgb => W::Bc7RgbaUnormSrgb,
        TextureFormat::Bc7 => W::Bc7RgbaUnorm,
        TextureFormat::Astc4x4 => astc(wgpu::AstcBlock::B4x4),
        TextureFormat::Astc5x5 => astc(wgpu::AstcBlock::B5x5),
        TextureFormat::Astc6x6 => astc(wgpu::AstcBlock::B6x6),
        TextureFormat::Astc8x8 => astc(wgpu::AstcBlock::B8x8),
        TextureFormat::Bc6h => W::Bc6hRgbUfloat,
        TextureFormat::Astc4x4Hdr => W::Astc { block: wgpu::AstcBlock::B4x4, channel: wgpu::AstcChannel::Hdr },
        TextureFormat::Astc6x6Hdr => W::Astc { block: wgpu::AstcBlock::B6x6, channel: wgpu::AstcChannel::Hdr },
        // Decoded into RGBA holding sRGB-encoded bytes (storage textures cannot be sRGB).
        TextureFormat::Motion => W::Rgba8Unorm,
    }
}

/// Nits of a track's 1.0: its reference white when HDR, 203 otherwise.
fn track_white(p: &praxi::VideoParams) -> f32 {
    if p.hdr() { p.color.reference_white as f32 } else { DEFAULT_WHITE_NITS }
}

/// Whether a render target holds floats, so HDR tracks keep values above 1.0.
fn is_float(target: wgpu::TextureFormat) -> bool {
    use wgpu::TextureFormat as W;
    matches!(target, W::Rgba16Float | W::Rgba32Float | W::Rg11b10Ufloat)
}

/// A pixel rectangle of the render target.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// The largest rectangle with the aspect ratio of `width x height` centred in the target.
    pub fn fit(width: u32, height: u32, target_w: u32, target_h: u32) -> Rect {
        let scale = (target_w as f32 / width as f32).min(target_h as f32 / height as f32);
        let (w, h) = ((width as f32 * scale).round().max(1.0), (height as f32 * scale).round().max(1.0));
        Rect { x: ((target_w as f32 - w) / 2.0).floor(), y: ((target_h as f32 - h) / 2.0).floor(), w, h }
    }

    /// The pixel at a cue position (`x`, `y` in `0..=1` from the top left of the video),
    /// y down (`docs/features/cue-runtime.md#screen-positions`).
    pub fn point(&self, (x, y): (f32, f32)) -> (f32, f32) {
        (self.x + x * self.w, self.y + y * self.h)
    }

    /// The cue position at a pixel, or `None` outside the rectangle.
    pub fn position(&self, (px, py): (f32, f32)) -> Option<(f32, f32)> {
        let (x, y) = ((px - self.x) / self.w, (py - self.y) / self.h);
        ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)).then_some((x, y))
    }
}

struct TrackTexture {
    texture: wgpu::Texture,
    params: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    format: TextureFormat,
    hdr: bool,
    /// Nits of 1.0: the reference white of HDR tracks, 203 for SDR ones.
    white: f32,
    blocks: (u32, u32),
    /// `VideoFrame::index` of the uploaded frame.
    uploaded: Option<u64>,
    /// The GPU decoder of a `motion` track.
    motion: Option<motion::Decoder>,
}

/// GPU time of one frame reconstructed by the motion decoder
/// (`docs/features/motion-codec.md#gpu-timing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionGpuTime {
    pub track: usize,
    pub keyframe: bool,
    pub predict: Duration,
    pub residual: Duration,
    /// The color pass, for the frame that was converted to RGBA.
    pub color: Option<Duration>,
}

/// The textures of one player's video tracks and the pipelines that draw them.
pub struct VideoRenderer {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    blend: wgpu::RenderPipeline,
    replace: wgpu::RenderPipeline,
    tracks: Vec<TrackTexture>,
    opacity: f32,
    /// Whether the target holds floats, so HDR tracks are drawn linear, not tonemapped.
    float_target: bool,
    tonemap_white: f32,
    output: Output,
    /// The motion decoder's pipelines, when a track needs them.
    motion: Option<motion::Pipelines>,
}

impl VideoRenderer {
    /// Creates pipelines drawing into `target` and one texture per video track of `player`.
    pub fn new<R: Read + Seek>(device: &wgpu::Device, target: wgpu::TextureFormat, player: &Player<R>) -> VideoRenderer {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("praxi"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("track"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("track"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let pipeline = |label, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_video"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("track"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mut renderer = VideoRenderer {
            blend: pipeline("blend", Some(blend)),
            replace: pipeline("replace", None),
            layout,
            sampler,
            tracks: Vec::new(),
            opacity: 1.0,
            float_target: is_float(target),
            tonemap_white: praxi::format::color::DEFAULT_TONEMAP_WHITE,
            output: Output::Auto,
            motion: None,
        };
        if player.tracks().iter().any(|t| t.params.format == TextureFormat::Motion) {
            renderer.motion = Some(motion::Pipelines::new(device));
        }
        for info in player.tracks() {
            let p = &info.params;
            let mut track = renderer.track_texture(device, p);
            if let Some(pipelines) = &renderer.motion
                && p.format == TextureFormat::Motion
            {
                let layout = praxi::format::motion::Layout::new(p);
                track.motion = Some(motion::Decoder::new(device, pipelines, layout, &track.texture));
            }
            renderer.tracks.push(track);
        }
        renderer
    }

    fn track_texture(&self, device: &wgpu::Device, p: &praxi::VideoParams) -> TrackTexture {
        let (format, srgb, width, height) = (p.format, p.srgb(), p.width, p.height);
        let (bw, bh) = format.block_size();
        let blocks = (width.div_ceil(bw), height.div_ceil(bh));
        let (padded_w, padded_h) = (blocks.0 * bw, blocks.1 * bh);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("track"),
            size: wgpu::Extent3d { width: padded_w, height: padded_h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format(format, srgb),
            usage: if format == TextureFormat::Motion {
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC
            } else {
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST
            },
            view_formats: &[],
        });
        let white = track_white(p);
        let [output, scale] = self.output.uniforms(white);
        let values: [f32; 10] = [
            width as f32 / padded_w as f32,
            height as f32 / padded_h as f32,
            (width as f32 - 0.5) / padded_w as f32,
            (height as f32 - 0.5) / padded_h as f32,
            self.opacity,
            (format == TextureFormat::Motion && srgb) as u32 as f32,
            (p.hdr() && p.color.primaries == praxi::Primaries::Bt2020) as u32 as f32,
            self.track_tonemap_white(p),
            output,
            scale,
        ];
        let mut bytes = [0u8; PARAMS_SIZE];
        for (dst, v) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(values) {
            *dst = v.to_le_bytes();
        }
        let params = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("track"),
                contents: &bytes,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            },
        );
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("track"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
            ],
        });
        TrackTexture { texture, params, bind_group, format, hdr: p.hdr(), white, blocks, uploaded: None, motion: None }
    }

    /// The `tonemap_white` uniform of a track: the white for HDR tracks drawn into an SDR
    /// target, 0 (linear) otherwise.
    fn track_tonemap_white(&self, p: &praxi::VideoParams) -> f32 {
        if p.hdr() && !self.float_target { self.tonemap_white } else { 0.0 }
    }

    /// Sets the linear value that HDR tracks drawn into an SDR target map to white (default
    /// 4.0, `docs/features/hdr.md#tonemap-to-sdr`). Values that are not positive are ignored.
    pub fn set_tonemap_white(&mut self, queue: &wgpu::Queue, white: f32) {
        if !(white.is_finite() && white > 0.0) {
            return;
        }
        self.tonemap_white = white;
        if self.float_target {
            return;
        }
        for t in self.tracks.iter().filter(|t| t.hdr) {
            queue.write_buffer(&t.params, TONEMAP_WHITE_OFFSET, &white.to_le_bytes());
        }
    }

    /// Sets how tracks are encoded for the target (`docs/features/hdr.md#hdr-output`): the
    /// target must be the surface format of the color space the output is for.
    pub fn set_output(&mut self, queue: &wgpu::Queue, output: Output) {
        self.output = output;
        for t in &self.tracks {
            let bytes: Vec<u8> = output.uniforms(t.white).iter().flat_map(|v| v.to_le_bytes()).collect();
            queue.write_buffer(&t.params, OUTPUT_OFFSET, &bytes);
        }
    }

    /// Starts or stops measuring the GPU time of the motion decoder's passes with timestamp
    /// queries, for profiling. Returns whether timing is on: never when `device` was created
    /// without `Features::TIMESTAMP_QUERY`.
    pub fn set_gpu_timing(&mut self, device: &wgpu::Device, enabled: bool) -> bool {
        let enabled = enabled && device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        for decoder in self.tracks.iter_mut().filter_map(|t| t.motion.as_mut()) {
            decoder.set_timing(device, enabled);
        }
        enabled
    }

    /// The GPU times of the motion frames reconstructed since the last call, in order (up to
    /// 64 per track). Waits for the GPU to finish them.
    pub fn gpu_times(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<MotionGpuTime> {
        let mut times = Vec::new();
        for (track, t) in self.tracks.iter_mut().enumerate() {
            let Some(decoder) = &mut t.motion else { continue };
            times.extend(decoder.read_times(device, queue).into_iter().map(|f| MotionGpuTime {
                track,
                keyframe: f.keyframe,
                predict: f.predict,
                residual: f.residual,
                color: f.color,
            }));
        }
        times
    }

    /// Uploads the frames that changed since the last call, reconstructs the coded frames
    /// queued by `motion` tracks, and sets the opacity (clamped to 0..=1) of the next draws.
    /// Returns how many tracks got a new frame.
    pub fn prepare<R: Read + Seek>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        player: &mut Player<R>,
        opacity: f32,
    ) -> usize {
        let opacity = if opacity.is_nan() { 1.0 } else { opacity.clamp(0.0, 1.0) };
        if opacity != self.opacity {
            self.opacity = opacity;
            for t in &self.tracks {
                queue.write_buffer(&t.params, 16, &opacity.to_le_bytes());
            }
        }
        let mut uploaded = 0;
        for (i, t) in self.tracks.iter_mut().enumerate() {
            if let (Some(decoder), Some(pipelines)) = (&mut t.motion, &self.motion) {
                let frames = player.take_coded_frames(i);
                for (k, frame) in frames.iter().enumerate() {
                    decoder.decode(device, queue, pipelines, frame, k + 1 == frames.len());
                }
                if !frames.is_empty() {
                    t.uploaded = player.video_frame(i).map(|f| f.index);
                    uploaded += 1;
                }
                continue;
            }
            let Some(frame) = player.video_frame(i) else { continue };
            if t.uploaded == Some(frame.index) {
                continue;
            }
            let (bw, bh) = t.format.block_size();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &t.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                frame.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(t.blocks.0 * t.format.block_bytes()),
                    rows_per_image: Some(t.blocks.1),
                },
                wgpu::Extent3d { width: t.blocks.0 * bw, height: t.blocks.1 * bh, depth_or_array_layers: 1 },
            );
            t.uploaded = Some(frame.index);
            uploaded += 1;
        }
        uploaded
    }

    /// Draws every track that has a frame into `rect`, in track order, each blended over
    /// what is already there (straight alpha times the opacity). Sets the viewport to `rect`.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, rect: Rect) {
        self.draw_tracks(pass, rect, false);
    }

    /// Like [`VideoRenderer::draw`], but track 0 replaces the target pixels, alpha included
    /// (for offscreen rendering such as screenshots).
    pub fn draw_replace(&self, pass: &mut wgpu::RenderPass<'_>, rect: Rect) {
        self.draw_tracks(pass, rect, true);
    }

    fn draw_tracks(&self, pass: &mut wgpu::RenderPass<'_>, rect: Rect, replace_first: bool) {
        pass.set_viewport(rect.x, rect.y, rect.w, rect.h, 0.0, 1.0);
        for (i, t) in self.tracks.iter().enumerate().filter(|(_, t)| t.uploaded.is_some()) {
            pass.set_pipeline(if replace_first && i == 0 { &self.replace } else { &self.blend });
            pass.set_bind_group(0, &t.bind_group, &[]);
            pass.draw(0..4, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wgpu_formats_match_block_layout() {
        for format in TextureFormat::ALL {
            for srgb in [false, true] {
                let w = wgpu_format(format, srgb);
                assert_eq!(w.block_dimensions(), format.block_size(), "{format}");
                assert_eq!(w.block_copy_size(None), Some(format.block_bytes()), "{format}");
                let has_srgb = !matches!(format, TextureFormat::Bc4 | TextureFormat::Bc5) && !format.is_hdr();
                assert_eq!(w.is_srgb(), srgb && has_srgb, "{format}");
            }
        }
    }

    #[test]
    fn prefers_bc_then_astc() {
        assert!(supported_formats(wgpu::Features::empty()).is_empty());
        let both = supported_formats(
            wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::TEXTURE_COMPRESSION_ASTC,
        );
        assert_eq!(both[0], TextureFormat::Bc7);
        assert_eq!(both.len(), 10, "BC6H last, no ASTC HDR without its feature");
        assert_eq!(both[9], TextureFormat::Bc6h);
        assert_eq!(supported_formats(wgpu::Features::TEXTURE_COMPRESSION_ASTC), ASTC);
        let all = wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::TEXTURE_COMPRESSION_ASTC
            | wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR;
        assert_eq!(supported_formats(all).len(), 12);
        let hdr = supported_formats_hdr(all);
        assert_eq!(&hdr[..2], [TextureFormat::Bc6h, TextureFormat::Bc7]);
        assert_eq!(&hdr[6..9], [TextureFormat::Astc4x4Hdr, TextureFormat::Astc6x6Hdr, TextureFormat::Astc4x4]);
    }

    /// A device on the adapter `wgpu` picks (`WGPU_ADAPTER_NAME` selects one), if it can
    /// run the motion decoder. It starts from WebGL 2 limits, so the tests check that
    /// `required_limits` raises everything the decoder needs.
    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(&instance, None)).ok()?;
        if !supports_motion(&adapter) {
            return None;
        }
        eprintln!("adapter: {}", adapter.get_info().name);
        let required_limits = required_limits(&adapter, wgpu::Limits::downlevel_webgl2_defaults());
        let required_features = adapter.features() & wgpu::Features::TIMESTAMP_QUERY;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features,
            required_limits,
            ..Default::default()
        }))
        .ok()
    }

    /// Skips a test that cannot run here, or fails it when `PRAXI_TEST_STRICT` is set
    /// (`docs/features/testing.md`).
    fn skip(why: &str) {
        assert!(std::env::var_os("PRAXI_TEST_STRICT").is_none(), "PRAXI_TEST_STRICT: {why}");
        eprintln!("skipped: {why}");
    }

    /// A device with the block compression features of the adapter `wgpu` picks.
    fn block_gpu() -> Option<(wgpu::Device, wgpu::Queue, wgpu::Features)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(&instance, None)).ok()?;
        let required_features = required_features(&adapter);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features,
            ..Default::default()
        }))
        .ok()?;
        Some((device, queue, required_features))
    }

    #[test]
    fn block_formats_draw_like_the_cpu_decoder() {
        use praxi_encoder::{Encoder, EncoderConfig, VariantConfig};
        use std::io::Cursor;

        let Some((device, queue, features)) = block_gpu() else {
            return skip("no adapter");
        };
        let formats = supported_formats(features);
        // Sizes that are not multiples of the blocks, so padding is cropped.
        let (w, h) = (50, 34);
        let mut drawn = Vec::new();
        for (format, variant) in [
            (TextureFormat::Bc7, VariantConfig::bc7()),
            (TextureFormat::Bc1, VariantConfig::bc1()),
            (TextureFormat::Astc4x4, VariantConfig::astc(4)),
            (TextureFormat::Astc6x6, VariantConfig::astc(6)),
        ] {
            if !formats.contains(&format) {
                continue;
            }
            let config = EncoderConfig::new(w, h, 10, 1).variant(variant).alpha(true).threads(2);
            let mut enc = Encoder::new(Cursor::new(Vec::new()), config).unwrap();
            for t in 0..3 {
                enc.push_frame(&clip_frame(w, h, t)).unwrap();
            }
            let bytes = enc.finish().unwrap().0.into_inner();
            let mut player = Player::open(Cursor::new(bytes), &Capabilities::new([format])).unwrap();
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = target.create_view(&Default::default());
            let mut renderer = VideoRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb, &player);
            player.seek(std::time::Duration::from_millis(200)).unwrap();
            assert_eq!(renderer.prepare(&device, &queue, &mut player, 1.0), 1, "{format}");
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    })],
                    ..Default::default()
                });
                renderer.draw_replace(&mut pass, Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 });
            }
            queue.submit([encoder.finish()]);
            let gpu = read_texture(&device, &queue, &target, w, h);
            let frame = player.video_frame(0).unwrap();
            let cpu = praxi_encoder::decode_frame(&player.tracks()[0].params, frame.data).unwrap();
            let worst = gpu.iter().zip(&cpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            // The sampler decodes sRGB to linear and the target encodes it back. BC1 hardware rounds its
            // interpolated palette colors its own way (D3D allows it, unlike BC7 and ASTC): a few steps more.
            let limit = if format == TextureFormat::Bc1 { 4 } else { 1 };
            assert!(worst <= limit, "{format}: a channel differs by {worst}");
            drawn.push(format);
        }
        eprintln!("drawn: {drawn:?}");
        if drawn.is_empty() {
            skip("the adapter samples none of BC7, BC1, ASTC 4x4, ASTC 6x6");
        }
    }

    /// Draws track 0 of `player` at 200 ms into a new `target` texture and reads it back.
    fn draw_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        player: &mut Player<std::io::Cursor<Vec<u8>>>,
        target: wgpu::TextureFormat,
        tonemap_white: f32,
        output: Output,
    ) -> Vec<u8> {
        let (w, h) = (player.tracks()[0].params.width, player.tracks()[0].params.height);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: target,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mut renderer = VideoRenderer::new(device, target, player);
        renderer.set_tonemap_white(queue, tonemap_white);
        renderer.set_output(queue, output);
        player.seek(std::time::Duration::from_millis(200)).unwrap();
        renderer.prepare(device, queue, player, 1.0);
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            renderer.draw_replace(&mut pass, Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 });
        }
        queue.submit([encoder.finish()]);
        read_texture(device, queue, &texture, w, h)
    }

    #[test]
    fn hdr_formats_draw_like_the_cpu_decoder() {
        use praxi::format::color::tonemap_srgb8;
        use praxi_encoder::{ColorConfig, Encoder, EncoderConfig, VariantConfig};
        use std::io::Cursor;

        let Some((device, queue, features)) = block_gpu() else {
            return skip("no adapter");
        };
        let formats = supported_formats(features);
        let (w, h) = (50, 34);
        let mut drawn = Vec::new();
        for (format, variant, primaries) in [
            (TextureFormat::Bc6h, VariantConfig::bc6h(), praxi::Primaries::Bt709),
            (TextureFormat::Bc6h, VariantConfig::bc6h(), praxi::Primaries::Bt2020),
            (TextureFormat::Astc4x4Hdr, VariantConfig::astc_hdr(4), praxi::Primaries::Bt709),
            (TextureFormat::Astc6x6Hdr, VariantConfig::astc_hdr(6), praxi::Primaries::Bt709),
        ] {
            if !formats.contains(&format) {
                continue;
            }
            let color = ColorConfig::hdr().primaries(primaries);
            let config = EncoderConfig::new(w, h, 10, 1).color(color).variant(variant).threads(2);
            let mut enc = Encoder::new(Cursor::new(Vec::new()), config).unwrap();
            for t in 0..3 {
                // The SDR clip scaled to 0..12, with a colored highlight.
                let frame: Vec<f32> = clip_frame(w, h, t)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0] as f32 / 20.0, p[1] as f32 / 40.0, p[2] as f32 / 255.0 * 0.3, 1.0])
                    .collect();
                enc.push_frame_hdr(&frame).unwrap();
            }
            let bytes = enc.finish().unwrap().0.into_inner();
            let mut player = Player::open(Cursor::new(bytes), &Capabilities::new([format])).unwrap();
            player.seek(std::time::Duration::from_millis(200)).unwrap();
            let params = player.tracks()[0].params;
            let cpu = praxi_encoder::decode_frame_hdr(&params, player.video_frame(0).unwrap().data).unwrap();
            let cpu_709: Vec<[f32; 3]> = cpu
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| praxi::format::color::convert_primaries([p[0], p[1], p[2]], primaries, praxi::Primaries::Bt709).map(|v| v.max(0.0)))
                .collect();

            // Linear values in a float target, within half-float rounding (and the matrix).
            let gpu = draw_frame(&device, &queue, &mut player, wgpu::TextureFormat::Rgba16Float, 4.0, Output::Auto);
            let gpu: Vec<f32> = gpu.as_chunks::<2>().0.iter().map(|b| imgio::half::to_f32(u16::from_le_bytes(*b))).collect();
            for (g, c) in gpu.as_chunks::<4>().0.iter().zip(&cpu_709) {
                for k in 0..3 {
                    assert!((g[k] - c[k]).abs() <= 0.002 + c[k] * 0.002, "{format} {primaries}: GPU {g:?}, CPU {c:?}");
                }
            }
            // The tonemap in an sRGB target.
            let gpu = draw_frame(&device, &queue, &mut player, wgpu::TextureFormat::Rgba8UnormSrgb, 2.0, Output::Auto);
            let worst = gpu
                .as_chunks::<4>()
                .0
                .iter()
                .zip(cpu.as_chunks::<4>().0)
                .flat_map(|(g, c)| {
                    let t = tonemap_srgb8([c[0], c[1], c[2]], primaries, 2.0);
                    (0..3).map(move |k| g[k].abs_diff(t[k]))
                })
                .max()
                .unwrap();
            assert!(worst <= 1, "{format} {primaries}: tonemapped channel differs by {worst}");

            // HDR10 output: BT.2020 PQ codes, reference white at 203 nits, within 1 code.
            use praxi::format::color::{convert_primaries, nits_to_pq};
            let gpu = draw_frame(&device, &queue, &mut player, wgpu::TextureFormat::Rgb10a2Unorm, 2.0, Output::Pq { white_nits: None });
            let worst = gpu
                .as_chunks::<4>()
                .0
                .iter()
                .zip(cpu.as_chunks::<4>().0)
                .flat_map(|(g, c)| {
                    let texel = u32::from_le_bytes(*g);
                    let rgb = convert_primaries([c[0], c[1], c[2]], primaries, praxi::Primaries::Bt2020);
                    (0..3).map(move |k| {
                        let want = (nits_to_pq(rgb[k].max(0.0) * 203.0) * 1023.0).round() as i32;
                        ((texel >> (10 * k)) & 1023) as i32 - want
                    })
                })
                .map(i32::abs)
                .max()
                .unwrap();
            assert!(worst <= 1, "{format} {primaries}: PQ code differs by {worst}");
            // scRGB output: linear BT.709 with negatives kept, reference white at 160 / 80.
            let output = Output::ScRgb { white_nits: Some(160.0), unit_nits: 80.0 };
            let gpu = draw_frame(&device, &queue, &mut player, wgpu::TextureFormat::Rgba16Float, 2.0, output);
            let gpu: Vec<f32> = gpu.as_chunks::<2>().0.iter().map(|b| imgio::half::to_f32(u16::from_le_bytes(*b))).collect();
            for (g, c) in gpu.as_chunks::<4>().0.iter().zip(cpu.as_chunks::<4>().0) {
                let want = convert_primaries([c[0], c[1], c[2]], primaries, praxi::Primaries::Bt709).map(|v| v * 2.0);
                for k in 0..3 {
                    assert!((g[k] - want[k]).abs() <= 0.004 + want[k].abs() * 0.002, "{format} {primaries} scRGB: GPU {g:?}, CPU {want:?}");
                }
            }
            drawn.push(format);
        }
        eprintln!("drawn: {drawn:?}");
        if drawn.is_empty() {
            skip("the adapter samples no HDR format");
        }
    }

    #[test]
    fn hdr_surfaces_and_outputs() {
        use wgpu::{SurfaceColorSpace as Space, SurfaceColorSpaces as Spaces, TextureFormat as F};
        let caps = |list: &[(F, Spaces)]| wgpu::SurfaceCapabilities {
            format_capabilities: list
                .iter()
                .map(|&(format, color_spaces)| wgpu::SurfaceFormatCapabilities { format, color_spaces })
                .collect(),
            ..Default::default()
        };
        let sdr = caps(&[(F::Bgra8UnormSrgb, Spaces::SRGB), (F::Rgba16Float, Spaces::SRGB)]);
        assert_eq!(hdr_surface(&sdr), None);
        let scrgb = caps(&[(F::Bgra8UnormSrgb, Spaces::SRGB), (F::Rgba16Float, Spaces::EXTENDED_SRGB_LINEAR)]);
        assert_eq!(hdr_surface(&scrgb), Some((F::Rgba16Float, Space::ExtendedSrgbLinear)));
        let both = caps(&[(F::Rgba16Float, Spaces::EXTENDED_SRGB_LINEAR), (F::Rgb10a2Unorm, Spaces::SRGB | Spaces::BT2100_PQ)]);
        assert_eq!(hdr_surface(&both), Some((F::Rgb10a2Unorm, Space::Bt2100Pq)));

        let unknown = wgpu::DisplayHdrInfo::default();
        assert!(!is_sdr_display(&unknown));
        let windows = wgpu::DisplayHdrInfo { luminance: Some(Default::default()), ..Default::default() };
        assert_eq!(Output::for_surface(Space::ExtendedSrgbLinear, &windows, None), Output::ScRgb { white_nits: None, unit_nits: 80.0 });
        assert_eq!(Output::for_surface(Space::ExtendedSrgbLinear, &unknown, None), Output::ScRgb { white_nits: None, unit_nits: 203.0 });
        assert_eq!(Output::for_surface(Space::Bt2100Pq, &unknown, Some(300.0)), Output::Pq { white_nits: Some(300.0) });
        assert_eq!(Output::for_surface(Space::Srgb, &unknown, None), Output::Auto);
        assert_eq!(Output::Pq { white_nits: None }.uniforms(203.0), [2.0, 0.0203]);
        assert_eq!(Output::ScRgb { white_nits: Some(160.0), unit_nits: 80.0 }.uniforms(203.0), [1.0, 2.0]);
    }

    fn read_buffer(device: &wgpu::Device, queue: &wgpu::Queue, buffer: &wgpu::Buffer) -> Vec<u8> {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: buffer.size(),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, buffer.size());
        queue.submit([encoder.finish()]);
        map(device, &staging)
    }

    /// The top-left `width x height` pixels of an RGBA texture.
    fn read_texture(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture, width: u32, height: u32) -> Vec<u8> {
        let texel = texture.format().block_copy_size(None).expect("uncompressed");
        let row = (width * texel).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        queue.submit([encoder.finish()]);
        map(device, &staging).chunks(row as usize).flat_map(|r| r[..(width * texel) as usize].to_vec()).collect()
    }

    fn map(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
        buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        buffer.slice(..).get_mapped_range().expect("mapped").to_vec()
    }

    /// Frame `t` of a clip with a scrolling background, a square moving down over it and a
    /// still corner, so that delta frames hold inter, split and skipped macroblocks.
    fn clip_frame(w: u32, h: u32, t: u32) -> Vec<u8> {
        let texture = |x: u32, y: u32| ((x * 7 + y * 3) % 64 * 3 + (x / 8 + y / 8) % 2 * 40) as u8;
        let (top, left) = (4 + 2 * t, w / 4);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if x >= w * 2 / 3 && y >= h / 2 {
                    texture(x, y)
                } else if (left..left + 20).contains(&x) && (top..top + 18).contains(&y) {
                    texture(x * 3, (y - top) * 5) / 2 + 100
                } else {
                    texture(x + 3 * t, y)
                };
                rgba.extend([v / 2 + x as u8, v / 2 + 40, v / 2 + (y * 3) as u8, (y * 6) as u8]);
            }
        }
        rgba
    }

    #[test]
    fn motion_decoder_matches_reference() {
        use praxi::format::motion::{self, Layout};
        use praxi_encoder::{Chroma, Encoder, EncoderConfig, VariantConfig};
        use std::io::Cursor;
        use std::time::Duration;

        let Some((device, queue)) = gpu() else {
            return skip("no adapter that runs compute shaders");
        };
        // The 4:4:4 clip runs timed, in separate passes per stage.
        for (chroma, w, h, alpha, timed) in [(Chroma::C420, 72, 40, true, false), (Chroma::C444, 50, 34, false, true)] {
            let config = EncoderConfig::new(w, h, 10, 1)
                .variant(VariantConfig::motion().qp(12).slices(2).chroma(chroma))
                .keyint(4)
                .alpha(alpha)
                .threads(2);
            let mut enc = Encoder::new(Cursor::new(Vec::new()), config).unwrap();
            for t in 0..10 {
                enc.push_frame(&clip_frame(w, h, t)).unwrap();
            }
            let (out, stats) = enc.finish().unwrap();
            let m = stats.variants[0].motion.unwrap();
            assert!(m.inter > 0 && m.split > 0 && m.skip > 0, "{chroma:?}: {m:?}");
            let bytes = out.into_inner();

            // The CPU reference of every frame.
            let mut reader = praxi::format::Reader::new(Cursor::new(bytes.clone())).unwrap();
            let params = *reader.streams()[0].video_params().unwrap();
            let layout = Layout::new(&params);
            let mut decoder = motion::Decoder::new(&params);
            let (mut payload, mut coded) = (Vec::new(), motion::CodedFrame::default());
            let (mut expected, mut keyframes): (Vec<Vec<u8>>, Vec<bool>) = (Vec::new(), Vec::new());
            while let Some(chunk) = reader.next_chunk().unwrap() {
                keyframes.push(chunk.keyframe());
                reader.read_payload(&chunk, &mut payload).unwrap();
                decoder.decode(chunk.keyframe(), &payload, &mut coded).unwrap();
                let mut planes = vec![0; layout.size];
                motion::reconstruct(&layout, &coded, expected.last().map_or(&[][..], |p| p), &mut planes);
                expected.push(planes);
            }

            let mut player = Player::open(Cursor::new(bytes), &Capabilities::new([TextureFormat::Motion])).unwrap();
            let mut renderer = VideoRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb, &player);
            let timed = timed && renderer.set_gpu_timing(&device, true);
            if !timed && chroma == Chroma::C444 {
                eprintln!("timing not checked: the adapter has no timestamp queries");
            }
            let mut check = |player: &mut Player<Cursor<Vec<u8>>>, what: &str| {
                assert_eq!(renderer.prepare(&device, &queue, player, 1.0), 1, "{chroma:?} {what}");
                let times = renderer.gpu_times(&device, &queue);
                if timed {
                    // One entry per reconstructed frame; only the last one is converted.
                    let (last, rest) = times.split_last().expect("timed frames");
                    assert!(last.color.is_some() && rest.iter().all(|t| t.color.is_none()), "{what}: {times:?}");
                    assert!(times.iter().all(|t| t.track == 0));
                    let last = player.video_frame(0).unwrap().time.as_millis() as usize / 100;
                    let flags: Vec<bool> = times.iter().map(|t| t.keyframe).collect();
                    assert_eq!(flags, keyframes[last + 1 - times.len()..=last], "{what}");
                } else {
                    assert!(times.is_empty());
                }
                let t = (player.video_frame(0).unwrap().time.as_millis() / 100) as usize;
                let track = &renderer.tracks[0];
                let planes = read_buffer(&device, &queue, track.motion.as_ref().unwrap().planes());
                if let Some(i) = planes.iter().zip(&expected[t]).position(|(a, b)| a != b) {
                    let p = layout.planes.iter().rposition(|p| p.offset <= i).unwrap();
                    let at = i - layout.planes[p].offset;
                    let width = layout.planes[p].width as usize;
                    panic!(
                        "{chroma:?} {what}, frame {t}: plane {p} pixel ({}, {}) is {}, expected {}",
                        at % width,
                        at / width,
                        planes[i],
                        expected[t][i]
                    );
                }
                let mut rgba = Vec::new();
                motion::to_rgba(&layout, &expected[t], &mut rgba);
                assert!(read_texture(&device, &queue, &track.texture, w, h) == rgba, "{chroma:?} {what}, frame {t}: RGBA");
                t
            };
            player.play().unwrap();
            assert_eq!(check(&mut player, "first frame"), 0);
            // One frame at a time, then several queued frames reconstructed at once.
            let mut shown = 0;
            for frames in [1, 1, 2, 3, 1] {
                player.advance(Duration::from_millis(100 * frames)).unwrap();
                shown += frames as usize;
                assert_eq!(check(&mut player, &format!("advance to {shown}")), shown);
            }
            // Seeking into a group of pictures queues its keyframe and the frames after it.
            player.seek(Duration::from_millis(600)).unwrap();
            assert_eq!(check(&mut player, "seek"), 6);
        }
    }

    #[test]
    fn fit_letterboxes() {
        let r = Rect::fit(1920, 1080, 640, 480);
        assert_eq!((r.x, r.y, r.w, r.h), (0.0, 60.0, 640.0, 360.0));
        let r = Rect::fit(1920, 1080, 1000, 360);
        assert_eq!((r.x, r.y, r.w, r.h), (180.0, 0.0, 640.0, 360.0));
    }

    #[test]
    fn rects_map_cue_positions() {
        let r = Rect::fit(1920, 1080, 640, 480);
        assert_eq!(r.point((0.0, 0.0)), (0.0, 60.0));
        assert_eq!(r.point((0.25, 1.0)), (160.0, 420.0));
        assert_eq!(r.position((160.0, 420.0)), Some((0.25, 1.0)));
        assert_eq!(r.position((320.0, 240.0)), Some((0.5, 0.5)));
        assert_eq!(r.position((320.0, 30.0)), None, "in the letterbox bar");
        assert_eq!(r.position((641.0, 240.0)), None);
    }
}
