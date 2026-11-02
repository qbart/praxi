//! Texture upload and drawing of praxi video for `wgpu` engines
//! (`docs/features/engine-integration.md#praxi-wgpu`).

use praxi::{Capabilities, Player, TextureFormat};
use std::io::{Read, Seek};

/// BC formats in order of preference, then ASTC.
const BC: [TextureFormat; 5] = [
    TextureFormat::Bc7,
    TextureFormat::Bc3,
    TextureFormat::Bc1,
    TextureFormat::Bc5,
    TextureFormat::Bc4,
];
const ASTC: [TextureFormat; 4] = [
    TextureFormat::Astc4x4,
    TextureFormat::Astc5x5,
    TextureFormat::Astc6x6,
    TextureFormat::Astc8x8,
];

const SHADER: &str = r#"
struct Params {
    uv_scale: vec2<f32>,
    uv_max: vec2<f32>,
    opacity: f32,
};

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
    return vec4<f32>(c.rgb, c.a * params.opacity);
}
"#;

/// Bytes of the `Params` uniform (`opacity` at 16, padded to 32).
const PARAMS_SIZE: usize = 32;

/// Texture formats the adapter can sample, most preferred first.
pub fn supported_formats(features: wgpu::Features) -> Vec<TextureFormat> {
    let mut formats = Vec::new();
    if features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC) {
        formats.extend(BC);
    }
    if features.contains(wgpu::Features::TEXTURE_COMPRESSION_ASTC) {
        formats.extend(ASTC);
    }
    formats
}

/// The player capabilities for an adapter, restricted to `only` when given.
pub fn capabilities(
    adapter: &wgpu::Adapter,
    only: Option<TextureFormat>,
) -> Result<Capabilities, String> {
    let supported = supported_formats(adapter.features());
    match only {
        Some(f) if !supported.contains(&f) => Err(format!("the GPU cannot sample {f}")),
        Some(f) => Ok(Capabilities::new([f])),
        None if supported.is_empty() => Err("the GPU supports neither BC nor ASTC textures".into()),
        None => Ok(Capabilities::new(supported)),
    }
}

/// The compression features to request from `adapter`.
pub fn required_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features()
        & (wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::TEXTURE_COMPRESSION_ASTC)
}

pub fn wgpu_format(format: TextureFormat, srgb: bool) -> wgpu::TextureFormat {
    use wgpu::TextureFormat as W;
    let astc = |block| W::Astc {
        block,
        channel: if srgb {
            wgpu::AstcChannel::UnormSrgb
        } else {
            wgpu::AstcChannel::Unorm
        },
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
        // Decoded into RGBA holding sRGB-encoded bytes (storage textures cannot be sRGB).
        TextureFormat::Motion => W::Rgba8Unorm,
    }
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
        let (w, h) = (
            (width as f32 * scale).round().max(1.0),
            (height as f32 * scale).round().max(1.0),
        );
        Rect {
            x: ((target_w as f32 - w) / 2.0).floor(),
            y: ((target_h as f32 - h) / 2.0).floor(),
            w,
            h,
        }
    }
}

struct TrackTexture {
    texture: wgpu::Texture,
    params: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    format: TextureFormat,
    blocks: (u32, u32),
    /// `VideoFrame::index` of the uploaded frame.
    uploaded: Option<u64>,
}

/// The textures of one player's video tracks and the pipelines that draw them.
pub struct VideoRenderer {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    blend: wgpu::RenderPipeline,
    replace: wgpu::RenderPipeline,
    tracks: Vec<TrackTexture>,
    opacity: f32,
}

impl VideoRenderer {
    /// Creates pipelines drawing into `target` and one texture per video track of `player`.
    pub fn new<R: Read + Seek>(
        device: &wgpu::Device,
        target: wgpu::TextureFormat,
        player: &Player<R>,
    ) -> VideoRenderer {
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
        };
        for info in player.tracks() {
            let p = &info.params;
            renderer.tracks.push(renderer.track_texture(
                device,
                p.format,
                p.srgb(),
                p.width,
                p.height,
            ));
        }
        renderer
    }

    fn track_texture(
        &self,
        device: &wgpu::Device,
        format: TextureFormat,
        srgb: bool,
        width: u32,
        height: u32,
    ) -> TrackTexture {
        let (bw, bh) = format.block_size();
        let blocks = (width.div_ceil(bw), height.div_ceil(bh));
        let (padded_w, padded_h) = (blocks.0 * bw, blocks.1 * bh);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("track"),
            size: wgpu::Extent3d {
                width: padded_w,
                height: padded_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format(format, srgb),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let values: [f32; 5] = [
            width as f32 / padded_w as f32,
            height as f32 / padded_h as f32,
            (width as f32 - 0.5) / padded_w as f32,
            (height as f32 - 0.5) / padded_h as f32,
            self.opacity,
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
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params.as_entire_binding(),
                },
            ],
        });
        TrackTexture {
            texture,
            params,
            bind_group,
            format,
            blocks,
            uploaded: None,
        }
    }

    /// Uploads the frames that changed since the last call and sets the opacity (clamped to
    /// 0..=1) of the next draws. Returns how many frames were uploaded.
    pub fn prepare<R: Read + Seek>(
        &mut self,
        queue: &wgpu::Queue,
        player: &Player<R>,
        opacity: f32,
    ) -> usize {
        let opacity = if opacity.is_nan() {
            1.0
        } else {
            opacity.clamp(0.0, 1.0)
        };
        if opacity != self.opacity {
            self.opacity = opacity;
            for t in &self.tracks {
                queue.write_buffer(&t.params, 16, &opacity.to_le_bytes());
            }
        }
        let mut uploaded = 0;
        for (i, t) in self.tracks.iter_mut().enumerate() {
            let Some(frame) = player.video_frame(i) else {
                continue;
            };
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
                wgpu::Extent3d {
                    width: t.blocks.0 * bw,
                    height: t.blocks.1 * bh,
                    depth_or_array_layers: 1,
                },
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
        for (i, t) in self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.uploaded.is_some())
        {
            pass.set_pipeline(if replace_first && i == 0 {
                &self.replace
            } else {
                &self.blend
            });
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
                assert_eq!(
                    w.block_copy_size(None),
                    Some(format.block_bytes()),
                    "{format}"
                );
                let has_srgb = !matches!(format, TextureFormat::Bc4 | TextureFormat::Bc5);
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
        assert_eq!(both.len(), 9);
        assert_eq!(
            supported_formats(wgpu::Features::TEXTURE_COMPRESSION_ASTC),
            ASTC
        );
    }

    #[test]
    fn fit_letterboxes() {
        let r = Rect::fit(1920, 1080, 640, 480);
        assert_eq!((r.x, r.y, r.w, r.h), (0.0, 60.0, 640.0, 360.0));
        let r = Rect::fit(1920, 1080, 1000, 360);
        assert_eq!((r.x, r.y, r.w, r.h), (180.0, 0.0, 640.0, 360.0));
    }
}
