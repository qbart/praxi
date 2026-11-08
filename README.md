# Praxi

praxi (from praxinoscope animation device) is the video codec for games in pure rust. It uses very little CPU/GPU power, handles multi-channel audio beautifully, and decodes lightning-fast without lagging the game engine. It allows for seamless transitions between gameplay and video, supports transparency channels (alpha transparency), and handles multi-stream playback.

The player can also emit events (e.g. playback started/finished, loop points, custom timed cues embedded in the video, with any type and fields the game defines) so the game engine can hook into them.

## Usage

Requires `ffmpeg` and `ffprobe` on `PATH` for encoding.

```sh
make build
./target/release/praxi encode input.mp4 -o intro.praxi --variant bc7 --variant astc6x6 --cue 2.5=explosion \
    --audio-layout stereo --audio-layout 5.1
./target/release/praxi info intro.praxi
./target/release/praxi frame intro.praxi --at 2.5 -o frame.png
```

Edit cues after encoding, without re-encoding video ([cue-editing.md](docs/features/cue-editing.md)):

```sh
./target/release/praxi cues export intro.praxi -o intro.cues.json
./target/release/praxi cues srt subtitles.en.srt --field lang=en -o subs.en.json
./target/release/praxi-viewer intro.praxi --cues intro.cues.json --cues subs.en.json  # live preview, reloads on save
./target/release/praxi-editor intro.praxi intro.cues.json  # timeline editor with waveform
./target/release/praxi cues import intro.praxi intro.cues.json subs.en.json -o intro.praxi
```

Use `--audio-format adpcm` for audio 3.6 times smaller than 16-bit PCM
([audio.md](docs/features/audio.md#adpcm)).

Watch a file on the GPU (Space pauses, arrows seek, L loops, M mutes, Q quits):

```sh
./target/release/praxi-viewer intro.praxi
./target/release/praxi-viewer intro.praxi --screenshot frame.png --at 2.5
```

Playback from Rust:

```rust
let caps = praxi::Capabilities::new([praxi::TextureFormat::Bc7]).max_audio_channels(2);
let mut player = praxi::Player::open(std::fs::File::open("intro.praxi")?, &caps)?;
let audio = player.audio_output(0).unwrap(); // Send + Sync: read it from the audio callback
// audio callback: let n = audio.read(buf); buf[n..].fill(0.0);
player.play()?;
// every game frame:
player.advance(dt)?;
while let Some(event) = player.poll_event() { /* Started, Cue, CueEnd, Finished, ... */ }
if let Some(frame) = player.video_frame(0) { /* upload frame.data as a compressed texture */ }
```

Engine integration guide, the `praxi-wgpu` helper crate and runnable examples (a headless
game loop, decoding on a worker thread, a wgpu cutscene with fades):
[docs/features/engine-integration.md](docs/features/engine-integration.md).

## Docs

- Features: [docs/features/](docs/features/)
- File format: [docs/spec/PRAXI.md](docs/spec/PRAXI.md)
- Benchmark results format: [docs/spec/BENCH.md](docs/spec/BENCH.md)
- Roadmap: [ROADMAP.md](ROADMAP.md)
- Tests and manual checks: [docs/features/testing.md](docs/features/testing.md) (`make check`)
