//! Video frames as an egui texture.
//!
//! egui expects textures holding sRGB-encoded bytes in `Rgba8Unorm`. The video is drawn into
//! an `Rgba8UnormSrgb` texture, which encodes on write, and egui samples it through an
//! `Rgba8Unorm` view of the same texture.

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use praxi::Player;
use praxi_wgpu::{Rect, VideoRenderer};
use std::io::{Read, Seek};

pub struct VideoView {
    renderer: VideoRenderer,
    target: wgpu::TextureView,
    pub texture: egui::TextureId,
    pub size: (u32, u32),
}

impl VideoView {
    pub fn new<R: Read + Seek>(state: &RenderState, player: &Player<R>) -> VideoView {
        let video = player.tracks()[0].params;
        let size = (video.width, video.height);
        let texture = state.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        });
        let target = texture.create_view(&Default::default());
        let encoded = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            ..Default::default()
        });
        let id = state.renderer.write().register_native_texture(&state.device, &encoded, wgpu::FilterMode::Linear);
        let renderer = VideoRenderer::new(&state.device, wgpu::TextureFormat::Rgba8UnormSrgb, player);
        VideoView { renderer, target, texture: id, size }
    }

    /// Uploads and draws the frames that changed since the last call.
    pub fn update<R: Read + Seek>(&mut self, state: &RenderState, player: &Player<R>) {
        if self.renderer.prepare(&state.queue, player, 1.0) == 0 {
            return;
        }
        let mut encoder = state.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            let (w, h) = self.size;
            self.renderer.draw_replace(&mut pass, Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 });
        }
        state.queue.submit([encoder.finish()]);
    }
}
