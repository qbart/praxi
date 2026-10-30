# Encoder

Encoding is offline. The `praxi-encoder` crate turns RGBA frames into a `.praxi` file; the
`praxi` CLI wraps it and reads any video ffmpeg can decode.

## CLI

```sh
praxi encode input.mp4 -o intro.praxi --variant bc7 --variant astc6x6 \
    --cue 2.5=explosion --cue "4+2.5=subtitle:text=Run!;speaker=Ann"
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
| `--cue <SPEC>` (repeatable) | | Cue `TIME[+DURATION]=TYPE[:KEY=VALUE[;KEY=VALUE...]]`, times in seconds, string values ([cues.md](cues.md)) |
| `--frames <N>` | all | Encode only the first `N` frames |

The CLI runs `ffprobe` for width, height and frame rate, and `ffmpeg` to decode frames as
raw RGBA (`-f rawvideo -pix_fmt rgba`), so alpha is kept when the source has it. Both must be
on `PATH`. All variants become streams of group 0.

It prints per-variant statistics: bytes, MB/s, skipped blocks, keyframes.

`praxi info <FILE>` prints the header and stream table; `--chunks` also lists every chunk.

## Library

```rust
let config = EncoderConfig::new(width, height, fps_num, fps_den)
    .variant(VariantConfig::bc7())
    .variant(VariantConfig::astc(6))
    .cue(2.5, "explosion")
    .cue_stream(CueStreamConfig::new("subtitles").language("en").cue(
        CueConfig::new(4.0, "subtitle").duration(2.5).field("text", Value::String("Run!".into())),
    ));
let mut encoder = Encoder::new(File::create("intro.praxi")?, config)?;
for frame in frames {
    encoder.push_frame(&frame)?; // width * height * 4 bytes, RGBA8, sRGB
}
let (_file, stats) = encoder.finish()?;
```

Frames are encoded on all CPU cores (`std::thread::available_parallelism`).

## Output

- One video stream per variant, group 0, time base `fps_den / fps_num`, frame `n` at pts `n`.
- One cue stream per non-empty cue stream config, time base 1/1000000 (microseconds).
  `cue()` adds to a stream named `cues` without a language. Cues are sorted by time; cues
  with equal times keep their order.
- Chunks are written in time order: the cues due at or before a frame come first, then
  the frame's chunks in variant order. Cues after the last frame are written at the end.
- Header duration is `frame_count * fps_den / fps_num` seconds, raised to the latest cue
  end if a cue ends beyond it.
