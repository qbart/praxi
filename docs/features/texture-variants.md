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
| BC1 | 4x4 | 8 | 1-bit | no (reserved) |
| BC3 | 4x4 | 16 | yes | no (reserved) |
| BC4 | 4x4 | 8 | single channel | no (reserved) |
| BC5 | 4x4 | 16 | two channels | no (reserved) |
| BC7 | 4x4 | 16 | yes | yes |
| ASTC 4x4 | 4x4 | 16 | yes | yes |
| ASTC 5x5 | 5x5 | 16 | yes | no (reserved) |
| ASTC 6x6 | 6x6 | 16 | yes | yes |
| ASTC 8x8 | 8x8 | 16 | yes | yes |

Numeric ids are in [PRAXI.md](../spec/PRAXI.md#texture-formats). Color data is sRGB encoded;
the stream's sRGB flag tells the engine to sample it with an sRGB texture format.

## Typical setups

- Desktop + consoles + Apple Silicon: `bc7`.
- Mobile: `astc6x6` (good size/quality balance) or `astc4x4` (higher quality).
- Shipping one file everywhere: `bc7` + `astc6x6`. Platform-specific builds may encode one
  variant each instead.
