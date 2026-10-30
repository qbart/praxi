# Praxi

praxi (from praxinoscope animation device) is the video codec for games in pure rust. It uses very little CPU/GPU power, handles multi-channel audio beautifully, and decodes lightning-fast without lagging the game engine. It allows for seamless transitions between gameplay and video, supports transparency channels (alpha transparency), and handles multi-stream playback.

The player can also emit events (e.g. playback started/finished, loop points, custom timed cues embedded in the video, such as markers or subtitles) so the game engine can hook into them.

## Usage

Requires `ffmpeg` and `ffprobe` on `PATH` for encoding.

```sh
make build
./target/release/praxi encode input.mp4 -o intro.praxi --variant bc7 --variant astc6x6 --cue 2.5=explosion
./target/release/praxi info intro.praxi
```

Playback from Rust:

```rust
let caps = praxi::Capabilities::new([praxi::TextureFormat::Bc7]);
let mut player = praxi::Player::open(std::fs::File::open("intro.praxi")?, &caps)?;
player.play();
// every game frame:
player.advance(dt)?;
while let Some(event) = player.poll_event() { /* Started, Cue, CueEnd, Finished, ... */ }
if let Some(frame) = player.video_frame(0) { /* upload frame.data as a compressed texture */ }
```

## Docs

- Features: [docs/features/](docs/features/)
- File format: [docs/spec/PRAXI.md](docs/spec/PRAXI.md)
