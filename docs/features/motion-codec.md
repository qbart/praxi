# Motion codec

A second video codec next to the block textures of [video-codec.md](video-codec.md):
transform coding with motion compensation, the family of H.264, decoded mostly on the
GPU. It trades a little decode work for files several times smaller than BC7 at the
same or better quality.

Status: phase 1 (bitstream and encoder) and phase 2 (GPU decoder) done; the byte layout
is in [PRAXI.md](../spec/PRAXI.md#motion-payload) (format 0.9). Playback decodes on the
GPU only; there is no CPU playback path. The GPU decoder matches the reference byte for
byte on lavapipe; its time on a hardware GPU is not measured yet.

## Why

Block textures cost about 1 byte per pixel: a 1080p60 video is about 56 MB/s after praxi's
compression, against 1 MB/s for the H.264 source ([overview.md](overview.md#baseline-measurements)).
Smaller block formats (ASTC 8x8, ETC1S) lose quality and have a fixed quality ceiling. A
transform codec has no ceiling: its quality is set by its bitrate, and it reaches BC7's
quality (41 dB PSNR on the bench input) at a fraction of BC7's size.

## Goals

- **Quality**: at least the PSNR of the BC7 variant of the same input (RGB PSNR as in
  [benchmarks.md](benchmarks.md)), with no visible blocking at that quality.
- **Size**: at most 1/5 of the BC7 variant at equal PSNR on the bench input. Phase 1
  stops the project if it cannot get there.
- **Decode**: at 1080p, at most 1 ms per frame of entropy decoding on the CPU (slices on
  up to 4 cores, keyframes included) and at most 1 ms of GPU per frame on an integrated GPU. Exact output: the GPU decoder
  produces the same pixels as the encoder's reference reconstruction.
- **Fits praxi**: one more variant of a video track, same container, cues, audio, seeking,
  looping and events. Pure Rust, no new runtime dependencies.

Non-goals: competing with H.265/AV1 at low bitrates, real-time encoding, compatibility with
any standard bitstream.

## Where it fits

The codec is a video stream *format*, `motion`, next to `bc7` and `astc*`:

```sh
praxi encode intro.mp4 -o intro.praxi --variant motion
praxi encode intro.mp4 -o intro.praxi --variant motion --variant bc7   # both, player picks
```

- A track can carry `motion` together with block variants. Variant selection works as
  today ([texture-variants.md](texture-variants.md#selection)): the engine lists the
  formats it can play in order of preference, and lists `motion` only when it has a GPU
  decoder for it (compute shaders). A file meant to play everywhere also carries a block
  variant.
- Choose `motion` where size matters (downloads, mobile, long videos); choose `bc7` or
  `astc*` where decode cost must be close to zero (many videos at once, weak GPUs).

| | BC7 | motion |
|---|---|---|
| File size (bench input) | 56 MB/s | 8.7 MB/s at equal PSNR (goal: under 11) |
| CPU per 1080p frame | 0.6-1.5 ms (decompress) | 0.4 ms entropy decode on 4 threads (keyframes 1.3 ms) |
| GPU per 1080p frame | upload only | goal: under 1 ms (compute) |
| GPU memory per track (1080p) | 2 MB | about 14 MB (two frames of planes + RGBA) |
| Seek | decode to the frame | decode every frame since the keyframe |

## Design

### Color

- Pixels are coded as Y, Co, Cg planes (YCoCg), 8 bits each, from sRGB-encoded RGB.
  The inverse is exact integer arithmetic (normative):
  `cg = Cg - 128; co = Co - 128; t = Y - cg; G = Y + cg; R = t + co; B = t - co`, each
  clamped to 0..255. The encoder's forward transform is
  `Y = (R + 2G + B) / 4`, `Co = (R - B) / 2 + 128`, `Cg = (2G - R - B) / 4 + 128`, rounded.
  YCoCg instead of a YCbCr matrix: exact integers, cheap, no matrix choice to signal.
- Chroma: 4:2:0 by default (Co and Cg at half width and height), 4:4:4 as an option for
  graphics and UI with sharp colored edges. Chroma samples sit at the center of their 2x2
  luma pixels. Upsampling to RGBA is bilinear with integer weights 9/3/3/1 over 16,
  rounded, edges clamped (normative, so CPU and GPU output match).
- Alpha (stream flag "alpha meaningful"): a fourth plane at full resolution, coded like Y
  with its own quantizer offset.

### Blocks and frame types

- The frame is split into 16x16 **macroblocks** (luma), padded at the right and bottom by
  repeating the last column and row. Each macroblock holds 8x8 **transform blocks**: 4 luma,
  1 Co and 1 Cg in 4:2:0 (4 each in 4:4:4), and 4 alpha when present.
- **Keyframes** (I) code every macroblock without reference to other frames.
- **Delta frames** (P) predict from the previous decoded frame only. There are no
  bidirectional frames: decode order is display order, a frame never waits for a later one,
  and the player keeps one reference frame per track.
- The keyframe flag, keyframe interval and index work as for block textures. The encoder
  also starts a keyframe at a scene cut (when most macroblocks code cheaper as intra).

### Prediction

- **No spatial pixel prediction.** H.264-style intra prediction makes every block wait for
  its left and upper neighbours, which serializes the GPU. Here every block of a frame
  reconstructs independently, so one compute dispatch decodes a frame. Intra coding gets
  its spatial redundancy from the transform and from **DC prediction** in the coefficient
  domain: a block's DC coefficient is coded as the difference to the previous intra block's
  DC of the same plane in the slice (resolved during entropy decoding, on the CPU).
- **Macroblock types in P frames**:
  - `skip`: the predicted motion vector, no residual. Covers static areas and uniform pans.
  - `inter`: a motion vector difference, a coded block pattern (which transform blocks have
    coefficients) and their coefficients, added to the motion-compensated prediction.
  - `split`: as `inter`, with one vector per 8x8 luma quadrant (four differences). Chroma
    quadrants (4x4 in 4:2:0) use their quadrant's vector. Covers motion edges and objects
    smaller than a macroblock.
  - `intra`: coefficients only, as in keyframes; the prediction is 128.
- **Motion vectors**: quarter-pel for luma (chroma uses the same vector at its own
  resolution, eighth-pel in 4:2:0). Sub-pel samples come from separable integer filters
  (normative): 8 taps for full-resolution planes (the HEVC luma filters), 4 taps for 4:2:0
  chroma (the HEVC chroma filters), one rounding shift after both passes. Positions outside
  the reference frame clamp to its edge, so vectors may point outside. Bilinear
  interpolation, the first choice, blurred fine texture: the filters cut the stream by
  22% at equal PSNR.
- Vectors are predicted from the median of the left, upper and upper-right neighbours
  (upper-left when the upper-right one is not decoded yet) at 8x8 granularity, inside the
  slice; only the difference is coded.

### Transform and quantization

- 8x8 integer DCT approximation (the 64/89/83/75/50/36/18 matrix). The inverse is
  normative: two 1-D passes in 32-bit integer arithmetic with a rounding shift after each
  pass and the intermediate clamped to 16 bits; the residual is added to the prediction and
  clamped to 0..255. Shifts are *pinned in phase 1*.
- A quantizer `qp` from 0 to 51; the step size doubles every 6 (`level * scale[qp % 6] <<
  (qp / 6)`, rounded and clamped to 16 bits), flat over all coefficients. Per frame `qp`,
  per stream offsets for chroma and alpha, an optional per macroblock `qp` difference for
  rate control. Scale table *pinned in phase 1*.
- Flat quantization instead of a perceptual matrix: the target is PSNR parity with BC7, and
  flat steps maximize PSNR per bit.
- No in-loop deblocking filter at first: at the target quality blocking is below
  visibility, and a filter adds two dependent GPU passes. Phase 1 measures whether a
  filter pays at lower bitrates.

### Entropy coding

- Coefficients of each transform block in zigzag order become JPEG-style tokens: one byte
  per nonzero coefficient, `(zero run, magnitude class)`, plus the class's raw extra bits;
  an end-of-block token ends the block. The DC difference has its own class token.
- Tokens are coded with praxi's canonical Huffman codes (code lengths stored as in
  [Huffman sections](../spec/PRAXI.md#huffman-section), at most 11 bits), with separate
  tables for luma AC, chroma AC, alpha AC, DC classes, macroblock type plus coded block
  pattern, and motion vector classes, decoded with single 2048-entry lookup tables. Extra
  bits follow their token in the same stream: a separate stream for them measured slower.
- Keyframes carry all tables; a P frame carries only the tables it changes and inherits the
  rest from the previous frame (decoding starts at a keyframe anyway).
- A frame is split into **slices** of whole macroblock rows. Predictions (DC, motion
  vectors) do not cross slices and each slice has its own bit stream, so slices are
  entropy-decoded on several threads (see [Decoding](#decoding)).
- Context-adaptive coding was estimated on the bench input: choosing the AC table by
  macroblock kind, zigzag band and previous magnitude saves about 6% of the bits. Adaptive
  arithmetic coding would save more but decodes several times slower; neither is used.

## Decoding

Decoding has two stages:

1. **Entropy decoding** (CPU, in the player): the chunk becomes a **coded frame** (below).
   Slices decode in parallel, goal under 1 ms per 1080p frame.
2. **Reconstruction** (GPU, in `praxi-wgpu` or the engine's own shaders): per transform
   block, motion-compensated prediction (or 128), dequantization, inverse transform, add,
   clamp, into the frame's planes; then color conversion to RGBA for display. Fully
   parallel over blocks.

### Parallel entropy decoding

`motion::Decoder` decodes the slices of a frame on up to `threads` threads at once, the
calling thread included:

- `Decoder::new` uses `std::thread::available_parallelism`, at most 8, so that the decoder
  leaves cores to the game; `Decoder::set_threads(n)` changes it (1 decodes on the calling
  thread only). No more threads than slices are used.
- Worker threads start when first needed and wait between frames (spawning threads per
  frame cost about 0.1 ms). Plain `std` threads, mutex and condition variable: no new
  dependency, no `unsafe`.
- The header, tables and slice sizes are read on the calling thread. The slices then go
  into a queue in order and every thread, the calling one too, takes the next slice until
  none is left. Cores differ in speed (and may be asleep), so taking slices as they come
  balances better than fixed shares.
- What the slices read (layout, tables, a copy of the payload) is shared through an `Arc`
  that only the decoder holds between frames, so it is updated in place. Copying the
  payload costs about 30 us for a keyframe.
- Each slice decodes into its own coded frame, kept by the decoder between frames so its
  buffers are reused; slice 0 decodes into the output's buffers. While other slices still
  decode, the calling thread appends finished slices to the output in slice order, the
  first-coefficient position of each block offset by the coefficients before its slice.
  The output is identical to decoding on one thread.
- Each slice has its own rows of the 8x8 vector field, since predictions never look
  outside the slice and the field does not carry over between frames.
- Errors report the first failing slice, as on one thread.

The encoder writes 16 slices by default (`--slices`): at 1080p that is 4-5 macroblock rows
per slice, at no measurable cost in size on the bench input, and enough slices for slow
and fast cores to balance.

### Coded frames

`praxi_format::motion::CodedFrame`, all `u32` words so it uploads as is:

- `mbs`: 5 words per macroblock, row-major: the kind, then the vectors of its four 8x8
  luma quadrants (x in the low, y in the high 16 bits, two's complement, quarter pels). The
  four are equal except for `split` macroblocks.
- `blocks`: 2 words per transform block with nonzero levels: its index (bits 0-24) and
  level count (bits 25-31), then the position of its first entry in `coeffs`.
- `coeffs`: the nonzero levels, block after block: the row-major position (bits 0-5) and
  the level (high 16 bits). At BC7 quality on the bench input a delta frame's lists take
  0.9 MB against 2.9 MB for 64 coefficients per coded block, and cost the decoder less to
  write.
- `scales`: the dequantization factor per plane. The GPU dequantizes, which took a fifth
  of the CPU time.

There is no CPU playback path: reconstructing on the CPU would cost an estimated 10-20 ms
per 1080p frame, too slow for a game. The same reconstruction exists on the CPU only as
the **reference** in `praxi_format::motion`: the encoder needs it (it predicts from exactly
what the decoder will show), the tests compare the GPU output with it, and offline tools
use it (`praxi frame`, `praxi bench`).

Seeking decodes every frame from the preceding keyframe through both stages. Only the last
frame needs color conversion. With `keyint` 60 a worst-case 1080p seek costs about 60
frames of decoding (estimated 50-100 ms); preroll ([engine-integration.md](engine-integration.md#seamless-transitions))
hides it for transitions, and seek-heavy uses can encode with a shorter `keyint`.

### Player API

- `TextureFormat::Motion` (id `0x0201`) is selected through `Capabilities` like any other
  format. It is not a GPU texture format: the player entropy-decodes its chunks and queues
  coded frames for a GPU decoder.
- `take_coded_frames(track) -> &[CodedFrame]` returns the frames queued since the last
  call, oldest first, and empties the queue; each must be reconstructed in order, since P
  frames build on the previous one. A keyframe clears the queue, so it never holds more
  than `keyint` frames. Empty for tracks of other formats.
- `video_frame(track)` reports index, time and size as for other formats, with empty
  `data`; `index` changes with every decoded frame.
- Coded frames live in buffers the player recycles: a taken frame's buffer is reused for
  the next decoded frame, so steady playback does not allocate.
- `set_decode_threads(n)`: the most threads entropy-decoding the slices of a motion frame
  (default 8 or the core count, see [Parallel entropy decoding](#parallel-entropy-decoding)).
  The player otherwise owns no threads; files without motion tracks start none.
- The coded frame layout (macroblock records, coefficient blocks) is public and documented,
  so an engine with its own renderer can write the reconstruction shaders itself, as it
  uploads block textures itself today.

### GPU decoder (`praxi-wgpu`)

`VideoRenderer::prepare(device, queue, player, opacity)` takes the queued coded frames of
each motion track (`player` is `&mut` for this) and reconstructs them in order. Per track
it keeps two plane buffers (the frame being built and its reference, swapping roles each
frame), the upload buffers, and the `Rgba8Unorm` texture it draws.

- **Planes** are storage buffers of `u32` words holding 4 pixels each, little-endian, in
  the [`Layout`](#coded-frames) of the CPU reference (Y, Co, Cg, A one after another, rows
  padded to whole macroblocks). Plane widths are multiples of 8 and transform blocks
  start on word boundaries, so every word belongs to one block and one invocation writes
  it whole: no atomics.
- **Per frame**: `write_buffer` of the coded frame's `mbs`, `blocks` and `coeffs` (grown to
  the next power of two when too small) and of a uniform with the block count, keyframe
  flag and dequantization scales, then one command buffer with the passes below and a
  `submit`. Writes land before the next submit's commands, so frames queued together are
  submitted one after another and each sees its own data.
- **Prediction pass**: one invocation per plane word (4 horizontal pixels). It finds the
  macroblock and the quadrant vector of its pixels as `predict_transform_block` does (luma
  and 4:4:4 planes: the 8x8 block's quadrant; 4:2:0 chroma: the 4x4 part's quadrant) and
  computes the normative `predict` of each pixel from the reference planes: the separable
  filter in exact integer arithmetic, coordinates clamped to the plane. Intra macroblocks
  write 128.
- **Residual pass**: one workgroup of 64 invocations per coded block, after the prediction
  pass. It scatters the block's levels into workgroup memory, dequantizes them with the
  plane's scale, runs the two passes of `inverse_transform` (one invocation per output
  value, barriers between), and 16 invocations add the residuals to the predicted words
  and clamp. Blocks do not overlap, so blocks run in any order.
- **Color pass**, after the last queued frame only: one invocation per pixel converts the
  planes to RGBA as `to_rgba` does (YCoCg-R, 9/3/3/1 chroma upsampling with clamped
  coordinates) and stores it in the track's `Rgba8Unorm` storage texture as `value /
  255`. sRGB-encoded bytes: storage textures cannot be sRGB, so the draw shader decodes
  sRGB itself for sRGB tracks.
- Workgroup counts above 65535 (residual pass of a large keyframe) are split over two
  dispatch dimensions.
- All arithmetic is `i32` and stays below 2^31: products of levels and scales are at most
  16383 * 14592, transform sums at most 8 * 89 * 32767.

`prepare` returns how many tracks got a new frame, counting reconstructed motion tracks.

Requirements: compute shaders (`wgpu` downlevel `COMPUTE_SHADERS`) and write-only
`Rgba8Unorm` storage textures: Vulkan, Metal, DX12 and GLES 3.1, not WebGL 2. The device
limits the passes need:

| Limit | Needed | Why |
|---|---|---|
| `max_storage_buffers_per_shader_stage` | 3 | prediction: reference, planes, macroblocks |
| `max_storage_textures_per_shader_stage` | 1 | color pass |
| `max_storage_buffer_binding_size`, `max_buffer_size` | 128 MiB | planes of frames up to 128 MiB (8-bit planes of 4K 4:4:4 with alpha: 33 MB) and the coded frame lists |
| `max_compute_workgroup_storage_size` | 512 | residual pass: two arrays of 64 `i32` |
| `max_compute_invocations_per_workgroup` | 64 | |
| `max_compute_workgroup_size_x` / `_y` / `_z` | 64 / 8 / 1 | |
| `max_compute_workgroups_per_dimension` | 65535 | dispatch splitting assumes it |

`praxi_wgpu::supports_motion(adapter)` checks the downlevel flag and these limits against
the adapter; `praxi_wgpu::required_limits(adapter, base)` raises each of them in `base` to
the needed value when the adapter supports motion, so engines and eframe, which start from
lower limits on GL, get a device that can create the pipelines. `praxi_wgpu::capabilities`
lists `motion` after the block formats, only when the adapter qualifies, so a file with
both block and motion variants plays the block variant.

**Tests** (`praxi-wgpu`): clips encoded with `praxi-encoder` (4:2:0 and 4:4:4, alpha,
sizes not multiple of 16, keyframes and delta frames with every macroblock kind) play
through the player and the GPU decoder; the plane buffers read back must equal
`reconstruct` byte for byte after every frame and the texture must equal `to_rgba`. The
tests run on any adapter `wgpu` finds, including lavapipe, and are skipped with a message
when there is none.

Upload sizes at BC7 quality on the bench input: a 1080p keyframe has 1.3 million nonzero
levels, 5.6 MB of lists; delta frames about 1 MB.

## Encoder

`praxi encode --variant motion` with:

| Option | Default | Meaning |
|---|---|---|
| `--qp <N>` | 22 (*pinned in phase 1*) | Constant quantizer, 0-51; lower is larger and better |
| `--psnr <DB>` | | Instead of a constant `qp`, the largest `qp` per frame whose RGB PSNR reaches `DB`. `--psnr match` uses the PSNR of a BC7 encode of the same frame, for "smaller than BC7, never worse". |
| `--chroma <C>` | `420` | `420` or `444` |
| `--slices <N>` | 16 | Slices per frame |
| `--keyint`, `--quality` | | As for block variants; `--quality` sets the motion search effort |

- Motion search per macroblock: from the best of zero, the predicted vector and the
  vectors of neighbouring macroblocks in the previous frame, square steps at integer
  positions, then half- and quarter-pel refinement; then each 8x8 quadrant searches around
  the macroblock's vector. The cost is SATD (sum of Hadamard-transformed differences) plus
  `lambda` times estimated vector bits. SAD picked poor vectors on fine texture: SATD cut
  the stream by a quarter at equal PSNR.
- Mode decision (skip, inter, split, intra) by rate-distortion cost, RGB squared error
  plus `lambda` times estimated bits.
- 4:2:0 chroma is downsampled by least squares against the decoder's fixed bilinear
  upsampling (a few conjugate gradient steps from the 2x2 average), which raises the
  4:2:0 quality ceiling from 43.1 to 44.1 dB on the bench input.
- The encoder reconstructs each frame exactly as the decoder does and predicts from that,
  so encoder and decoder never drift apart.
- P frames depend on the previous reconstruction, so frames inside a keyframe interval are
  encoded one after another, macroblock rows in parallel; keyframe intervals are encoded in
  parallel with each other.
- Library: `VariantConfig::motion()` with `qp`, `psnr`, `chroma` and `slices` setters.

## Tools

- `praxi bench`: rows for `motion` with size, PSNR (of the reference reconstruction) and
  entropy decode time; GPU time where a GPU is available. [BENCH.md](../spec/BENCH.md)
  gains the fields.
- `praxi frame` (through the reference reconstruction, offline), `praxi info`.
- Viewer and editor: play `motion` tracks through `praxi-wgpu`.
- `motion_timing` example of `praxi-wgpu` (`cargo run --release -p praxi-wgpu --example
  motion_timing -- FILE`): plays the motion track of `FILE` through the player and the GPU
  decoder, waiting for the GPU after every frame, and prints the time of `prepare` plus
  that wait for keyframes and delta frames (mean, median, p95, max over two passes after a
  warm-up pass). `WGPU_ADAPTER_NAME` selects the adapter. It measures wall time, an upper
  bound of the GPU time.

## Format changes

- 0.8: format id `0x0201` `motion` in the stream table, with `block_width = block_height =
  16` (the macroblock) and `block_bytes = 0`; readers stop deriving `frame_size` for it.
  Motion parameters in the reserved video parameter bytes: chroma layout, chroma and alpha
  `qp` offsets. Motion chunks use compression `0`: their payload is already entropy
  coded. The payload layout is in PRAXI.md.
- 0.9: `split` macroblocks, vector prediction at 8x8 granularity, 8- and 4-tap
  interpolation filters. 0.8 motion streams do not decode with 0.9 rules, and readers
  accept one version only.

## Phases

The encoder comes first in any order: nothing can be decoded before something is encoded.

1. **Bitstream and encoder**: `praxi_format::motion` (syntax, entropy coding, reference
   reconstruction; no new dependencies), the encoder in `praxi-encoder`, format 0.8 in
   PRAXI.md, `--variant motion`, bench rows. Measures size against PSNR next to BC7 on the
   bench input, entropy decode time, and the open questions below. **Gate**: continue only
   if `motion` reaches BC7's PSNR at no more than 1/5 of its size with entropy decoding
   under 1 ms per 1080p frame.
2. **GPU decoder**: coded frame queue in the player, compute shaders in `praxi-wgpu`, tests
   that GPU output equals the reference byte for byte (lavapipe locally, and in CI if the
   runner allows), GPU timing.
3. **Integration**: viewer, editor and `cutscene` example on `motion` tracks; engine guide
   and texture-variants pages updated.

## Measurements

`make bench` input (Big Buck Bunny 1080p60, first 180 frames), keyframes every 60 frames,
4:2:0, 4 slices. Decode is entropy decoding per frame, including reading the chunk; one
core unless stated, the machine's core ran at about 3.5 GHz.

First encoder (16x16 vectors, bilinear quarter-pel, SAD search, dense coefficients):

| Variant | MB/s | PSNR | Entropy decode (mean) |
|---|---|---|---|
| BC7 | 55.6 | 41.0 dB | (1.9 ms full decode) |
| motion `qp` 22 | 6.1 | 37.2 dB | 2.8 ms |
| motion `qp` 16 | 15.0 | 40.4 dB | 5.3 ms |
| motion `qp` 13 | 22.2 | 41.7 dB | 6.4 ms |

About 18.5 MB/s at BC7's PSNR, a third of BC7.

Format 0.9 (8- and 4-tap filters, `split` macroblocks, SATD search, least-squares chroma,
sparse coded frames):

| Variant | MB/s | PSNR | Entropy decode mean / p95 / max |
|---|---|---|---|
| motion `qp` 14 | 12.3 | 42.0 dB | 1.75 / 2.02 / 6.1 ms |
| motion `qp` 15 | 10.5 | 41.5 dB | 1.56 / 1.78 / 5.8 ms |
| motion `qp` 16 | 8.9 | 41.1 dB | 1.39 / 1.58 / 5.1 ms |
| motion `qp` 17 | 7.5 | 40.6 dB | 1.25 / 1.43 / 4.4 ms |

At BC7's PSNR (`qp` 16.1) the stream is about 8.7 MB/s, 1/6.4 of BC7: the size goal is
met. Savings at equal PSNR, measured one after another on the first 40 frames: filters
22%, least-squares chroma 8-11%, `split` macroblocks 6.5%, SATD 25%. On those frames
motion needs 9.1 MB/s and x264 (`-preset slow`, no B-frames, same RGB PSNR) 7.7 MB/s.

Entropy decoding misses its goal: 1.4 ms per delta frame, 4-6 ms per keyframe (most
blocks coded). The decoder went from 3.7 to 1.4 ms through sparse coefficient lists,
leaving dequantization to the GPU, and keeping the error paths out of the token loop. The
loop costs about 20 cycles per token; lookahead tables that decode a whole token at once,
and a separate stream for extra bits, measured no faster.

Parallel slices, `qp` 16, 16 slices (8.86 MB/s, the same size as 4 slices to the reported
precision). The machine is a Ryzen AI 9 HX 370: 4 Zen 5 cores up to 5.2 GHz and 8 Zen 5c
cores up to 3.3 GHz, two threads each. Bench decode includes reading the chunk; its max is
the first keyframe, which also starts the workers and grows the buffers. Keyframe and
delta columns decode payloads from memory, 4 passes after a warm-up:

| Threads | Bench mean / p95 / max | Keyframe | Delta frame |
|---|---|---|---|
| 1 | 1.46 / 1.67 / 5.7 ms | 4.28 ms | 1.37 ms |
| 2 | | 2.22 ms | 0.73 ms |
| 4 | 0.53 / 0.69 / 2.8 ms | 1.26 ms | 0.41 ms |
| 8 (default) | 0.37 / 0.45 / 3.2 ms | 1.05 ms | 0.33 ms |
| 12 | | 0.94 ms | 0.31 ms |

Delta frames meet the goal from 4 threads. Keyframes do not: 1.26 ms on 4 threads, about
1 ms on 8. Fixed shares of slices per thread (the first version) reached only 2.4x on 8
threads with 8 slices, as threads on slow or sleeping cores held up the frame, and
spawning threads per frame cost about 0.1 ms; the slice queue on waiting workers fixed
both. Appending finished slices while others still decode hides most of the 0.15-0.25 ms
of copying into the output on keyframes.

## Open questions

- 4x4 transforms and an in-loop deblocking filter at lower bitrates.
- Default `qp` and the `qp` that matches BC7 PSNR on the bench input.
- Keyframes over 1 ms of entropy decoding: the player could decode a keyframe a frame
  ahead so that its extra time does not land in one game frame.

Tried in phase 1 and dropped, each under 1% on the bench input: rate-distortion optimized
quantization, bit estimates from the previous frame's Huffman tables, a lower keyframe
`qp`, other rounding offsets. A negative `chroma_qp_offset` (-2) saves about 1.5%.

## Rejected

- **ETC1S or other supercompressed block formats**: quality ceiling below BC7.
- **Re-encoding decoded frames to BC7 on the GPU**: a second lossy step, and real-time BC7
  encoding at good quality is expensive.
- **B-frames**: smaller files, but reordering delay, two reference frames and a more
  complex player for a modest gain at high quality.
- **Spatial intra prediction**: serializes reconstruction (see [Prediction](#prediction)).
