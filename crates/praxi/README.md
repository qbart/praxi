# praxi

The praxi video player for games. Frames are stored as GPU block-compressed textures
(BC7, BC1, ASTC) or as a GPU-decoded motion codec, so playback is a light decompress and a
texture upload, with no YUV conversion and no per-pixel CPU work. The player also plays
PCM, ADPCM and Opus audio and emits timed cues (subtitles, markers, anything the game
defines) as events.

```rust
let caps = praxi::Capabilities::new([praxi::TextureFormat::Bc7]).max_audio_channels(2);
let mut player = praxi::Player::open(std::fs::File::open("intro.praxi")?, &caps)?;
let audio = player.audio_output(0).unwrap(); // Send + Sync: read it from the audio callback
player.play()?;
// every game frame:
player.advance(dt)?;
while let Some(event) = player.poll_event() { /* Started, Cue, CueEnd, Looped, Finished, ... */ }
if let Some(frame) = player.video_frame(0) { /* upload frame.data as a compressed texture */ }
```

The player allocates nothing per frame and spawns no threads. Feature `serde` parses cues
into game types (`cue.parse::<GameCue>()`), `cue-sheet` reads JSON cue sheets.

Make files with the `praxi` command line tool (`cargo install praxi-cli`). Engines on
`wgpu` can use `praxi-wgpu`, Bevy games `praxi-bevy`.

Reads and writes file format 0.10.
