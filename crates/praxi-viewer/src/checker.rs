//! The gray checkerboard drawn under the video in the window, so transparency is visible.

const SHADER: &str = r#"
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    return vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let odd = ((u32(pos.x) / 16u) + (u32(pos.y) / 16u)) & 1u;
    let v = select(0.2, 0.35, odd == 1u);
    return vec4<f32>(v, v, v, 1.0);
}
"#;

pub struct Checker {
    pipeline: wgpu::RenderPipeline,
}

impl Checker {
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Checker {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("checker"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("checker"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(target.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        Checker { pipeline }
    }

    /// Fills `rect` with the checkerboard.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, rect: praxi_wgpu::Rect) {
        pass.set_viewport(rect.x, rect.y, rect.w, rect.h, 0.0, 1.0);
        pass.set_pipeline(&self.pipeline);
        pass.draw(0..4, 0..1);
    }
}
