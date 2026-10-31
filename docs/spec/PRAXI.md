# PRAXI file format

Version 0.4. File extension `.praxi`.

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
| 10 | 2 | `version_minor` | `4`. While `version_major` is `0`, minor versions may change the format incompatibly; readers reject minor versions other than their own. |
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
| 31 | 33 | | Reserved. |

Derived values:

- `blocks_x = ceil(width / block_width)`, `blocks_y = ceil(height / block_height)`
- `block_count = blocks_x * blocks_y`
- `frame_size = block_count * block_bytes`

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
| 22 | 2 | `sample_format` | `1` signed 16-bit integer (`s16`), `2` 32-bit float (`f32`). Readers reject other values. |
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

Readers reject video streams with unknown format ids or block fields that do not match.

## Chunks

Chunks follow the stream table directly and run until `index_offset`. Each chunk is a
24-byte header followed by `stored_size` bytes of payload. There is no padding between chunks.

| Offset | Size | Field | Description |
|---|---|---|---|
| 0 | 2 | `stream` | Stream number, less than `stream_count`. |
| 2 | 1 | `flags` | Bit 0: keyframe. Other bits reserved. |
| 3 | 1 | `compression` | `0` none, `1` LZ4 block. |
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
| `changed * block_bytes` | `blocks` | The present blocks, in increasing block order. `changed` is the number of set bits. |

The frame equals the previous frame of the same stream, with each present block replaced.
`raw_size` equals `ceil(block_count / 8) + changed * block_bytes`.

## Audio payload

Every audio chunk has the keyframe flag set. The decompressed payload is `frames * channels`
samples, interleaved frame by frame (all channels of frame 0, then frame 1, ...), each sample
in `sample_format`:

- `s16`: little-endian two's complement; `v` maps to `v / 32768`.
- `f32`: little-endian IEEE 754 binary32, nominally in `[-1, 1]`.

`raw_size` is a non-zero multiple of `channels * bytes_per_sample`; `frames` follows from it.
The chunk covers frames `pts` to `pts + frames - 1`. Frames not covered by any chunk are
silence.

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
