# praxi-encoder

Encoder library for `.praxi` files: RGBA or linear HDR frames in, BC7, BC1, ASTC, BC6H,
ASTC HDR or motion-codec video
out, with audio tracks and cues.

```rust
use praxi_encoder::{AudioTrackConfig, CueConfig, CueStreamConfig, Encoder, EncoderConfig, VariantConfig};

let config = EncoderConfig::new(1920, 1080, 30, 1)
    .variant(VariantConfig::bc7())
    .variant(VariantConfig::astc(6))
    .audio(AudioTrackConfig::from_samples("main", 48_000, 2, samples))
    .cue_stream(CueStreamConfig::new("events").cue(CueConfig::new(2.5, "explosion").field("strength", 0.8)));
let mut encoder = Encoder::new(std::fs::File::create("intro.praxi")?, config)?;
for frame in frames {
    encoder.push_frame(&frame)?; // width * height * 4 bytes of RGBA
}
encoder.finish()?;
```

To encode video files, use the `praxi` command line tool (`cargo install praxi-cli`), which
decodes them with ffmpeg.
