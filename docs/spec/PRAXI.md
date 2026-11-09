# PRAXI file format

Version 0.9. File extension `.praxi`.

## Conventions

- All integers are little-endian and unsigned unless noted.
- Offsets are byte offsets from the start of the file.
- Reserved fields and reserved bits are written as zero and ignored by readers.
- Sizes in tables are in bytes.

## Layout

```
+--------------------+  0
| Header             |  48 bytes
+--------------------+  48
| Stream table       |  64 bytes per stream
+--------------------+  48 + 64 * stream_count
| Chunks             |  chunk header (24 bytes) + payload, repeated
+--------------------+  index_offset
| Index              |  24 bytes per entry
+--------------------+  end of file
```

## Header

| Offset | Size | Field | Description |
|---|---|---|---|
| 0 | 8 | `magic` | `89 50 52 58 0D 0A 1A 0A` (`\x89PRX\r\n\x1a\n`) |
| 8 | 2 | `version_major` | `0`. Readers reject other values. |
| 10 | 2 | `version_minor` | `8`. While `version_major` is `0`, minor versions may change the format incompatibly; readers reject minor versions other than their own. |
| 12 | 4 | `flags` | Reserved, `0`. |
| 16 | 2 | `stream_count` | Number of entries in the stream table, at least 1. |
| 18 | 2 | | Reserved. |
| 20 | 4 | | Reserved. |
| 24 | 8 | `duration_us` | Playback duration in microseconds. |
| 32 | 8 | `index_offset` | Offset of the index. Chunks end here. |
| 40 | 4 | `index_count` | Number of index entries. |
| 44 | 4 | | Reserved. |

## Stream table

`stream_count` entries of 64 bytes. A stream's number is its position in the table,
starting at 0.

| Offset | Size | Field | Description |
|---|---|---|---|
| 0 | 1 | `kind` | `1` video, `2` audio, `3` cues. Readers skip streams of unknown kinds. |
| 1 | 1 | | Reserved. |
| 2 | 2 | `group` | Video and audio: streams of the same kind and group are variants of one track; a player selects one of them. Video and audio groups are numbered independently. Cue streams: `0`, ignored. |
| 4 | 4 | `timebase_num` | Time base numerator, at least 1. |
| 8 | 4 | `timebase_den` | Time base denominator, at least 1. A pts of `p` is `p * num / den` seconds. |
| 12 | 4 | `chunk_count` | Number of chunks of this stream in the file. |
| 16 | 48 | `params` | Kind-specific parameters, below. |

### Video parameters (`kind = 1`)

