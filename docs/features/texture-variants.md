# Texture variants

GPU texture formats are not universal: desktop GPUs, Xbox, PlayStation and Apple Silicon
Macs/iPads sample BC formats; iPhones and most Android GPUs sample ASTC (and ETC2). A praxi
file can therefore carry the same video track in several texture formats, called
**variants**.

## Model

- A **track** is one logical video (for example "intro" or "menu background").
- Each variant of a track is a separate video stream in the file. Streams of the same track
  share a **group** id (see [PRAXI.md](../spec/PRAXI.md#stream-table)).
- All variants of a group have the same width, height, frame rate and frame timestamps.
  Their keyframes may differ.
- A file may hold several groups (several tracks played together, for example a background
  and an overlay with alpha).

## Selection

The engine tells the player which texture formats its GPU supports, in order of preference.
For each group the player selects the first format in that list that the group provides.
Chunks of variants that were not selected are skipped without being read or decompressed.

If a group has no supported variant, opening the file fails with an error naming the group.
A CPU fallback (decoding blocks to RGBA) may be added later behind an opt-in feature.

## Formats

| Format | Block | Bytes per block | Alpha | Produced by encoder |
|---|---|---|---|---|
| BC1 | 4x4 | 8 | no (opaque) | yes |
| BC3 | 4x4 | 16 | yes | no (reserved) |
| BC4 | 4x4 | 8 | single channel | no (reserved) |
| BC5 | 4x4 | 16 | two channels | no (reserved) |
| BC6H (HDR) | 4x4 | 16 | no | yes |
| BC7 | 4x4 | 16 | yes | yes |
| ASTC 4x4 | 4x4 | 16 | yes | yes |
| ASTC 5x5 | 5x5 | 16 | yes | no (reserved) |
| ASTC 6x6 | 6x6 | 16 | yes | yes |
| ASTC 8x8 | 8x8 | 16 | yes | yes |
| ASTC 4x4 HDR | 4x4 | 16 | no | yes |
| ASTC 6x6 HDR | 6x6 | 16 | no | no (reserved) |
| motion | 16x16 macroblock | variable | yes | yes |

`motion` is not a GPU texture format: it is a transform codec with motion compensation,
about 1/6 of BC7's size at equal quality, decoded into an `Rgba8Unorm` texture by compute
shaders ([motion-codec.md](motion-codec.md)). It needs compute shaders (Vulkan, Metal,
DX12, GLES 3.1; not WebGL 2), so `praxi-wgpu` lists it after the block formats: a group
that has both plays the block variant, which costs nothing to decode.

BC1 is half the size of BC7 for opaque video, at lower quality: two 16-bit endpoint
colors and 2-bit indices per block, so gradients band and colorful blocks smear. praxi
writes it opaque only. A BC1 stream never has the alpha flag, even when the source has
alpha, and every block uses the four-color mode, so it decodes with alpha 1. The 1-bit
alpha mode is not produced.

On the bench input (`make bench`, 1080p60, first 3 s), BC1 is 24.0 MB/s at 36.2 dB against
BC7's 55.6 MB/s at 41.0 dB, ASTC 6x6's 34.1 MB/s at 37.4 dB and ASTC 8x8's 18.4 MB/s at
34.0 dB, with 1.0 ms mean decode time per frame (BC7 1.5 ms). `--quality medium` and
`thorough` (2 and 3 least-squares passes) gain 0.2 dB, so `fast` is the default.

Numeric ids are in [PRAXI.md](../spec/PRAXI.md#texture-formats). Color data is sRGB encoded;
the stream's sRGB flag tells the engine to sample it with an sRGB texture format. HDR
formats hold linear light instead and sit in the same groups as SDR variants of the same
video; the engine's format order chooses between them ([hdr.md](hdr.md)).

## Typical setups

- Desktop + consoles + Apple Silicon: `bc7`, or `bc1` for opaque video where size matters
  more than quality.
- Mobile: `astc6x6` (good size/quality balance) or `astc4x4` (higher quality).
- Shipping one file everywhere: `bc7` + `astc6x6`. Platform-specific builds may encode one
  variant each instead.
- Smallest files on GPUs with compute shaders: `motion` alone, or `motion` + `astc6x6` for
  devices without compute shaders.
