# praxi-audio

Plays the audio tracks of a praxi player on the default output device through
[`cpal`](https://crates.io/crates/cpal), converting sample rate and channel layout to the
device's own format. Used by `praxi-viewer` and `praxi-editor`; games usually hand
`Player::audio_output` to their own mixer instead.

```rust
let device = praxi_audio::Device::open_default().ok_or("no audio device")?;
let muted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
let stream = device.start(vec![player.audio_output(0).unwrap()], muted.clone())?;
player.set_clock(praxi::Clock::Audio { track: 0, latency: stream.latency() });
```

On Linux, building needs the ALSA headers (`libasound2-dev` or `alsa-lib-devel`).
