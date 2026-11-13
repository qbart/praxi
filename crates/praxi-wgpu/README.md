# praxi-wgpu

Texture upload and drawing of praxi video for engines built on
[`wgpu`](https://crates.io/crates/wgpu) 30. Block formats are uploaded as compressed
textures; the motion codec is decoded on the GPU with compute shaders.

```rust
let caps = praxi_wgpu::capabilities(&adapter, None)?;
let features = praxi_wgpu::required_features(&adapter);                       // request these
let limits = praxi_wgpu::required_limits(&adapter, wgpu::Limits::default());  // and these
let mut player = praxi::Player::open(file, &caps)?;
let mut video = praxi_wgpu::VideoRenderer::new(&device, surface_format, &player);
// every frame:
player.advance(dt)?;
video.prepare(&device, &queue, &mut player, 1.0);
// inside a render pass:
video.draw(&mut pass, praxi_wgpu::Rect::fit(width, height, target_w, target_h));
```

The `cutscene` example plays a file in a window with fades.
