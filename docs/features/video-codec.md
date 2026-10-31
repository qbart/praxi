# Video codec

A praxi video frame is a grid of GPU texture blocks. Decoding never touches pixels: it
decompresses the chunk (LZ4 or Huffman, when used) and writes blocks into the current frame
buffer, which the engine uploads as a compressed texture.

## Frame types

- **Keyframe**: every block of the frame. Decodable on its own; seeking starts here.
- **Delta frame**: a bitmask with one bit per block plus a *residual* for every block whose
  bit is set. Blocks whose bit is clear keep their value from the previous frame of the
  same stream.

The byte layout is in [PRAXI.md](../spec/PRAXI.md#video-payload).

### Residuals

A changed block is not stored as is: its residual is the new block XORed byte by byte with
the block at the same position in the previous frame, and the decoder XORs the residual
into its frame buffer. The encoder takes the previous block from what the decoder shows
(after temporal skip), so both sides stay in step.

A block that changes usually keeps its mode, partition and the high bits of its endpoints,
so most residual bytes are zero or small. That does little for LZ4 but a lot for an entropy
coder: in byte planes of BC7 output, the endpoint bytes drop from about 7.6 to 4.3-5.0 bits
of order-0 entropy per byte, the index bytes from 7.5 to 6.7. XOR costs the same as the copy
it replaces.

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

Each chunk is stored in one of these forms:

- **raw**;
- **LZ4** (the block format of `lz4_flex`);
- **byte planes** (video chunks only): the blocks are transposed so that byte 0 of every
  block comes first, then byte 1 of every block, and so on. The mask of a delta frame and
  each of these planes are then stored as separate *sections*, each one raw, LZ4 or
  Huffman coded, whichever the writer picks for that section.

Interleaved BC7/ASTC blocks are close to incompressible, but each byte position of a block
has its own statistics: mode and partition bytes take few distinct values across a frame,
and residual bytes (see [Residuals](#residuals)) are skewed toward zero. In byte planes
these become runs that LZ4 finds, or skewed byte distributions that an order-0 Huffman code
compresses. On Big Buck Bunny 1080p (`make bench` input, 180 frames), percent of the
uncompressed payload:

| Variant | LZ4 | LZ4 over byte planes | byte planes, residuals, best section codec |
|---|---|---|---|
| BC7 | 100.0% | 88.0% | 63.2% |
| ASTC 6x6 | 100.0% | 95.0% | 78.0% |
| ASTC 8x8 | 99.9% | 94.7% | 71.5% |

Most BC7 planes end up Huffman coded; the mode plane and the two near-constant planes
(bytes 6 and 7) are smaller as LZ4 runs, since Huffman needs at least one bit per byte.

### Huffman sections

An order-0 canonical Huffman code per section, at most 11 bits per code, with the symbols
split into 4 streams decoded in an interleaved loop (four independent bit readers, refilled
64 bits at a time, five symbols per refill; one 2048-entry lookup per symbol). The code
lengths are stored with the section (128 bytes), so every chunk decodes on its own. Byte
layout in [PRAXI.md](../spec/PRAXI.md#byte-plane-form).

Measured alternatives: 8 streams were slower than 4 (register pressure), a 4096-entry
table decoding two symbols per lookup was slower because building it costs more than it
saves on 1 MB chunks, and rANS would only beat Huffman on the skewed planes that LZ4
already handles (Huffman sizes are within 1% of the order-0 entropy on the rest).

### Decode cost and writer policy

The reader turns sections back into planes and planes back into blocks while reading the
chunk (8x8 byte tiles transposed in `u64`s), so the player and the engine see the usual
block layout. All of it is safe portable Rust.

Huffman decodes at about 1.4 GB/s of output on the development machine (Ryzen AI 9 HX 370),
against several GB/s for LZ4 on incompressible data. A Huffman BC7 1080p frame takes about
1.1 ms to entropy-decode against 0.3 ms for LZ4 over planes. Smaller files trade against
decode time, so the writer only moves to a slower form when it pays:

1. raw or LZ4, whichever is smaller (LZ4 only when smaller than raw);
2. within byte planes, each section takes raw or LZ4, whichever is smaller, or Huffman when
   that is smaller still by at least `plane_min_saving` percent (default 8);
3. byte planes instead of step 1 when their total is smaller than step 1 by at least
   `plane_min_saving` percent.

With the default, residual planes (15-40% saving) are Huffman coded, while the noisy index
planes of keyframes (about 5%) stay raw. `plane_min_saving = 0` always takes the smallest
form; `100` never uses byte planes. `Writer::write_chunk` with `compression: None` applies
this rule, with the threshold set by `Writer::set_plane_min_saving` (default 8).

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
- **Context modeling** (a plane's code chosen by byte 0 of the same block): no gain on BC7,
  3.5-4.5% of raw on ASTC (with residuals), for one code table per context.

## Encoding paths

Both come from `ktx-rs` (encoder only):

- **BC7**: each 4x4 block is encoded to UASTC (`basisu::uastc_enc::encode_uastc`), optionally
  post-processed by UASTC RDO (`uastc_rdo`), then transcoded to BC7
  (`basisu::uastc_bc7::transcode_uastc_to_bc7`).
- **ASTC**: `astcenc::astcenc_compress_image` with the LDR sRGB profile.

Frames whose size is not a multiple of the block size are padded by repeating the last
column and row; the stream's width and height keep the real size.
