# Overview

praxi is a video codec and player for games, written in pure Rust. Frames are stored as
GPU block-compressed textures (BC7, ASTC), so playback is a light decompress plus a texture
upload: no YUV conversion and no per-pixel CPU work. Frames stay compressed in VRAM.

## Goals

- **Cheap decode.** A 1080p frame decodes in 0.6-1.5 ms on one CPU core with the smallest
  encoding, or 0.15-0.45 ms when the encoder trades size for speed
  ([video-codec.md](video-codec.md#decode-cost-and-writer-policy)). The player allocates
  nothing per frame and spawns no threads.
- **Alpha.** Video streams can carry straight (non-premultiplied) alpha.
- **Per-platform variants.** One video track can be stored in several GPU texture formats
  (BC for desktop/consoles, ASTC for mobile); the player picks the one the GPU supports
  (see [texture-variants.md](texture-variants.md)).
- **Seamless transitions.** Fast seeking to keyframes, looping and timed cues, so the game
  can cut between gameplay and video without hitches (see [player.md](player.md)).
- **Events.** The player emits events (started, finished, looped...) the engine can hook
  into, including custom timed **cues** stored in the file, such as markers or anything else the game defines
  (see [player.md](player.md) and [cues.md](cues.md)).
- **Multi-stream playback.** Several players, and several tracks in one file, can play at
  the same time.
- **Engine agnostic.** The player hands out raw block data; uploading it is the engine's job.
- **Multi-channel audio.** Any number of PCM, ADPCM or Opus audio tracks in any channel layout, with
  layout variants, handed to the engine's mixer from any thread and kept in sync with the
  video (see [audio.md](audio.md)).

## Non-goals

- Competing with H.265/AV1 on file size. The block formats trade size for decode cost (see
  [Baseline measurements](#baseline-measurements)); the motion codec
  ([motion-codec.md](motion-codec.md)) gets within reach of H.264 at high quality.
- Real-time encoding. Encoding is an offline asset-pipeline step and may be slow.
- Container compatibility with KTX2, MP4 or Matroska. praxi has its own container
  ([docs/spec/PRAXI.md](../spec/PRAXI.md)) without per-frame overhead.

## Architecture

Cargo workspace under `crates/`:

| Crate | Role | Notable dependencies |
|---|---|---|
| `praxi-format` | Container reader and writer, chunk compression, cue sheets (feature `cue-sheet`) | `lz4_flex`, optional `serde_json` |
| `praxi` | Runtime player: variant selection, frame reconstruction, clock, events | `praxi-format` |
| `praxi-encoder` | Encoder library: RGBA frames to `.praxi` | `imgio`, `praxi-format` |
| `praxi-cli` | `praxi` command line tool (encode via ffmpeg, inspect files, edit cues) | `praxi-encoder`, `clap`, `serde_json` |
| `praxi-wgpu` | Texture upload and drawing for `wgpu` engines ([engine-integration.md](engine-integration.md)) | `praxi`, `wgpu` |
| `praxi-viewer` | GPU viewer: plays files in a window, screenshots ([viewer.md](viewer.md)) | `praxi-wgpu`, `winit`, `praxi-audio` |
| `praxi-editor` | Cue editor: timeline, waveform, inspector ([cue-editor.md](cue-editor.md)) | `praxi-wgpu`, `eframe`, `praxi-audio` |
| `praxi-bevy` | Bevy plugin: video into an `Image`, events as messages, a cue editor example ([bevy.md](bevy.md)) | `praxi-wgpu`, `bevy`, `bevy_egui` (example) |
| `praxi-godot` | Godot 4.4+ extension: `VideoStreamPraxi` for `VideoStreamPlayer`, cues as signals ([godot.md](godot.md)) | `praxi`, `godot` (godot-rust) |
| `praxi-audio` | Audio output for the viewer and the editor, with rate and channel conversion ([audio-output.md](audio-output.md)) | `praxi`, `cpal` |

The runtime (`praxi`, `praxi-format`) does not depend on `imgio`, the pure-Rust texture
codec library. Only the encoder does, with default features off (no image files, no GPU
loaders); the CLI adds its `png` feature for `praxi frame`. See [video-codec.md](video-codec.md) and
[encoder.md](encoder.md).

## Baseline measurements

Big Buck Bunny, 1080p60, 10 s (`make samples`), source H.264 at 1.05 MB/s. Skip threshold 3.

| Variant | Raw | + LZ4 | + temporal skip | Blocks skipped | PSNR |
|---|---|---|---|---|---|
| BC7 (UASTC fastest) | 124 MB/s | 125 MB/s | 89 MB/s | 29% | 41.4 dB |
| BC7 + RDO lambda 1 | 124 MB/s | 108 MB/s | 82 MB/s | 29% | 40.0 dB |
| BC7 + RDO lambda 3 | 124 MB/s | 89 MB/s | 67 MB/s | 29% | 35.6 dB |
| ASTC 4x4 fast | 124 MB/s | 125 MB/s | 89 MB/s | 29% | - |
| ASTC 6x6 fast | 55 MB/s | 56 MB/s | 44 MB/s | 21% | - |
| ASTC 8x8 fast | 31 MB/s | 31 MB/s | 26 MB/s | 17% | - |

LZ4 decompression took 0.05-0.5 ms per frame. PSNR is per frame before temporal skip.

These were early experiments. Current numbers come from `make bench` (first 3 s, after
temporal skip) and are recorded in [bench/baseline.json](../../bench/baseline.json): BC7
41.0 dB at 55.6 MB/s, BC1 36.2 dB at 24.0 MB/s, ASTC 6x6 37.4 dB at 34.1 MB/s, ASTC 8x8 34.0 dB at 18.4 MB/s
(Huffman-coded byte planes of residuals), with 0.6-1.5 ms mean decode time per frame on one core of a Ryzen AI 9 HX 370 (the
machine is recorded in the baseline's `machine`).

## Milestones

1. **M0** Container format, encoder (BC7, ASTC), player with events. Specs in this folder.
2. **M1** Frame viewer (wgpu) for visual checks; size, PSNR and decode-time benchmarks in CI.
   Done: GPU viewer with screenshots ([viewer.md](viewer.md)), `praxi bench` with a CI
   baseline check ([benchmarks.md](benchmarks.md)).
3. **M2** Multi-channel audio streams and A/V sync. Done: PCM tracks with layout variants,
   game-clock sync, an audio-master clock and ADPCM ([audio.md](audio.md)).
4. **M3** Better compression: motion copy, zstd, codebook frame types. Done: byte planes
   with per-plane LZ4 or Huffman, XOR residuals in delta frames
   ([video-codec.md](video-codec.md#compression)). Measured and set aside: zstd, block
   motion copy, context modeling.
5. **M4** Engine integration examples: integration guide, `praxi-wgpu`, headless and
   windowed examples ([engine-integration.md](engine-integration.md)).
6. **M5** Authoring: cue sheets to edit the cues of encoded files, subtitle conversion
   ([cue-editing.md](cue-editing.md)), live preview of cue sheets in the viewer
   ([cue-preview.md](cue-preview.md)), a cue editor with a timeline
   ([cue-editor.md](cue-editor.md)).
7. **M6** Motion codec: transform coding with motion compensation, about 1/6 of BC7's size
   at equal PSNR, entropy decoded on CPU slices in parallel and reconstructed on the GPU,
   playable in the viewer, editor and `cutscene` ([motion-codec.md](motion-codec.md)).
8. **M7** Formats, tests and decoding: `praxi cues add` / `remove` and "Write cues into .praxi"
   that releases the player's file ([cue-editing.md](cue-editing.md),
   [cue-editor.md](cue-editor.md)); BC1 at about half of BC7's size
   ([texture-variants.md](texture-variants.md)); `make test`, `check`, `test-strict` and
   `check-windows` with CLI end-to-end and GPU block-format tests, without CI
   ([testing.md](testing.md)); motion decode-ahead of up to 4 frames
   ([motion-codec.md](motion-codec.md#decode-ahead)).
9. **M8** Audio output and Bevy: `praxi-audio` with rate and channel conversion for the
   viewer and the editor ([audio-output.md](audio-output.md)); an Opus evaluation
   ([audio.md](audio.md#opus-evaluation)), then Opus audio in format 0.10
   ([audio.md](audio.md#opus)); `praxi-bevy` with a `bevy_egui` cue editor
   example ([bevy.md](bevy.md)).
10. **M9** Cues at runtime: every cue in memory at open, typed cues with serde, active and
    upcoming cues, cue positions mapped to the drawn video, typed cue messages in Bevy, and
    a `cue_game` example ([cue-runtime.md](cue-runtime.md)).
11. **M10** Cue authoring follow-ups: typed values on the command line, so `x=0.5;y=0.3`
    is a position ([cues.md](cues.md#values-on-the-command-line)), and positions in the
    Bevy cue editor ([bevy.md](bevy.md#cue-editor-example)).
12. **M11** Bevy audio and observable cues: `PraxiAudioPlugin` plays the audio tracks
    through Bevy's audio with the video's clock following the sound
    ([bevy.md](bevy.md#audio)), and `PraxiEvent` / `PraxiCue<T>` are entity events that
    observers see as they happen ([bevy.md](bevy.md#cues-as-they-happen)).
13. **0.1.1** First release (0.1.0 was tagged but not published): crates.io metadata, dual MIT / Apache-2.0 license, a README
    per crate, `make package`, `make tag` and `make publish`, release binaries for Linux, macOS and Windows built on GitHub
    ([releasing.md](releasing.md)).
14. **M12** Godot runtime: `praxi-godot`, a GDExtension for Godot 4.4+ that plays `.praxi`
    files in `VideoStreamPlayer` with block-compressed textures, audio and cues as signals,
    an example project, and the addon zip in releases ([godot.md](godot.md)).
15. **0.2.0** Second release: the Godot addon in a release for the first time, opt-in
    mipmaps for video on 3D surfaces in Bevy and the video box example
    ([bevy.md](bevy.md#mipmaps)), and the machine recorded in benchmarks.
16. **M13** HDR video in format 0.11: BC6H and ASTC 4x4 / 6x6 HDR variants as linear
    half-float textures with a color description, PQ, HLG and OpenEXR sources, SDR
    variants tonemapped from them, HDR drawing in `praxi-wgpu`, the viewer, Bevy and Godot
    ([hdr.md](hdr.md)); every texture encoder and decoder from `imgio`
    ([video-codec.md](video-codec.md#encoding-paths)).

## Next

Planned work, in order, is in [ROADMAP.md](../../ROADMAP.md).
