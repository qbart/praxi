# Encoder

Encoding is offline. The `praxi-encoder` crate turns RGBA frames into a `.praxi` file; the
`praxi` CLI wraps it and reads any video ffmpeg can decode.

## CLI

```sh
praxi encode input.mp4 -o intro.praxi --variant bc7 --variant astc6x6 \
    --cue 2.5=explosion --cue "4+2.5=light.flash:color=red"
praxi info intro.praxi
```

`praxi encode <INPUT> -o <OUTPUT>`:

| Option | Default | Meaning |
|---|---|---|
| `--variant <V>` (repeatable) | `bc7` | `bc7`, `astc4x4`, `astc6x6`, `astc8x8` |
| `--quality <Q>` | `fast` | `fast`, `medium`, `thorough`: UASTC level / astcenc preset |
| `--rdo <LAMBDA>` | `0` (off) | UASTC RDO strength for `bc7`; higher is smaller and lower quality |
| `--keyint <N>` | `60` | Frames between keyframes |
| `--skip-threshold <T>` | `3` | Temporal skip threshold, see [video-codec.md](video-codec.md) |
| `--plane-min-saving <PCT>` | `8` | Use byte planes for a chunk, and Huffman for a plane, only when they save at least this many percent over the faster form, see [video-codec.md](video-codec.md#decode-cost-and-writer-policy). `0`: always the smallest form; `100`: never byte planes. |
| `--cue <SPEC>` (repeatable) | | Cue `TIME[+DURATION]=TYPE[:KEY=VALUE[;KEY=VALUE...]]`, times in seconds, string values ([cues.md](cues.md)) |
| `--frames <N>` | all | Encode only the first `N` frames |
| `--no-audio`, `--audio-layout`, `--audio-rate`, `--audio-format`, `--audio-name` | | Audio options, see [audio.md](audio.md#cli) |

The CLI runs `ffprobe` for width, height and frame rate, and `ffmpeg` to decode frames as
raw RGBA (`-f rawvideo -pix_fmt rgba`), so alpha is kept when the source has it. Both must be
on `PATH`. All variants become streams of group 0. The first audio stream of the input is
encoded too, through a separate `ffmpeg` process per audio variant.

It prints per-variant statistics: bytes, MB/s, skipped blocks, keyframes, and bytes and
MB/s per audio stream.

`praxi info <FILE>` prints the header and stream table; `--chunks` also lists every chunk.

`praxi frame <FILE>` writes one decoded frame as a PNG; see
[frame-export.md](frame-export.md).

## Library

```rust
let config = EncoderConfig::new(width, height, fps_num, fps_den)
    .variant(VariantConfig::bc7())
    .variant(VariantConfig::astc(6))
    .cue(2.5, "explosion")
    .cue_stream(CueStreamConfig::new("gameplay").cue(
        CueConfig::new(4.0, "camera.shake").duration(2.5).field("strength", 0.8),
    ));
let mut encoder = Encoder::new(File::create("intro.praxi")?, config)?;
for frame in frames {
    encoder.push_frame(&frame)?; // width * height * 4 bytes, RGBA8, sRGB
}
let (_file, stats) = encoder.finish()?;
```

`EncoderConfig::plane_min_saving(percent)` (default 8) sets the byte-plane policy, like
`--plane-min-saving`. Frames are encoded on all CPU cores (`std::thread::available_parallelism`). Audio tracks
are added with `EncoderConfig::audio` ([audio.md](audio.md#encoder)).

## Output

- One video stream per variant, group 0, time base `fps_den / fps_num`, frame `n` at pts `n`.
- One audio stream per audio track config, time base `1 / sample_rate`, chunks of
  `audio_chunk` frames from pts 0, in groups by name ([audio.md](audio.md#encoder)).
- One cue stream per non-empty cue stream config, time base 1/1000000 (microseconds).
  `cue()` adds to the stream named `cues`. Cues are sorted by time; cues
  with equal times keep their order.
- Chunks are written in decode-time order: the cues and audio chunks due at or before a
  frame come first (cues first on ties), then the frame's chunks in variant order. What is
  due after the last frame is written at the end.
- Header duration is `frame_count * fps_den / fps_num` seconds, raised to the latest cue
  end or audio end beyond it.
