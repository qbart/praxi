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
- **Multi-channel audio.** Any number of PCM or ADPCM audio tracks in any channel layout, with
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
| `praxi-encoder` | Encoder library: RGBA frames to `.praxi` | `ktx-rs`, `praxi-format` |
| `praxi-cli` | `praxi` command line tool (encode via ffmpeg, inspect files, edit cues) | `praxi-encoder`, `clap`, `serde_json` |
| `praxi-wgpu` | Texture upload and drawing for `wgpu` engines ([engine-integration.md](engine-integration.md)) | `praxi`, `wgpu` |
| `praxi-viewer` | GPU viewer: plays files in a window, screenshots ([viewer.md](viewer.md)) | `praxi-wgpu`, `winit`, `cpal` |
| `praxi-editor` | Cue editor: timeline, waveform, inspector ([cue-editor.md](cue-editor.md)) | `praxi-wgpu`, `eframe`, `cpal` |

The runtime (`praxi`, `praxi-format`) does not depend on `ktx-rs`. Only the encoder does,
with default features off (no Vulkan loader). See [video-codec.md](video-codec.md) and
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
(Huffman-coded byte planes of residuals), with 0.6-1.5 ms mean decode time per frame.

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

## Next

Planned work, in order, is in [ROADMAP.md](../../ROADMAP.md).
