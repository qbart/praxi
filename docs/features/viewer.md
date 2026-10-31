# Viewer

`praxi-viewer` plays a `.praxi` file in a window using the GPU, the way a game would:
compressed blocks are uploaded as-is to BC or ASTC textures and sampled by a shader, audio
goes to the default output device. It is the visual check for encoder output (milestone M1)
and the reference for engine integration.

`praxi frame` ([frame-export.md](frame-export.md)) decodes on the CPU; the viewer exercises
the real GPU path, including texture format support and alpha blending.

## Crate

`crates/praxi-viewer`, binary `praxi-viewer`. It is a separate crate so the CLI and the
runtime crates do not depend on a GPU stack.

| Dependency | Use |
|---|---|
| `praxi-wgpu` | Compressed textures, uploads and drawing ([engine-integration.md](engine-integration.md#praxi-wgpu)) |
| `wgpu` | Device, surface, checkerboard |
| `winit` | Window and input |
| `cpal` (feature `audio`, default) | Audio output. Build with `--no-default-features` on systems without audio libraries (ALSA on Linux). |
| `png` | Screenshots |
| `clap` | Arguments |

## Usage

```sh
praxi-viewer intro.praxi
praxi-viewer intro.praxi --format astc6x6 --loop
praxi-viewer intro.praxi --screenshot frame.png --at 2.5
```

| Option | Default | Meaning |
|---|---|---|
| `--format <F>` | best supported | Only accept this texture format: `bc1`, `bc3`, `bc4`, `bc5`, `bc7`, `astc4x4`, `astc5x5`, `astc6x6`, `astc8x8`. Fails if the GPU cannot sample it. |
| `--loop` | off | Start with looping on. |
| `--paused` | off | Open paused at the first frame. |
| `--at <SECONDS>` | `0` | Start position. |
| `--no-audio` | | Do not open an audio device. |
| `--clock <C>` | `audio` | `audio`: the clock follows the audio output while audio plays; `game`: the wall clock is master ([audio.md](audio.md#audio-master-clock)). |
| `--screenshot <PNG>` | | Render the frame at `--at` without a window, write it and exit. |
| `--size <WxH>` | video size | Screenshot size. |

The adapter and backend are chosen by wgpu and can be overridden with its environment
variables, for example `WGPU_BACKEND=gl` or `WGPU_ADAPTER_NAME=llvmpipe`.

## Format selection

The viewer builds the player's `Capabilities` from the adapter's features:

1. With `--format F`: `[F]` if the adapter supports it, otherwise an error.
2. Otherwise BC formats if `TEXTURE_COMPRESSION_BC` is available (BC7, BC3, BC1, BC5,
   BC4), followed by ASTC formats if `TEXTURE_COMPRESSION_ASTC` is available (4x4, 5x5,
   6x6, 8x8). Desktop GPUs get BC, mobile and Apple silicon ASTC, as in
   [texture-variants.md](texture-variants.md).

`max_audio_channels` is the output device's default channel count. The viewer prints the
adapter, backend and the selected variant of every track at start-up.

## Rendering

- Each video track gets one texture in the stream's format, `*UnormSrgb` when the stream is
  sRGB. The texture is padded up to whole blocks, as GPUs require for compressed formats;
  the shader crops the padding through its texture coordinates.
- When a track's `VideoFrame::index` changes, the whole frame is uploaded with
  `Queue::write_texture` (rows of `blocks_x * block_bytes` bytes). There is no CPU decode
  and no conversion.
- Tracks are drawn in track order, all stretched to the same rectangle: the largest one
  with track 0's aspect ratio that fits the window (letterboxed). Alpha is straight, so
  later tracks are blended with `src_alpha, 1 - src_alpha`.
- In the window, a gray checkerboard is drawn first so transparency is visible.
- BC4 and BC5 show as stored: red, and red plus green.
- The surface uses an sRGB format and vsync (`Fifo`).

## Playback

- Each redraw calls `player.advance(elapsed)` with the wall-clock time since the last one.
  When audio plays, the clock follows the output (`Clock::Audio` on the first mixed track,
  latency from cpal's callback and playback timestamps); `--clock game` keeps the wall
  clock as master.
- Every enabled audio track with the same sample rate and channel count as audio track 0
  is mixed (summed) into one output stream opened at that rate and channel count; other
  tracks are reported and skipped. The viewer does not resample: if the device refuses the
  rate or channel count, the viewer prints why and plays without sound.
- Events are printed to stdout with the position, for example
  `[   2.500] cue explosion id=4096 stream=2 power=0.8`.
- The window title shows the file name, position, duration, state and track 0's format,
  updated four times a second.

### Keys

| Key | Action |
|---|---|
| Space | Play / pause (after the end: play from the start) |
| Left / Right | Seek 1 s back / forward (with Shift: 5 s) |
| Home | Seek to the start |
| L | Toggle looping |
| M | Mute / unmute |
| Esc, Q | Quit |

On exit the viewer prints the number of frames presented and the average and maximum time
spent in `advance` and in uploads, per presented frame.

## Screenshot

`--screenshot` renders without a window or audio device:

- The player is seeked to `--at` (clamped to the duration), exactly like `praxi frame`.
- The image is rendered into an offscreen `Rgba8UnormSrgb` texture of `--size` (default:
  track 0's size) and written as an 8-bit RGBA PNG.
- There is no checkerboard: track 0 is written as-is, alpha included, and later tracks
  are blended over it (color `src_alpha, 1 - src_alpha`, alpha `1, 1 - src_alpha`).
- Outside the video rectangle (when `--size` has another aspect ratio) pixels are
  transparent black.
- At the video size, the PNG matches `praxi frame` up to differences between the GPU's
  and the CPU's block decoders.
