# HDR video

Video whose pixels are brighter than white: sunlight, fire, neon, an explosion. It is
stored in GPU HDR texture formats and decodes to linear floating-point color that a game
renders like any other HDR texture: through its own tonemapper, with bloom, on HDR and SDR
displays alike. Format changes are in [PRAXI.md](../spec/PRAXI.md) (version 0.11).

## Why games want it

- **Cutscenes on HDR displays**: an HDR game cuts to a video that is as bright as the
  game around it, instead of flat SDR next to it.
- **Video as emissive material**: screens, holograms and fire in the world. Values above
  1.0 drive bloom and survive the game's exposure and tonemapping like the rest of the
  scene.

Both need the same thing from praxi: frames that the GPU samples as linear light in a
known scale and gamut, not PQ or HLG code values that every engine would decode itself.

## Model

- An **HDR stream** is a video stream in an HDR texture format (below). Its texels are
  **linear light**: `1.0` is the reference white (SDR white, paper white), values above it
  are brighter, `0` is black. The scale is relative, so a game shows the video at its own
  paper white and the video's 1.0 matches the game's UI white.
- Each HDR stream records its **primaries** (BT.709 or BT.2020), its **reference white**
  in nits (what 1.0 was in the source, 203 by default per ITU-R BT.2408), and its
  **MaxCLL / MaxFALL** in nits (brightest pixel, brightest frame average), for engines that
  tonemap to a display's peak.
