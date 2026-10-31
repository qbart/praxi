# Video codec

A praxi video frame is a grid of GPU texture blocks. Decoding never touches pixels: it
decompresses LZ4 (when used) and copies blocks into the current frame buffer, which the
engine uploads as a compressed texture.

## Frame types

- **Keyframe**: every block of the frame. Decodable on its own; seeking starts here.
- **Delta frame**: a bitmask with one bit per block plus the blocks whose bit is set. Blocks
  whose bit is clear keep their value from the previous frame of the same stream.

The byte layout is in [PRAXI.md](../spec/PRAXI.md#video-payload).

## Temporal skip

For each block position the encoder remembers the source pixels of the last block it
emitted there. A block in a delta frame is skipped when either:

- every channel of every pixel differs by at most the **skip threshold** from those
  remembered pixels, or
- its encoded bytes equal the previously emitted block at that position.

Comparing against the last *emitted* source (not the previous frame) keeps slow drifts from
accumulating: a slow gradient is re-emitted once its total change exceeds the threshold.
Threshold 0 skips only blocks with identical source pixels or identical encoded bytes.

## Keyframe interval

The encoder writes a keyframe every `keyint` frames (default 60) and at frame 0. Seeking
decodes from the preceding keyframe, so `keyint` bounds the seek cost.

## Compression

Each chunk is stored in one of these forms (LZ4 is the block format of `lz4_flex`):

- **raw**;
- **LZ4**;
- **LZ4 over byte planes** (video chunks only): the blocks are transposed so that byte 0 of
  every block comes first, then byte 1 of every block, and so on, and the result is LZ4
  compressed. A delta frame's mask stays in front, untouched.

Interleaved BC7/ASTC blocks are close to incompressible for LZ4, but a few byte positions
of each block (mode, partition and similar header bits) take few distinct values across a
frame. In byte planes those become long runs. On Big Buck Bunny 1080p (`make bench` input):

| Variant | LZ4 | LZ4 over byte planes |
|---|---|---|
| BC7 | 100.0% | 88.0% |
| BC7, RDO lambda 1 | 91.4% | 88.0% |
| ASTC 6x6 | 100.0% | 95.1% |
| ASTC 8x8 | 99.9% | 94.7% |

(percent of the uncompressed payload). The reader turns planes back into blocks while
reading the chunk, so the player and the engine see the usual block layout. It transposes
8x8 byte tiles held in `u64`s, in safe portable Rust.

Byte planes cost decode time: LZ4 has to run over the whole chunk (most of the cost is in
the few low-entropy planes), and the planes have to be transposed back. On the same input,
mean decode time per 1080p frame rises from 0.25 to 0.45 ms for BC7 (for 12% less data)
and from 0.11 to 0.19 ms for ASTC 6x6 (for 5% less). So the writer picks:

1. raw or LZ4, whichever is smaller (LZ4 only when smaller than raw);
2. byte planes instead, when they are smaller than that by at least the encoder's
   `plane_min_saving` percent (default 8).

With the default, BC7 chunks (12% saving) use byte planes and ASTC chunks (about 5%) do
not. `plane_min_saving = 0` always takes the smallest form; `100` never uses byte planes.
`Writer::write_chunk` with `compression: None` applies this rule, with the threshold set by
`Writer::set_plane_min_saving` (default 8).

### Rejected for now

Measured on the same input, 180 frames, and not worth their cost for camera footage:

- **zstd** instead of LZ4: 7.5% (level 3) to 12.5% (level 19) smaller than LZ4 on
  interleaved blocks, but the pure-Rust decoder (`ruzstd`) needs about 4 ms per 1080p BC7
  frame against 0.06 ms for LZ4, and the C library would end pure Rust.
- **Block motion copy** (a changed block copied from another block position of the
  previous frame, within 4 blocks): only 0.6% of changed BC7 blocks and 0.01% of ASTC 8x8
  blocks have such a source within the skip threshold. Copies must be block aligned, and
  camera motion rarely is. It may still pay off for 2D or UI content that scrolls in
  whole blocks.
- **Entropy coding of byte planes** is the next step: the order-0 entropy of the planes is
  78% (BC7) and 85-88% (ASTC) of the raw size, against 88% and 95% for LZ4 over planes.

## Encoding paths

Both come from `ktx-rs` (encoder only):

- **BC7**: each 4x4 block is encoded to UASTC (`basisu::uastc_enc::encode_uastc`), optionally
  post-processed by UASTC RDO (`uastc_rdo`), then transcoded to BC7
  (`basisu::uastc_bc7::transcode_uastc_to_bc7`).
- **ASTC**: `astcenc::astcenc_compress_image` with the LDR sRGB profile.

Frames whose size is not a multiple of the block size are padded by repeating the last
column and row; the stream's width and height keep the real size.