| Offset | Size | Field | Description |
|---|---|---|---|
| 16 | 4 | `width` | Frame width in pixels, at least 1. |
| 20 | 4 | `height` | Frame height in pixels, at least 1. |
| 24 | 2 | `format` | Texture format id, see [Texture formats](#texture-formats). |
| 26 | 2 | `color_flags` | Bit 0: sRGB encoded color. Bit 1: alpha channel is meaningful (straight, not premultiplied). Other bits reserved. |
| 28 | 1 | `block_width` | Block width in pixels. Must match `format`. |
| 29 | 1 | `block_height` | Block height in pixels. Must match `format`. |
| 30 | 1 | `block_bytes` | Bytes per block. Must match `format`. |
| 31 | 33 | | Motion parameters for format `0x0201` (below), else reserved. |

Derived values:

- `blocks_x = ceil(width / block_width)`, `blocks_y = ceil(height / block_height)`
- `block_count = blocks_x * blocks_y`
- `frame_size = block_count * block_bytes`

#### Motion parameters

For format `0x0201` ([Motion payload](#motion-payload)):

| Offset | Size | Field | Description |
|---|---|---|---|
| 31 | 1 | `chroma` | `0` 4:2:0 (chroma planes at half width and height), `1` 4:4:4. Readers reject other values. |
| 32 | 1 | `chroma_qp_offset` | Signed. Added to a frame's `qp` for the chroma planes. |
| 33 | 1 | `alpha_qp_offset` | Signed. Added to a frame's `qp` for the alpha plane. |
| 34 | 30 | | Reserved. |

### Cue parameters (`kind = 3`)

| Offset | Size | Field | Description |
|---|---|---|---|
| 16 | 8 | `max_duration` | Largest cue `duration` in the stream, in the stream's time base. Used for seeking. |
| 24 | 32 | `name` | Stream name, UTF-8, zero padded. Holds no zero bytes; readers stop at the first zero byte. |
| 56 | 8 | | Reserved. |

### Audio parameters (`kind = 2`)

| Offset | Size | Field | Description |
|---|---|---|---|
| 16 | 4 | `sample_rate` | Sample frames per second, at least 1. The time base must be `1 / sample_rate` (`timebase_num = 1`, `timebase_den = sample_rate`), so a pts counts sample frames. |
| 20 | 2 | `channels` | Channels per frame, at least 1. |
| 22 | 2 | `sample_format` | `1` signed 16-bit integer (`s16`), `2` 32-bit float (`f32`), `3` ADPCM (`adpcm`, see [ADPCM payload](#adpcm-payload)). Readers reject other values. |
| 24 | 4 | `channel_mask` | Speakers fed by the channels, WAVE speaker bits (below). Either `0` (unspecified, channels in stored order) or exactly `channels` bits set; channels are interleaved in increasing bit order. |
| 28 | 4 | `preload_us` | How far ahead of their time chunks are stored, in microseconds. See [Ordering](#ordering). |
| 32 | 32 | `name` | Track name, UTF-8, zero padded. Holds no zero bytes; readers stop at the first zero byte. |

Speaker bits:

| Bit | Value | Speaker | Bit | Value | Speaker |
|---|---|---|---|---|---|
| 0 | `0x1` | front left | 9 | `0x200` | side left |
| 1 | `0x2` | front right | 10 | `0x400` | side right |
| 2 | `0x4` | front center | 11 | `0x800` | top center |
| 3 | `0x8` | low frequency | 12 | `0x1000` | top front left |
| 4 | `0x10` | back left | 13 | `0x2000` | top front center |
| 5 | `0x20` | back right | 14 | `0x4000` | top front right |
| 6 | `0x40` | front left of center | 15 | `0x8000` | top back left |
| 7 | `0x80` | front right of center | 16 | `0x10000` | top back center |
| 8 | `0x100` | back center | 17 | `0x20000` | top back right |

Bits 18-31 are reserved.

## Texture formats

| Id | Format | Block | Bytes per block |
|---|---|---|---|
| `0x0001` | BC1 | 4x4 | 8 |
| `0x0002` | BC3 | 4x4 | 16 |
| `0x0003` | BC4 | 4x4 | 8 |
| `0x0004` | BC5 | 4x4 | 16 |
| `0x0007` | BC7 | 4x4 | 16 |
| `0x0101` | ASTC 4x4 (LDR) | 4x4 | 16 |
| `0x0102` | ASTC 5x5 (LDR) | 5x5 | 16 |
| `0x0103` | ASTC 6x6 (LDR) | 6x6 | 16 |
| `0x0104` | ASTC 8x8 (LDR) | 8x8 | 16 |
| `0x0201` | motion (not a texture format) | 16x16 | 0 |

BC1 streams written by praxi have `color_flags` bit 1 (alpha) clear and use only the
four-color block mode (first endpoint color greater than the second as a 16-bit
integer).

For `0x0201`, `frame_size` and the block-based derived values do not apply; the block fields
describe the macroblock.

Readers reject video streams with unknown format ids or block fields that do not match.

## Chunks

Chunks follow the stream table directly and run until `index_offset`. Each chunk is a
24-byte header followed by `stored_size` bytes of payload. There is no padding between chunks.

| Offset | Size | Field | Description |
|---|---|---|---|
| 0 | 2 | `stream` | Stream number, less than `stream_count`. |
| 2 | 1 | `flags` | Bit 0: keyframe. Other bits reserved. |
| 3 | 1 | `compression` | `0` none, `1` LZ4 block, `2` byte planes (video only). |
| 4 | 4 | `stored_size` | Payload size in the file. |
| 8 | 4 | `raw_size` | Payload size after decompression. Equals `stored_size` when `compression = 0`. |
| 12 | 4 | | Reserved. |
| 16 | 8 | `pts` | Timestamp in the stream's time base. |

### Ordering

A chunk's *time* is `pts * num / den` seconds. Its *decode time* is its time for video and
cue chunks, and its time minus `preload_us` (but at least 0) for audio chunks, so audio is
stored ahead of when it plays.

Chunks are ordered by decode time, non-decreasing over the whole file. Chunks with equal
decode time may appear in any stream order. Within a video stream `pts` strictly increases;
within a cue stream it does not decrease (several cues may start together); within an audio
stream each chunk starts at or after the end of the previous one (`pts >= previous pts +
previous frame count`).

### Compression

- `0`: the payload is stored as is.
- `1`: the payload is one LZ4 block (the LZ4 block format, without frame header or size
  prefix) that decompresses to exactly `raw_size` bytes.
- `2`: the payload is in *byte-plane form*, stored as sections (below). Only allowed in
  video streams other than motion streams; readers reject it elsewhere.

Writers store motion payloads with compression `0`: they are entropy coded already.

#### Byte-plane form

A video payload (see [Video payload](#video-payload)) is a `prefix` followed by `n`
blocks of `block_bytes` bytes each:

- `prefix` is empty for keyframes and the `mask` (`ceil(block_count / 8)` bytes) for
  delta frames.
- `n = (raw_size - prefix_size) / block_bytes`, which must be a whole number; readers
  reject the chunk otherwise.
- Plane `k` (`0 <= k < block_bytes`) holds byte `k` of every block, in block order: `n`
  bytes.

The stored payload is `block_bytes + 1` sections, one after another: the prefix, then
planes `0` to `block_bytes - 1`. Each section is:

| Size | Field | Description |
|---|---|---|
| 1 | `codec` | `0` raw, `1` LZ4 block, `2` Huffman. Readers reject other values. |
| 4 | `size` | Bytes of `data`. |
| `size` | `data` | The section's bytes (`prefix_size` for the prefix, `n` for a plane), coded by `codec`. |

- Raw: `size` equals the section length and `data` is the bytes.
- LZ4: `data` is one LZ4 block that decompresses to exactly the section length.
- Huffman: see below. An empty section (keyframe prefix, or `n = 0`) is written raw with
  `size = 0`.

The sections must fill `stored_size` exactly. The decoded payload is the prefix followed by
the blocks, where byte `k` of block `b` is byte `b` of plane `k`; `raw_size` is
`prefix_size + n * block_bytes`.

#### Huffman section

| Size | Field | Description |
|---|---|---|
| 128 | `lengths` | Code length of each byte value, 4 bits each: value `2i` in the low nibble of byte `i`, value `2i + 1` in the high nibble. `0`: the value does not occur. At most `11`. |
| 12 | `stream_sizes` | Bytes of streams 0, 1 and 2, `u32` each. |
| rest | `streams` | Streams 0 to 3, one after another. Stream 3 takes the remaining bytes. |

The section's `len` bytes are split into 4 runs of `q = ceil(len / 4)` bytes (the last
runs shorter or empty): stream `j` codes bytes `j * q` up to `min(len, (j + 1) * q)`.

Codes are canonical: values sorted by code length, then by value, get consecutive codes
starting from 0 at the shortest length, shifted left by one bit for each length step
(as in DEFLATE). The lengths must form a complete code: the sum of `2^(11 - length)` over
all used values equals `2^11`. As the one exception, a single used value has length `1`
and code `0`.

Within a stream, codes are written one after another, most significant code bit first,
into the bits of the stream's bytes from least to most significant (byte 0 bit 0 first).
Unused bits of the last byte are zero. A reader rejects a stream whose codes need more
bits than the stream has.

## Video payload

The decompressed payload of a video chunk is a keyframe or a delta frame, given by the
chunk's keyframe flag. The first chunk of every video stream is a keyframe.

### Keyframe (flag set)

`frame_size` bytes: all blocks in row-major order, top-left block first. Each block is in
the native layout of the texture format, so the payload can be uploaded as is.

`raw_size` equals `frame_size`.

### Delta frame (flag clear)

| Size | Field | Description |
|---|---|---|
| `ceil(block_count / 8)` | `mask` | Bit `i` (byte `i / 8`, bit `i % 8`, least significant bit first) is set when block `i` is present. Bits past `block_count` are zero. |
| `changed * block_bytes` | `residuals` | One residual per present block, in increasing block order. `changed` is the number of set bits. |

The frame equals the previous frame of the same stream, with each present block XORed byte
by byte with its residual: `block = previous block ^ residual`.
`raw_size` equals `ceil(block_count / 8) + changed * block_bytes`.

## Motion payload

Video streams of format `0x0201` (the motion codec, `docs/features/motion-codec.md`). The
keyframe flag marks frames coded without reference to other frames; the first chunk of a
stream is a keyframe. A delta frame is predicted from the previous frame of its stream.

### Planes and macroblocks

- `mbs_x = ceil(width / 16)`, `mbs_y = ceil(height / 16)`. Planes are padded to whole
  macroblocks: Y is `16 * mbs_x` by `16 * mbs_y`; Co and Cg are the same in 4:4:4 and half
  of it in each direction in 4:2:0; A (only with color flag bit 1) is the size of Y.
- Pixel values are 8 bits. Planes are numbered Y 0, Co 1, Cg 2, A 3.
- Transform blocks are 8x8 pixels of one plane, numbered over all planes in plane order,
  then row-major within a plane: block `(bx, by)` of plane `p` has index
  `first_block(p) + by * (plane_width / 8) + bx`.
- A macroblock covers 16x16 luma pixels and holds, in this order: 4 Y blocks (row-major),
  the Co blocks (1 in 4:2:0, 4 in 4:4:4), the Cg blocks, and 4 A blocks with alpha.

### Frame

| Size | Field | Description |
|---|---|---|
| 1 | `qp` | Quantizer, `0` to `51`. |
| 1 | `slices` | Slice count, `1` to `mbs_y`. |
| 1 | `table_mask` | Bit `t` set: code lengths of table `t` follow. |
| 1 | | Reserved. |
| `128` per set bit | `tables` | Code lengths of each present table, in table order, as in [Huffman section](#huffman-section) (`lengths` field, a complete code or all zero). |
| `4 * slices` | `slice_sizes` | Bytes of each slice, `u32`. |
| rest | `slices` | The slices, one after another, filling the payload exactly. |

Tables: `0` macroblock, `1` motion vector, `2` Y DC, `3` Y AC, `4` chroma DC, `5` chroma AC,
`6` alpha DC, `7` alpha AC. A table absent from a keyframe has no codes; a table absent from
a delta frame keeps its codes from the previous frame. Tables 6 and 7 must be absent
without an alpha plane. Codes are canonical as in Huffman sections, at most 11 bits.

Slice `s` covers macroblock rows `s * mbs_y / slices` (rounded down) up to `(s + 1) *
mbs_y / slices`, its macroblocks in raster order. Each slice is a bit stream, least
significant bit of byte 0 first; Huffman codes are written most significant code bit first
into it, as in Huffman sections, and raw bit fields least significant bit first. Bits after
the last macroblock are zero; a reader rejects a slice whose syntax needs more bits than it
has. Predictions (below) are reset at the start of each slice.

### Macroblock

1. `mb`: a symbol of table 0: bits 0-1 `kind` (`0` skip, `1` inter, `2` intra, `3`
   split), bits 2-5 `cbp` (bit `i`: Y block `i` is coded), bit 6 `chroma` (all chroma
   blocks are coded), bit 7 `alpha` (all A blocks are coded). Keyframes hold only intra
   macroblocks. A skip macroblock is the symbol `0`.
2. Inter: `dx` then `dy`, values of table 1: the macroblock's vector is its predictor plus
   `(dx, dy)`. Split: four such differences, one per 8x8 luma quadrant in row-major
   order, each added to that quadrant's predictor. Each vector component must be within
   `-8191` to `8191`.
3. Inter, split and intra: the coded blocks in macroblock order.

A *value* of table `t` is a class `c` (a symbol, `0` to `15`) followed by `c` raw bits `b`:
`v = 0` for `c = 0`, `v = b` when bit `c - 1` of `b` is set, else `v = b - 2^c + 1`.

Motion vectors are in quarter pels and kept per 8x8 luma block: block `(bx, by)` is
quadrant `(by % 2) * 2 + bx % 2` of macroblock `(bx / 2, by / 2)`. Skip, inter and intra
macroblocks give all four quadrants one vector: skip takes the predictor, intra counts as
`(0, 0)`.

The predictor of a partition (the whole macroblock, width 2 blocks, or a quadrant of a
split macroblock, width 1) whose top-left 8x8 block is `(bx, by)` uses the neighbouring
blocks A left `(bx - 1, by)`, B above `(bx, by - 1)` and C above right `(bx + width, by -
1)`, or, when that block is not available, D above left `(bx - 1, by - 1)`. A block is
available when it lies inside the frame, belongs to a macroblock row of the slice, and
precedes the partition in decoding order: its macroblock comes earlier in raster order, or
it is an earlier quadrant of the same macroblock. When exactly one of A, B and C (or D)
is available it is the predictor; when none is, the predictor is `(0, 0)`; otherwise it is
the component-wise median of the three with unavailable ones as `(0, 0)`.

### Coded block

1. `dc`: a value of the plane's DC table (Y 2, chroma 4, alpha 6). For intra macroblocks it
   is added to the plane's DC predictor, the level of the previous coded block of that plane
   in an intra macroblock of the slice (`0` at the slice start); the result is the DC level
   and the new predictor. For inter macroblocks it is the DC level.
2. AC levels at zigzag positions 1 to 63, symbols of the plane's AC table (3, 5 or 7):
   `0x00` ends the block, `0xF0` skips 16 zero levels (and must leave the position below 64),
   any other `run << 4 | size` (`size` 1 to 15) skips `run` zero levels and gives the level
   at the next position as `size` raw bits, decoded like a value of class `size`. After
   position 63 no end symbol follows. Other symbols are invalid.

Levels must be within `-16383` to `16383`. Zigzag position `k` is row-major coefficient:

```
 0  1  8 16  9  2  3 10 17 24 32 25 18 11  4  5 12 19 26 33 40 48 41 34 27 20 13  6  7 14 21 28
35 42 49 56 57 50 43 36 29 22 15 23 30 37 44 51 58 59 52 45 38 31 39 46 53 60 61 54 47 55 62 63
```

### Reconstruction

All arithmetic is in 32-bit signed integers; `>>` is an arithmetic shift.

- Plane `qp`: `clamp(qp + offset, 0, 51)`, the offset being 0 for Y, `chroma_qp_offset` or
  `alpha_qp_offset`.
- Dequantization: `c = sign(l) * ((|l| * m[qp % 6] << (qp / 6)) >> 2)` clamped to
  `-32768..32767`, with `m = [40, 45, 51, 57, 64, 72]`.
- Inverse transform of coefficients `c[v][u]` (row `v`, column `u`) with the matrix `T`
  below: `g[y][u] = clamp((sum over v of T[v][y] * c[v][u] + 64) >> 7, -32768, 32767)`,
  then the residual `r[y][x] = (sum over u of T[u][x] * g[y][u] + 2048) >> 12`.
- Prediction: `128` for intra macroblocks. Otherwise each pixel takes the vector `(mx, my)`
  of its quadrant: luma-resolution planes (Y, A, and Co, Cg in 4:4:4) split the macroblock
  into 8x8 quadrants, 4:2:0 chroma into 4x4 quadrants. For a pixel `(x, y)` of a plane with
  `f` fraction bits (2; 3 for chroma in 4:2:0): `ix = x + (mx >> f)`, `fx = mx & (2^f -
  1)`, likewise `iy`, `fy`, and
  `p = clamp((sum over j of h[fy][j] * (sum over i of h[fx][i] * R(ix - o + i, iy - o + j))
  + 2048) >> 12, 0, 255)`, where `R` is the previous frame's plane with coordinates clamped
  to the padded plane, and `h`, `o` are the filters below: 8 taps with `o = 3` for planes
  with 2 fraction bits, 4 taps with `o = 1` for 4:2:0 chroma. Fraction 0 is the sample
  itself.
- A pixel is `clamp(prediction + residual, 0, 255)`; blocks without coefficients have a
  zero residual.

Filters (taps for `i` = 0 to 7, or 0 to 3), each summing to 64:

```
8 taps, fractions 0-3:           4 taps, fractions 0-3 and 4-7:
[  0  0   0 64  0   0  0  0 ]    [  0 64  0  0 ]    [ -4 36 36 -4 ]
[ -1  4 -10 58 17  -5  1  0 ]    [ -2 58 10 -2 ]    [ -4 28 46 -6 ]
[ -1  4 -11 40 40 -11  4 -1 ]    [ -4 54 16 -2 ]    [ -2 16 54 -4 ]
[  0  1  -5 17 58 -10  4 -1 ]    [ -6 46 28 -4 ]    [ -2 10 58 -2 ]
```

```
T = [ 64  64  64  64  64  64  64  64 ]
    [ 89  75  50  18 -18 -50 -75 -89 ]
    [ 83  36 -36 -83 -83 -36  36  83 ]
    [ 75 -18 -89 -50  50  89  18 -75 ]
    [ 64 -64 -64  64  64 -64 -64  64 ]
    [ 50 -89  18  75 -75 -18  89 -50 ]
    [ 36 -83  83 -36 -36  83 -83  36 ]
    [ 18 -50  75 -89  89 -75  50 -18 ]
```

### Display

RGBA of pixel `(x, y)` (`x < width`, `y < height`): `Y` from plane 0; `Co` and `Cg` the
plane sample in 4:4:4, and in 4:2:0 `(sum of w * C + 8) >> 4` over the two nearest samples
per axis with weights 3 (nearer) and 1, coordinates clamped to the plane: for even `x = 2i`
columns `i - 1` (weight 1) and `i` (3), for odd `x = 2i + 1` columns `i` (3) and `i + 1`
(1), likewise for rows. Then `cg = Cg - 128`, `co = Co - 128`, `t = Y - cg`, `G = Y + cg`,
`R = t + co`, `B = t - co`, each clamped to `0..255`, sRGB encoded. Alpha is plane 3, or
255 without alpha.

## Audio payload

Every audio chunk has the keyframe flag set and decodes on its own. It covers frames `pts`
to `pts + frames - 1`. Frames not covered by any chunk are silence.

For `s16` and `f32`, the decompressed payload is `frames * channels` samples, interleaved
frame by frame (all channels of frame 0, then frame 1, ...), each sample in
`sample_format`:

- `s16`: little-endian two's complement; `v` maps to `v / 32768`.
- `f32`: little-endian IEEE 754 binary32, nominally in `[-1, 1]`.

`raw_size` is a non-zero multiple of `channels * bytes_per_sample`; `frames` follows from it.

### ADPCM payload

`adpcm` codes each channel at 4.5 bits per sample: blocks of 16 samples, each with one of
16 fixed second-order predictors and a power-of-two step for the 4-bit prediction residuals.
The decompressed payload is:

| Size | Field | Description |
|---|---|---|
| 4 | `frames` | Sample frames in the chunk, at least 1. |
| `4 * channels` | `history` | Per channel, in channel order: `h1` then `h2`, two `i16` (two's complement) that seed the predictor, standing for the two samples before frame 0, `h1` the later one. Writers store the decoded values of the stream's two preceding samples (`0` before the first chunk), so consecutive chunks decode like one stream. |
| `9 * channels * ceil(frames / 16)` | `blocks` | Block `b` holds frames `16 * b` to `16 * b + 15`: 9 bytes per channel, in channel order. |

The 9 bytes of a channel's block:

| Size | Field | Description |
|---|---|---|
| 1 | `header` | High nibble: `predictor`, an index into the table below. Low nibble: `shift`, `0` to `12`. Readers reject other shifts. |
| 8 | `residuals` | 16 residuals of 4 bits, two's complement (`-8` to `7`). Byte `i` holds the residual of sample `2i` in its low nibble and of sample `2i + 1` in its high nibble. In the last block, residuals past `frames` are zero and ignored. |

`raw_size` equals `4 + 4 * channels + 9 * channels * ceil(frames / 16)`.

Each channel decodes its samples in order, starting from its `h1` and `h2`, in 32-bit signed
integer arithmetic (no intermediate value overflows):

```
p  = (c1[predictor] * h1 + c2[predictor] * h2 + 1024) >> 11    # arithmetic shift: floor
s  = clamp(p + residual * 2^shift, -32768, 32767)
h2 = h1
h1 = s
```

The decoded sample `s` maps to `s / 32768`. The predictor table (`c1`, `c2`, in units of
`1/2048`):

| Index | `c1` | `c2` | Index | `c1` | `c2` |
|---|---|---|---|---|---|
| 0 | 0 | 0 | 8 | 3197 | -1231 |
| 1 | 2048 | 0 | 9 | 2889 | -962 |
| 2 | -1024 | 0 | 10 | 2870 | -1719 |
| 3 | 4076 | -2041 | 11 | 2513 | -593 |
| 4 | 3944 | -1951 | 12 | 2022 | -1855 |
| 5 | 3817 | -1825 | 13 | 2006 | -99 |
| 6 | 3637 | -1686 | 14 | 1545 | 333 |
| 7 | 3434 | -1473 | 15 | 497 | -1376 |

## Cue payload

Each chunk of a cue stream holds one cue starting at the chunk's time. The keyframe flag is
set. The decompressed payload is:

| Size | Field | Description |
|---|---|---|
| 8 | `duration` | Duration in the stream's time base. `0` is an instantaneous cue. |
| 2 | `type_len` | Length of `type`, at least 1. |
| `type_len` | `type` | Cue type, UTF-8. |
| 2 | `field_count` | Number of fields. |
| ... | `fields` | `field_count` fields, below. |

The payload ends right after the last field; `raw_size` is the total.

Field:

| Size | Field | Description |
|---|---|---|
| 2 | `key_len` | Length of `key`, at least 1. |
| `key_len` | `key` | UTF-8. Keys are unique within a cue. |
| 1 | `value_type` | See below. |
| ... | `value` | Encoded by `value_type`. |

| `value_type` | Value | Encoding |
|---|---|---|
| `1` | string | `u32` length, then that many bytes of UTF-8 |
| `2` | integer | `i64`, two's complement |
| `3` | float | `f64`, IEEE 754 binary64 |
| `4` | bool | `u8`, `0` false or `1` true |
| `5` | bytes | `u32` length, then that many bytes |

Readers reject unknown value types, invalid UTF-8 and duplicate keys.

Field keys are free, except that float or integer fields `x` and `y` in `0..=1` together
give the cue a position on the video (`0,0` top left, `1,1` bottom right). This is a
convention of the tools and the player API, not a separate encoding: writers store them
as ordinary fields and readers that do not know it read them as such
([cues.md](../features/cues.md#screen-position)).

The cue ends at time `(pts + duration) * num / den`. The header `duration_us` should be at
least the latest cue end and the end of the last audio chunk.

## Index

`index_count` entries of 24 bytes at `index_offset`, ordered by chunk offset. The index has
one entry for every keyframe chunk of every video stream and one for every chunk of every
audio and cue stream.

| Offset | Size | Field | Description |
|---|---|---|---|
| 0 | 2 | `stream` | Stream number. |
| 2 | 6 | | Reserved. |
| 8 | 8 | `pts` | Chunk timestamp. |
| 16 | 8 | `offset` | Offset of the chunk header. |

### Seeking

To seek a set of selected streams to time `t`: for each selected video and audio stream take
the last index entry with time at or before `t` (or its first entry when there is none),
continue reading at the smallest of those offsets, decode chunks of each stream from its
entry on, and ignore earlier chunks of streams whose entry comes later. Audio frames before
`t` are dropped. Chunks are decoded up to decode time `t`, so audio is buffered up to
`t + preload`.

Cues active at `t` are found through the index: for each cue stream, read the cues whose
time lies in `[t - max_duration, t]` and keep those ending after `t`.

## Changelog

- 0.1: initial version.
- 0.2: marker streams replaced by cue streams (stream parameters, payload, index entries);
  cue streams allow equal pts.
- 0.3: cue streams lose the `language` parameter; `name` moves to offset 24.
- 0.4: audio streams (parameters, PCM payload, index entries); chunks are ordered by decode
  time, which stores audio `preload_us` ahead.
- 0.5: compression `2`, LZ4 over byte planes, for video chunks.
- 0.6: delta frames store residuals (XOR with the previous block) instead of blocks;
  compression `2` stores the prefix and each plane as a raw, LZ4 or Huffman section.
- 0.7: audio sample format `3`, ADPCM.
- 0.8: video format `0x0201`, the motion codec, with its stream parameters and payload.
- 0.9: motion payload: macroblock kind `3` (split, a vector per 8x8 quadrant), vector
  prediction per 8x8 block, 8- and 4-tap interpolation filters instead of bilinear.
