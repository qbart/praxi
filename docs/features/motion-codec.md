# Motion codec

A second video codec next to the block textures of [video-codec.md](video-codec.md):
transform coding with motion compensation, the family of H.264, decoded mostly on the
GPU. It trades a little decode work for files several times smaller than BC7 at the
same or better quality.

Status: draft. Playback decodes on the GPU only; there is no CPU playback path. The exact
byte layout goes into [PRAXI.md](../spec/PRAXI.md) (format 0.8) with the phase 1
implementation, and the constants marked *pinned in phase 1* may change until then.

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
- **Decode**: at 1080p, at most 1 ms of CPU per frame on one core (entropy decoding) and
  at most 1 ms of GPU per frame on an integrated GPU. Exact output: the GPU decoder
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
| File size (bench input) | 56 MB/s | goal: under 11 MB/s at equal PSNR |
| CPU per 1080p frame | 0.6-1.5 ms (decompress) | goal: under 1 ms (entropy decode) |
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
  - `intra`: coefficients only, as in keyframes; the prediction is 128.
- **Motion vectors**: one per macroblock, quarter-pel for luma (chroma uses the same vector
  at its own resolution, eighth-pel in 4:2:0). Prediction samples are bilinear with integer
  weights and rounding (normative); positions outside the reference frame clamp to its
  edge, so vectors may point outside. Vectors are predicted from the median of the left,
  upper and upper-right macroblocks of the slice; only the difference is coded.
  8x8 vectors (four per macroblock) are an open question for phase 1.

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
  pattern, and motion vector classes. The decoder reuses the table-driven Huffman decoder
  of byte planes. Raw extra bits go to their own stream, read without table lookups.
- Keyframes carry all tables; a P frame carries only the tables it changes and inherits the
  rest from the previous frame (decoding starts at a keyframe anyway).
- A frame is split into **slices** of whole macroblock rows. Predictions (DC, motion
  vectors) do not cross slices and each slice has its own bit streams, so an engine can
  entropy-decode slices on several threads; the player itself uses one.
- Context-adaptive arithmetic coding (rANS with adaptive models) would save an estimated
  10-15%; phase 1 measures it against Huffman, keeping Huffman unless the saving is large
  for the decode time it costs.

## Decoding

Decoding has two stages:

1. **Entropy decoding** (CPU, in the player): the chunk becomes a **coded frame**: per
   macroblock its type and motion vector, and the list of coded transform blocks with their
   dequantized coefficients (DC prediction resolved). This is the serial part, goal under
   1 ms per 1080p frame.
2. **Reconstruction** (GPU, in `praxi-wgpu` or the engine's own shaders): per transform
   block, motion-compensated prediction (or 128), inverse transform, add, clamp, into the
   frame's planes; then color conversion to RGBA for display. Fully parallel over blocks.

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
- `take_coded_frames(track)` returns the frames queued since the last call, oldest first;
  each must be reconstructed in order, since P frames build on the previous one. A keyframe
  clears the queue, so it never holds more than `keyint` frames. `video_frame(track)`
  reports index, time and size as for other formats, with empty `data`.
- Coded frames live in buffers the player recycles; it allocates only when the queue grows
  past its previous high-water mark, keeping the no-allocation-per-frame property in steady
  playback.
- The coded frame layout (macroblock records, coefficient blocks) is public and documented,
  so an engine with its own renderer can write the reconstruction shaders itself, as it
  uploads block textures itself today.

### GPU decoder (`praxi-wgpu`)

`VideoRenderer::prepare` takes the queued coded frames of motion tracks and, per frame:

1. Uploads the macroblock buffer (type and vector per macroblock, 8 bytes each: 65 KB at
   1080p) and the coded blocks (block index plus 64 coefficients of 16 bits).
2. Dispatches one workgroup of 64 threads per 8x8 transform block: prediction from the
   reference planes, inverse transform in workgroup memory, add and clamp, written to the
   current planes. Planes are storage buffers; the two plane sets swap roles each frame.
3. After the last queued frame only: a color pass from the planes into the track's
   `Rgba8Unorm` texture (sRGB-encoded bytes; storage textures cannot be sRGB, so the draw
   shader decodes sRGB itself).

Requirements: compute shaders and storage buffers (`wgpu` downlevel `COMPUTE_SHADERS`):
Vulkan, Metal, DX12 and GLES 3.1, not WebGL 2. `praxi_wgpu::capabilities` lists `motion`
only when the adapter qualifies.

Upload sizes: a 1080p keyframe at high quality codes most of its 48,960 transform blocks,
about 6 MB of coefficients; P frames code few blocks. Sparse coefficient lists (position
and value) would cut uploads further and are measured in phase 2.

## Encoder

`praxi encode --variant motion` with:

| Option | Default | Meaning |
|---|---|---|
| `--qp <N>` | 22 (*pinned in phase 1*) | Constant quantizer, 0-51; lower is larger and better |
| `--psnr <DB>` | | Instead of a constant `qp`, the largest `qp` per frame whose RGB PSNR reaches `DB`. `--psnr match` uses the PSNR of a BC7 encode of the same frame, for "smaller than BC7, never worse". |
| `--chroma <C>` | `420` | `420` or `444` |
| `--slices <N>` | 4 | Slices per frame |
| `--keyint`, `--quality` | | As for block variants; `--quality` sets the motion search effort |

- Motion search per macroblock: around the predicted vector and zero, a diamond search at
  integer positions, then half- and quarter-pel refinement; cost is SAD plus `lambda` times
  estimated bits. Mode decision (skip, inter, intra) by the same cost.
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

## Format changes (0.8)

- Format id `0x0201` `motion` in the stream table, with `block_width = block_height = 16`
  (the macroblock) and `block_bytes = 0`; readers stop deriving `frame_size` for it.
- Motion parameters in the reserved video parameter bytes: chroma layout, chroma and alpha
  `qp` offsets, slice count.
- Motion chunks use compression `0`: their payload is already entropy coded.
- The payload layout (frame header, tables, slices, macroblock and coefficient syntax) is
  specified in PRAXI.md with the phase 1 implementation.

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

## Open questions for phase 1

- 8x8 motion vectors and 4x4 transforms: gain against syntax and GPU cost.
- Huffman against adaptive rANS: size against entropy decode time.
- An in-loop deblocking filter at lower bitrates.
- Default `qp` and the `qp` that matches BC7 PSNR on the bench input.
- Slice count: cost of resetting predictions against parallel entropy decoding.

## Rejected

- **ETC1S or other supercompressed block formats**: quality ceiling below BC7.
- **Re-encoding decoded frames to BC7 on the GPU**: a second lossy step, and real-time BC7
  encoding at good quality is expensive.
- **B-frames**: smaller files, but reordering delay, two reference frames and a more
  complex player for a modest gain at high quality.
- **Spatial intra prediction**: serializes reconstruction (see [Prediction](#prediction)).
