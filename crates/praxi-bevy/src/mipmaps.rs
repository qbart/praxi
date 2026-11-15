//! Draws the mip chain of a video image from its level 0 (`docs/features/bevy.md#mipmaps`).

const SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle covering the target.
@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: Out;
    o.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment fn fs(o: Out) -> @location(0) vec4<f32> {
    return textureSampleLevel(src, smp, o.uv, 0.0);
}
"#;

/// The number of levels of a full mip chain for a `width` x `height` image.
pub fn levels(width: u32, height: u32) -> u32 {
    32 - width.max(height).max(1).leading_zeros()
}

/// The pipeline drawing one level from the level above it.
pub struct Mipmaps {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl Mipmaps {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Mipmaps {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("praxi mipmaps"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("praxi mipmaps"),
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
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("praxi mipmaps"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("praxi mipmaps"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("praxi mipmaps"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Mipmaps { pipeline, layout, sampler }
    }

    /// Records drawing every level of `texture` below level 0, each from the one above.
    pub fn record(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture) {
        let level = |i: u32| {
            texture.create_view(&wgpu::TextureViewDescriptor { base_mip_level: i, mip_level_count: Some(1), ..Default::default() })
        };
        for i in 1..texture.mip_level_count() {
            let (src, dst) = (level(i - 1), level(i));
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("praxi mipmaps"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("praxi mipmaps"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn full_chain_levels() {
        assert_eq!(super::levels(1, 1), 1);
        assert_eq!(super::levels(2, 1), 2);
        assert_eq!(super::levels(40, 24), 6);
        assert_eq!(super::levels(1920, 1080), 11);
        assert_eq!(super::levels(1280, 720), 11);
    }
}
