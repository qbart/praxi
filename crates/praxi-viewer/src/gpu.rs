//! Compressed texture upload and drawing, shared by the window and screenshots.

use praxi::{Capabilities, Player, TextureFormat};
use std::io::{Read, Seek};

/// BC formats in order of preference, then ASTC.
const BC: [TextureFormat; 5] =
    [TextureFormat::Bc7, TextureFormat::Bc3, TextureFormat::Bc1, TextureFormat::Bc5, TextureFormat::Bc4];
const ASTC: [TextureFormat; 4] =
    [TextureFormat::Astc4x4, TextureFormat::Astc5x5, TextureFormat::Astc6x6, TextureFormat::Astc8x8];

const SHADER: &str = r#"
struct Params {
    uv_scale: vec2<f32>,
    uv_max: vec2<f32>,
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
    return textureSample(tex, samp, min(in.uv * params.uv_scale, params.uv_max));
}

@fragment
fn fs_checker(in: VsOut) -> @location(0) vec4<f32> {
    let odd = ((u32(in.pos.x) / 16u) + (u32(in.pos.y) / 16u)) & 1u;
    let v = select(0.2, 0.35, odd == 1u);
    return vec4<f32>(v, v, v, 1.0);
}
"#;

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
pub fn capabilities(adapter: &wgpu::Adapter, only: Option<TextureFormat>) -> Result<Capabilities, String> {
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
    adapter.features() & (wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::TEXTURE_COMPRESSION_ASTC)
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
        let (w, h) = ((width as f32 * scale).round().max(1.0), (height as f32 * scale).round().max(1.0));
        Rect { x: ((target_w as f32 - w) / 2.0).floor(), y: ((target_h as f32 - h) / 2.0).floor(), w, h }
    }
}

struct TrackTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    format: TextureFormat,
    blocks: (u32, u32),
    /// `VideoFrame::index` of the uploaded frame.
    uploaded: Option<u64>,
}

pub struct Renderer {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    blend: wgpu::RenderPipeline,
    replace: wgpu::RenderPipeline,
    checker: wgpu::RenderPipeline,
    tracks: Vec<TrackTexture>,
}

impl Renderer {
    /// Creates pipelines drawing into `target` and one texture per video track of `player`.
    pub fn new<R: Read + Seek>(device: &wgpu::Device, target: wgpu::TextureFormat, player: &Player<R>) -> Renderer {
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
        let pipeline = |label, layout: Option<&wgpu::PipelineLayout>, fragment, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout,
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
                    entry_point: Some(fragment),
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
        let mut renderer = Renderer {
            blend: pipeline("blend", Some(&pipeline_layout), "fs_video", Some(blend)),
            replace: pipeline("replace", Some(&pipeline_layout), "fs_video", None),
            checker: pipeline("checker", None, "fs_checker", None),
            layout,
            sampler,
            tracks: Vec::new(),
        };
        for info in player.tracks() {
            let p = &info.params;
            renderer.tracks.push(renderer.track_texture(device, p.format, p.srgb(), p.width, p.height));
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
            size: wgpu::Extent3d { width: padded_w, height: padded_h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format(format, srgb),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let params: [f32; 4] = [
            width as f32 / padded_w as f32,
            height as f32 / padded_h as f32,
            (width as f32 - 0.5) / padded_w as f32,
            (height as f32 - 0.5) / padded_h as f32,
        ];
        let bytes: Vec<u8> = params.iter().flat_map(|v| v.to_le_bytes()).collect();
        let uniform = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("track"),
                contents: &bytes,
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("track"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
            ],
        });
        TrackTexture { texture, bind_group, format, blocks, uploaded: None }
    }

    /// Uploads the frames that changed since the last call. Returns how many were uploaded.
    pub fn upload<R: Read + Seek>(&mut self, queue: &wgpu::Queue, player: &Player<R>) -> usize {
        let mut uploaded = 0;
        for (i, t) in self.tracks.iter_mut().enumerate() {
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

    /// Clears `view` to black and draws every track that has a frame into `rect`. With
    /// `checker`, a checkerboard goes under the tracks; without, track 0 replaces the target.
    pub fn draw(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, rect: Rect, checker: bool) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("praxi"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_viewport(rect.x, rect.y, rect.w, rect.h, 0.0, 1.0);
        if checker {
            pass.set_pipeline(&self.checker);
            pass.draw(0..4, 0..1);
        }
        let mut first = !checker;
        for t in self.tracks.iter().filter(|t| t.uploaded.is_some()) {
            pass.set_pipeline(if first { &self.replace } else { &self.blend });
            pass.set_bind_group(0, &t.bind_group, &[]);
            pass.draw(0..4, 0..1);
            first = false;
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
        assert_eq!(supported_formats(wgpu::Features::TEXTURE_COMPRESSION_ASTC), ASTC);
    }

    #[test]
    fn fit_letterboxes() {
        let r = Rect::fit(1920, 1080, 640, 480);
        assert_eq!((r.x, r.y, r.w, r.h), (0.0, 60.0, 640.0, 360.0));
        let r = Rect::fit(1920, 1080, 1000, 360);
        assert_eq!((r.x, r.y, r.w, r.h), (180.0, 0.0, 640.0, 360.0));
    }
}