- HDR streams are **opaque**: BC6H has no alpha channel. Alpha from the source is dropped
  from HDR variants, as for BC1 ([texture-variants.md](texture-variants.md#formats)); SDR
  variants of the same track keep it.
- An HDR track may also carry **SDR variants** (BC7, ASTC, motion) in the same group,
  tonemapped from the same source at encode time. An engine that does not render HDR picks
  them and pays nothing for HDR.

## Formats

| Format | Id | Block | Bytes per block | Sampled as | GPU support | Produced |
|---|---|---|---|---|---|---|
| BC6H (unsigned) | `0x0006` | 4x4 | 16 | RGB half float | Every GPU with BC7 (desktop, consoles, Apple silicon) | yes |
| ASTC 4x4 HDR | `0x0111` | 4x4 | 16 | RGBA half float | ASTC HDR GPUs (Apple A13+ and M1+, Mali since Valhall; not every Adreno) | yes |
| ASTC 6x6 HDR | `0x0113` | 6x6 | 16 | RGBA half float | as above | yes |

BC6H is the HDR format: it costs the same 16 bytes per block as BC7, so an HDR file is as
large as a BC7 one (about 56 MB/s at 1080p60 before compression gains; the byte-plane and
Huffman stages of [video-codec.md](video-codec.md#compression) apply unchanged).
ASTC HDR serves mobile GPUs that have it, 4x4 at BC6H's size and 6x6 at 44% of it;
elsewhere an SDR ASTC variant is the fallback. A format id names the data's interpretation: ASTC HDR blocks need a texture
created with the HDR channel type, so they are not the LDR ASTC ids.

The motion codec stays SDR for now ([Later](#later)).

## Color

### Linear values

Texel values are linear RGB in the stream's primaries, relative to reference white:
`nits = value * reference_white`. Values are at least 0 and at most 65504 (the largest
half float); the encoder clamps negative values (colors outside the stored gamut), NaN
(to 0) and infinities.

### Primaries

- `bt709`: the sRGB / BT.709 primaries every game engine renders in. The default.
- `bt2020`: wide gamut, for engines that render or output BT.2020. Saturated HDR source
  colors outside BT.709 survive only here.

Engines that render in BT.709 convert BT.2020 streams with the 3x3 matrix in
[PRAXI.md](../spec/PRAXI.md#hdr-color); `praxi-wgpu` does it in its draw shader.

### Light level

`max_cll` is the largest `max(R, G, B)` of any pixel of any frame, `max_fall` the largest
per-frame mean of `max(R, G, B)`, both in nits, rounded up and clamped to 65535
(CTA-861.3). The encoder fills them in when the stream ends. A game tonemapping to a known
display peak can use them to place the video's highlights; most can ignore them.

### Tonemap to SDR

One curve, used wherever praxi itself turns HDR into SDR: the encoder's SDR variants of an
HDR source, `praxi-wgpu` drawing an HDR track into a non-float target, `praxi frame` and
the viewer. It is fixed (normative), so a file's SDR variant and an HDR variant tonemapped
at runtime look alike:

1. Convert to BT.709 primaries if the stream is BT.2020.
2. `Y = 0.2126 R + 0.7152 G + 0.0722 B`. If `Y > 0`, scale R, G and B by
   `Yd / Y` with `Yd = Y * (1 + Y / W²) / (1 + Y)` (extended Reinhard on luminance; `W`, the
   *tonemap white*, is the value that maps to 1.0, default `4.0`).
3. Clamp each channel to `0..1`, sRGB encode, round to 8 bits.

`W = 4.0` at the default reference white puts 812 nits at SDR white. Games with their own
tonemapper do not use this curve; they get linear values.

## Variant selection

Nothing new in the player: a group's variant is still the first format of the engine's
`Capabilities` list that the group provides ([texture-variants.md](texture-variants.md#selection)).
Preference expresses intent:

- An **HDR engine** lists HDR formats first: `[Bc6h, Bc7, ...]` picks BC6H where the file
  has it and BC7 elsewhere.
- An **SDR engine** lists SDR formats first and HDR formats last: `[Bc7, ..., Bc6h]`
  picks the SDR variant when there is one, and still plays an HDR-only file (tonemapped
  when drawn by `praxi-wgpu`).

Typical setups:

- Desktop and consoles: `bc6h` + `bc7` (HDR and its SDR fallback), or `bc6h` alone when
  every target renders HDR.
- Shipping one file everywhere: `bc6h` + `bc7` + `astc6x6`; add `astc6x6-hdr` (or
  `astc4x4-hdr` for higher quality) for HDR on recent mobile GPUs.

## Encoder

### Library

```rust
let config = EncoderConfig::new(width, height, fps_num, fps_den)
    .color(ColorConfig::hdr()                 // reference white 203 nits, BT.709
        .primaries(Primaries::Bt709)
        .tonemap_white(4.0))                  // for the SDR variants
    .variant(VariantConfig::bc6h())
    .variant(VariantConfig::bc7());           // SDR fallback, tonemapped
let mut encoder = Encoder::new(File::create("sunset.praxi")?, config)?;
for frame in frames {
    encoder.push_frame_hdr(&frame)?;          // width * height * 4 f32: linear RGBA, 1.0 = reference white
}
```

- `VariantConfig::bc6h()` and `VariantConfig::astc_hdr(4)` / `astc_hdr(6)` add HDR variants.
- `push_frame_hdr(&[f32])`: linear RGBA in the configured primaries, relative to reference
  white, straight alpha in `0..1`. HDR variants store RGB; SDR variants get the
  [tonemapped](#tonemap-to-sdr) frame with its alpha.
- `push_frame(&[u8])` (RGBA8, sRGB) still works in a file with HDR variants: they get the
  frame sRGB-decoded to linear (at most 1.0), converted to BT.2020 when configured. Each
  frame may come through either call.
- `ColorConfig::hdr()` sets `reference_white` (default 203), `primaries` (default
  `Bt709`) and `tonemap_white` (default 4.0). Without it (`ColorConfig::sdr()`, the
  default), HDR variants are rejected.
- HDR variants never have the alpha flag; `max_cll` and `max_fall` are written by
  `finish()`.

### Encoding paths

From `imgio`, like the SDR formats ([video-codec.md](video-codec.md#encoding-paths)):
`imgio::encode` on the frame as `Rgba32Float`, on all cores.

| Variant | Encoder | `fast` | `medium` | `thorough` |
|---|---|---|---|---|
| BC6H | basisu `fast_encode_bc6h` (unsigned), from RGB halves | `Quality::Default`: 2 endpoint modes, 1 two-subset pattern | `Quality::High`: 4 modes, 8 patterns | `Quality::Best`: 9 modes, 32 patterns, brute-force weights |
| ASTC 4x4 / 6x6 HDR | astcenc 6.0 port, HDR profile (`AstcKind::Float`) | `Quality::Fastest` (preset 0) | `Quality::Fast` (preset 10) | `Quality::Default` (preset 60) |

`--rdo` does not apply: astcenc's LZ RDO is for LDR profiles only.

### Temporal skip

For HDR variants, the skip threshold ([video-codec.md](video-codec.md#temporal-skip))
compares 10-bit PQ code values: each channel becomes `round(PQ(value * reference_white) *
1023)`, so a threshold step is about one 10-bit HDR10 code value at any brightness. The
default threshold (3) and `--skip-threshold` apply unchanged. SDR variants of an HDR source
compare their tonemapped 8-bit pixels, as any SDR variant.

### CLI

```sh
praxi encode sunset_hdr10.mkv -o sunset.praxi --variant bc6h --variant bc7
```

| Option | Default | Meaning |
|---|---|---|
| `--variant bc6h`, `--variant astc4x4-hdr`, `--variant astc6x6-hdr` | | HDR variants (with the existing SDR ones) |
| `--primaries <P>` | `bt709` | `bt709` or `bt2020`: primaries of the HDR variants |
| `--reference-white <NITS>` | `203` | Nits that become 1.0 |
| `--tonemap-white <W>` | `4.0` | `W` of the [SDR tonemap](#tonemap-to-sdr), for SDR variants of an HDR source |
| `--input-transfer <T>` | from `ffprobe` | `pq`, `hlg`, `linear` or `srgb`: override the source's transfer function |
| `--input-primaries <P>` | from `ffprobe` | `bt709` or `bt2020`: override the source's primaries |

The source's transfer function comes from `ffprobe`'s `color_transfer`: `smpte2084` is
`pq`, `arib-std-b67` is `hlg`, `linear` is `linear` (OpenEXR and other float sources);
anything else is `srgb` (today's path). Primaries come from `color_primaries`: `bt2020`, or
BT.709 for `bt709` and unspecified; an HDR source with other primaries is rejected.

Some sources carry no color tags at all: a `.y4m` file has none, so an HDR10 `.y4m` looks
like SDR BT.709 to `ffprobe`. `--input-transfer` and `--input-primaries` say what the
source is:

```sh
praxi encode clip_hdr10.y4m -o clip.praxi --input-transfer pq --input-primaries bt2020 \
    --variant bc6h --variant bc7
```

With either option the CLI also tags the frames for `ffmpeg` with the `setparams` filter,
ahead of `--fps` and `--scale`: `color_trc` and `color_primaries` from the resolved
transfer and primaries, and, when `ffprobe` reports no `color_space` for the source, the
YUV matrix of the primaries (`bt2020nc` for BT.2020, `bt709` for BT.709). Without the
matrix, `ffmpeg` converts untagged YUV to RGB with BT.601, which shifts the hues of a
BT.2020 source. Range is left to `ffmpeg` (narrow, the default for untagged YUV).

For `pq`, `hlg` and `linear` sources, `ffmpeg` decodes to float RGB without changing the
transfer (`-pix_fmt gbrapf32le`, `gbrpf32le` without alpha), and the CLI converts:

- `pq`: SMPTE ST 2084 EOTF to nits, divided by reference white;
- `hlg`: BT.2100 inverse OETF, then the OOTF for a 1000-nit display (system gamma 1.2,
  on luminance), divided by reference white;
- `linear`: taken as is (1.0 is reference white);
- then the primaries matrix to the target primaries.

A `pq` or `hlg` source now also gets SDR variants right: they are tonemapped from linear
light instead of being read as sRGB, which today washes them out.

`--scale` is rejected for `linear` sources: `swscale` clamps float samples to 1.0 when it
resizes (seen with ffmpeg 6.1 on an OpenEXR input), which would cut every highlight. `pq`
and `hlg` samples are code values in `0..1` and scale as before. `--fps` and `--frames`
work for every source.

`praxi encode` prints, per HDR variant, the usual statistics plus `max_cll` and
`max_fall`.

## Player

- `TextureFormat::Bc6h`, `TextureFormat::Astc4x4Hdr` and `TextureFormat::Astc6x6Hdr`;
  `TextureFormat::is_hdr()`.
- `VideoParams::hdr()`, `primaries()` (`Primaries::Bt709` / `Bt2020`),
  `reference_white()`, `max_cll()`, `max_fall()`.
- `VideoFrame` gets `hdr` next to `srgb`. HDR frames are uploaded like any other: rows of
  `blocks_x * 16` bytes, no conversion.

## Engines

### `praxi-wgpu`

| Item | Change |
|---|---|
| `wgpu_format(format, srgb)` | `Bc6h` → `Bc6hRgbUfloat`; `Astc4x4Hdr` and `Astc6x6Hdr` → `Astc { block: B4x4 / B6x6, channel: Hdr }`. |
| `supported_formats(features)` | As today, then `Bc6h` (with `TEXTURE_COMPRESSION_BC`) and `Astc4x4Hdr`, `Astc6x6Hdr` (with `TEXTURE_COMPRESSION_ASTC_HDR`) last: SDR preferred, HDR-only files still play. |
| `supported_formats_hdr(features)` | `Bc6h`, then the BC formats; `Astc4x4Hdr`, `Astc6x6Hdr`, then the ASTC formats. |
| `capabilities_hdr(adapter, only)` | Like `capabilities`, with `supported_formats_hdr`. |
| `required_features(adapter)` | Also `TEXTURE_COMPRESSION_ASTC_HDR` when the adapter has it. |
| `VideoRenderer` | Into a float target (`Rgba16Float`, `Rgba32Float`, `Rg11b10Ufloat`): HDR tracks drawn as linear BT.709 values, BT.2020 streams converted in the shader with negative results clamped to 0. Into any other target: HDR tracks [tonemapped](#tonemap-to-sdr) with `set_tonemap_white(queue, w)` (default 4.0). SDR tracks draw as before (linear values up to 1.0 in float targets). |

### Bevy

`PraxiVideoOptions { hdr: true, .. }` opens the video with `capabilities_hdr` and makes the
video's `Image` `Rgba16Float` instead of `Rgba8UnormSrgb`: linear values, 1.0 at reference
white, ready for a camera with `Hdr` and bloom or as an `emissive_texture`. Mipmaps work the
same on it. With `hdr: false` (default) the image is as today and HDR-only files are
tonemapped into it.

The `video_box` example takes `--hdr`: an `Hdr` camera with `Bloom` and the `AcesFitted`
tonemapper (Bevy's default tonemapper needs the `tonemapping_luts` feature and draws
magenta without it), and the box material takes the video as `emissive_texture` (black
`base_color`), so bright parts of the video glow.

### Godot

`VideoStreamPraxi` gets `hdr: bool` (default `false`); `formats` accepts `bc6h` and
`astc4x4-hdr`:

| Accepted | praxi format | Godot `Image` format |
|---|---|---|
| With the `bptc` feature, like BC7 | BC6H | `FORMAT_BPTC_RGBFU` |
| Only when `formats` names `astc4x4-hdr`: Godot reports no ASTC HDR support (its `RenderingDevice` has no ASTC float data format to query) | ASTC 4x4 HDR | `FORMAT_ASTC_4x4_HDR` |

With `hdr`, HDR formats come before the SDR formats, in `formats` and in the device's list;
without it, after them. Godot draws the
texture without a praxi shader, so it gets the values as stored: files for Godot use
`bt709` (the default), and a project without HDR 2D (`rendering/viewport/hdr_2d`) clips
values above 1.0; such projects should ship an SDR variant. Godot has no ASTC 6x6 image
format, so `astc6x6-hdr` is not accepted.

## Tools

- `praxi info` shows HDR streams as `video BC6H 1920x1080 HDR bt709 white 203 nits
  maxCLL 3249 maxFALL 410`.
- `praxi frame` on an HDR variant: `-o FILE.exr` writes the linear half floats as OpenEXR
  (RGBA with alpha 1, the stream's primaries, through `imgio::file` with its `exr` feature); `-o
  FILE.png` writes the [tonemapped](#tonemap-to-sdr) 8-bit image, with `--tonemap-white`.
  `--format` accepts `bc6h`, `astc4x4-hdr` and `astc6x6-hdr`. The library adds
  `praxi_encoder::decode_frame_hdr(&VideoParams, &[u8]) -> Vec<f32>` (RGBA, alpha 1),
  through `imgio::decode` (`Rgba16Float` output).
- `praxi-viewer --hdr` prefers HDR variants (`capabilities_hdr`). The surface stays sRGB,
  so HDR tracks are tonemapped, `--tonemap-white W` (default 4.0); the window title shows
  `HDR` after the format. Screenshots are tonemapped PNGs.

## Tests

- `praxi-format`: color description round trip; readers reject the HDR flag on an SDR
  format and its absence on an HDR one, the sRGB flag on an HDR stream, unknown primaries,
  and an HDR stream with reference white 0.
- `praxi-encoder`: PQ, HLG and primaries conversions against reference values (PQ code
  0.5807 is 203 nits, so 1.0; HLG 0.75 is 203 nits on the 1000-nit display); the tonemap
  curve at fixed points; BC6H and ASTC 4x4 and 6x6 HDR of a generated HDR frame (a gradient to 40.0
  with colored highlights) decode within a PQ-PSNR floor; `max_cll`/`max_fall` of known
  frames; alpha is dropped from HDR variants and kept in SDR ones.
- `praxi-wgpu`: `hdr_formats_draw_like_the_cpu_decoder` draws BC6H (BT.709 and BT.2020)
  and ASTC HDR where the adapter has it into an `Rgba16Float` target, within half-float
  rounding of the CPU decoder, and into an sRGB target, within 1 per channel of the CPU
  tonemap.
- `praxi-bevy`: `hdr_images_hold_linear_values`: with `hdr` the image is `Rgba16Float`
  and holds values above 1.0 from BC6H; without it the BC7 variant, tonemapped at encode.
- `praxi-godot`: `plays_in_godot` also opens a BC6H + BC7 file: its texture is
  `FORMAT_BPTC_RGBFU` with `hdr` and `FORMAT_BPTC_RGBA` without.
- CLI end to end: a 10-bit clip generated by `ffmpeg` and tagged PQ / BT.2020 encoded as
  `bc6h` + `astc6x6-hdr` + `bc7`; `praxi info` shows the color description; `praxi frame` writes `.exr`
  and `.png`; `--scale` with `--input-transfer linear` fails.

## Measurements

One 1080p frame per input, encoded with `imgio` 0.1.0 on 24 threads (Ryzen AI 9 HX 370)
and decoded with `imgio`. Quality is PSNR of PQ code values (linear 1.0 = 100 nits)
against the source rounded to half floats; it is not comparable with the 8-bit sRGB PSNR
of the SDR formats.

- *sky*: an OpenEXR sky panorama (1024x512) upscaled to 1920x1080, peak 3249 nits, 20% of
  pixels above 100 nits. Smooth.
- *bunny*: a frame of the bench input with highlights expanded (`x / (1 - 0.95x)` on
  linear values, so SDR white becomes 2000 nits), 9% of pixels above 100 nits. Detailed.

| Variant, `--quality` | sky ms | sky dB | bunny ms | bunny dB |
|---|---|---|---|---|
| BC6H `fast` | 6.6 | 55.40 | 12.1 | 43.07 |
| BC6H `medium` | 8.0 | 55.44 | 27.9 | 43.56 |
| BC6H `thorough` | 13.2 | 55.47 | 81.1 | 43.85 |
| ASTC 4x4 HDR `fast` | 81.9 | 58.99 | 78.8 | 45.31 |
| ASTC 4x4 HDR `medium` | 115.4 | 59.25 | 115.5 | 45.87 |
| ASTC 4x4 HDR `thorough` | 182.5 | 60.52 | 183.6 | 46.35 |
| ASTC 6x6 HDR `fast` | 87.1 | 53.64 | 85.1 | 39.91 |
| ASTC 6x6 HDR `medium` | 125.1 | 54.57 | 132.9 | 40.16 |
| ASTC 6x6 HDR `thorough` | 218.4 | 55.16 | 239.9 | 40.49 |
| BC7 `fast` (SDR, for scale) | | | 19 | |

Every HDR variant encodes a 1080p frame in under 0.25 s, so a 10 s clip at 60 fps takes
under a minute per variant at `fast`.

## Later

- **HDR motion codec**: 10-bit PQ planes (`u16`, about twice the plane memory), decoded
  by the color pass into `Rgba16Float` as linear values. This is where HDR files become
  small; BC6H costs what BC7 does.
- **HDR with alpha**: ASTC HDR with alpha (astcenc's HDR profile encodes it), with BC6H
  variants as opaque or a separate alpha track.
- **`praxi bench`** with PQ-PSNR for HDR variants (and the result format in
  [BENCH.md](../spec/BENCH.md)).
- **HDR output in the viewer**: an HDR swapchain where `wgpu` offers one.
- **`praxi-ffi`** exposes the color description from the start (roadmap).
