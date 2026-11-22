# Frame export

Decodes one video frame of a `.praxi` file on the CPU and writes it as a PNG, so encoded
quality can be checked by eye without a GPU viewer.

## CLI

```sh
praxi frame intro.praxi --at 2.5 -o frame.png
praxi frame intro.praxi --at 2.5 --format astc6x6 --track 1 -o overlay.png
```

`praxi frame <FILE> -o <OUTPUT>`:

| Option | Default | Meaning |
|---|---|---|
| `--at <SECONDS>` | `0` | Time of the frame |
| `--format <F>` | first variant | Variant to decode: `bc1`, `bc3`, `bc4`, `bc5`, `bc7`, `astc4x4`, `astc5x5`, `astc6x6`, `astc8x8`, `motion` |
| `--track <N>` | `0` | Track (group) index, as listed by the player |
| `-o, --output <PNG>` | | Output file |

## Behaviour

- The file is opened with the player and seeked to `--at` (clamped to the duration), so the
  frame is rebuilt exactly as a game would get it: from the preceding keyframe plus deltas.
  The exported frame is the latest one at or before `--at`.
- Without `--format`, each group uses its first variant in stream order. With `--format`,
  every group must have that variant.
- Blocks are decoded to RGBA8 with the `imgio` decoders. Padding blocks past the frame
  edge are cropped, so the PNG has the stream's width and height.
- Pixel values are written as stored. praxi frames are sRGB encoded, which is what PNG
  viewers expect, so no conversion happens.
- Supported formats: every block format praxi defines (BC1, BC3, BC4, BC5, BC7 and LDR
  ASTC of every block size). BC4 and BC5 are written as stored, as the viewer shows them:
  red, and red plus green, with blue 0 and alpha 255.
- `motion` frames are rebuilt with the CPU reference reconstruction
  (`motion::reconstruct` and `motion::to_rgba`, [motion-codec.md](motion-codec.md#coded-frames))
  from the coded frames the player queued since the preceding keyframe. The GPU decoder
  produces the same pixels.
- The command prints the frame's timestamp, format and size.

## Library

`praxi_encoder::decode_frame(&VideoParams, &[u8]) -> Result<Vec<u8>>` returns
`width * height * 4` bytes of RGBA8 for a frame's block data. It lives in the encoder crate
because it needs `imgio`; the runtime crates stay free of it. The PNG is written with
`imgio::file` too.
