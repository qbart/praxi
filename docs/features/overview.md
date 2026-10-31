# Overview

praxi is a video codec and player for games, written in pure Rust. Frames are stored as
GPU block-compressed textures (BC7, ASTC), so playback is a light decompress plus a texture
upload: no YUV conversion and no per-pixel CPU work. Frames stay compressed in VRAM.

## Goals

- **Cheap decode.** A 1080p frame decodes in well under 1 ms on one CPU core. The player
  allocates nothing per frame and spawns no threads.
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
- **Multi-channel audio.** Any number of PCM audio tracks in any channel layout, with
  layout variants, handed to the engine's mixer from any thread and kept in sync with the
  video (see [audio.md](audio.md)).

## Non-goals

- Competing with H.264/AV1 on file size. praxi trades size for decode cost; see
  [Baseline measurements](#baseline-measurements).
- Real-time encoding. Encoding is an offline asset-pipeline step and may be slow.
- Container compatibility with KTX2, MP4 or Matroska. praxi has its own container
  ([docs/spec/PRAXI.md](../spec/PRAXI.md)) without per-frame overhead.

## Architecture

Cargo workspace under `crates/`:

| Crate | Role | Notable dependencies |
|---|---|---|
| `praxi-format` | Container reader and writer, chunk compression | `lz4_flex` |
| `praxi` | Runtime player: variant selection, frame reconstruction, clock, events | `praxi-format` |
| `praxi-encoder` | Encoder library: RGBA frames to `.praxi` | `ktx-rs`, `praxi-format` |
| `praxi-cli` | `praxi` command line tool (encode via ffmpeg, inspect files) | `praxi-encoder`, `clap` |

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

## Milestones

1. **M0** Container format, encoder (BC7, ASTC), player with events. Specs in this folder.
2. **M1** Frame viewer (wgpu) for visual checks; size, PSNR and decode-time benchmarks in CI.
3. **M2** Multi-channel audio streams and A/V sync.
4. **M3** Better compression: motion copy, zstd, codebook frame types.
5. **M4** Engine integration examples.
