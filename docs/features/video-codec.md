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

Each chunk is compressed with LZ4 (block format, `lz4_flex`) only when that makes it
smaller; otherwise it is stored uncompressed. BC7/ASTC data is close to incompressible for
LZ4 on its own; LZ4 pays off on delta frames (sparse masks) and with rate-distortion
optimised (RDO) BC7 encoding.

## Encoding paths

Both come from `ktx-rs` (encoder only):

- **BC7**: each 4x4 block is encoded to UASTC (`basisu::uastc_enc::encode_uastc`), optionally
  post-processed by UASTC RDO (`uastc_rdo`), then transcoded to BC7
  (`basisu::uastc_bc7::transcode_uastc_to_bc7`).
- **ASTC**: `astcenc::astcenc_compress_image` with the LDR sRGB profile.

Frames whose size is not a multiple of the block size are padded by repeating the last
column and row; the stream's width and height keep the real size.
