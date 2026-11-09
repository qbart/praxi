# Audio

praxi files can carry any number of audio tracks next to the video: music, voice, effects,
in any channel layout (mono to 7.1 and beyond). The player hands decoded samples to the
engine's own mixer; praxi does not mix, resample or talk to audio devices.

## Codec

Each audio stream has one sample format:

| Format | Bits per sample | 48 kHz stereo | 48 kHz 5.1 | Decode |
|---|---|---|---|---|
| `s16` (default) | 16 | 0.19 MB/s | 0.58 MB/s | conversion to float |
| `f32` | 32 | 0.38 MB/s | 1.15 MB/s | copy |
| `adpcm` | 4.5 | 0.054 MB/s | 0.16 MB/s | one prediction per sample |
| `opus` | about 1.3 at 64 kbps per channel | 0.016 MB/s | 0.048 MB/s | Opus decoder, 1.5 ms per second of stereo |

PCM (`s16`, `f32`) chunks are LZ4 compressed when that makes them smaller (silence, quiet
passages). PCM is the default because it is exact and costs nothing to decode, and its size
hardly matters next to video: stereo `s16` is about 1% of the smallest 1080p60 variant
(ASTC 8x8, 18 MB/s). `adpcm` is for files where audio does weigh: many voice languages,
7.1 variants, small or low-rate video, audio-only files. `opus` is for when it weighs a lot
(long music, many languages, motion-coded video where audio is a large share): about 4
times smaller than `adpcm` at a bitrate near transparency, but lossy in a perceptual way,
with a real decode cost and a decoder that carries state between chunks.

### Opus

`opus` stores Opus (RFC 6716), encoded and decoded in pure Rust by the
[`opus1`](https://crates.io/crates/opus1) crate (0.3, BSD-3-Clause, a port of libopus
1.6). `praxi-format` depends on it for decoding, `praxi-encoder` for encoding; no C
library and no ffmpeg are involved. The byte layout is in
[PRAXI.md](../spec/PRAXI.md#opus-payload).

- **Rates**: Opus codes at 8, 12, 16, 24 or 48 kHz. Other rates are refused by the encoder
  library; the CLI resamples to 48 kHz unless the source already has one of these rates.
- **Channels**: any count up to 255, coded as one multistream: speaker pairs (front, back,
  side, ...) as stereo streams, the other channels as mono streams. The layout follows from
  the channel mask, so channels stay in praxi's order and nothing is reordered. The
  low-frequency channel is coded as a narrowband mono stream.
- **Packets**: 20 ms, `Application::Audio`, variable bitrate, complexity 10. The default
  bitrate is 64 kbps per channel (128 kbps stereo, 384 kbps for 5.1); `bitrate` changes it.
  Chunks hold whole packets: the chunk length is rounded to a multiple of 20 ms (100 ms by
  default, 5 packets).
- **Delay**: the encoder's lookahead (6.5 ms at 48 kHz) is dropped from the first chunk
  (its `skip`), and the input is padded with silence at the end, so decoded audio lines up
  with the source frame for frame and has its exact length.
- **Decoder state**: each chunk continues the decoder state of the one before it. The
  player keeps one decoder per track and resets it where playback jumps: after a seek it
  starts at least 200 ms before the target (pre-roll) and drops what it decodes before
  the target. A fresh decoder's difference from one that ran from the start falls by
  about 3 dB per 10 ms on a steady tone (19 dB after 86 ms, 39 dB after 156 ms), so the
  RFC's minimum of 80 ms is not enough for the first samples to be right; after 200 ms
  they are within 50 dB or more. Turning a track on mid-way
  starts a fresh decoder at the next chunk, whose first tens of milliseconds may sound
  slightly off while it converges. Looping starts a second decoder for the next pass, so
  the end of one pass and the start of the next both decode correctly.
- **Cost**: one 100 ms chunk is decoded at a time on the player's thread; seeking decodes
  two or three extra chunks of pre-roll. Measured below.

Big Buck Bunny's 5.1 soundtrack (10 s, 48 kHz, AAC source), encoded with `praxi encode
--audio-format F --audio-layout stereo --audio-layout 5.1` and played through `Player` on
one core of the Ryzen AI 9 HX 370 (time spent in `advance` for the whole clip, 60 Hz steps):

| Variant | `s16` | `adpcm` | `opus` | `opus` decode | `opus` against `s16` |
|---|---|---|---|---|---|
| Stereo | 1923 KB | 544 KB | 165 KB (132 kbps) | 1.5 ms per second of audio | 34.7 dB SNR |
| 5.1 | 5765 KB | 1606 KB | 490 KB (392 kbps) | 4.3 ms per second of audio | 32.3 dB SNR |

The decoded Opus tracks have exactly the source's 480256 frames. SNR says little about a
perceptual codec; it confirms that the output lines up with the source.
- **Quality**: lossy. Use `s16` for audio that must stay exact, and keep
  `adpcm` for short effects where its lower decode cost matters more than size.

### ADPCM

`adpcm` is praxi's own adaptive differential PCM, built for cheap decoding:

- Each channel is cut into blocks of 16 samples. A block stores one header byte (one of 16
  fixed second-order predictors, and a power-of-two step) and 16 residuals of 4 bits: 9
  bytes for 16 samples, 4.5 bits per sample, 3.6 times smaller than `s16`.
- Decoding a sample is one prediction from the two previous samples, one shift, one add and
  a clamp; no tables beyond the 16 predictors, no state across chunks. 48 kHz stereo
  decodes in 0.18 ms per second of audio (3 µs per 60 Hz game frame), against 0.02 ms for
  `s16`, on the machine of the video measurements.
- Each chunk stores the two decoded samples before it as predictor history, so chunks
  decode on their own (seeking and looping work as with PCM) and the predictor runs across
  chunk boundaries without a seam.
- The encoder rounds to 16 bits, then tries every predictor and step for each block while
  decoding as it goes (so errors do not build up) and keeps the pair with the least squared
  error. It runs at about 90 times real time per stereo track on one core. The predictor
  table comes from k-means over per-block least-squares
  second-order predictors of music, speech and sound effects.
- The predictor table and byte layout are in
  [PRAXI.md](../spec/PRAXI.md#adpcm-payload).

Signal-to-noise ratio against the 16-bit source, measured on local test sounds converted
to 48 kHz stereo, next to ffmpeg's IMA (`adpcm_ima_wav`) and Microsoft (`adpcm_ms`) ADPCM
at about 4 bits per sample:

| Source | `adpcm` | IMA ADPCM | MS ADPCM |
|---|---|---|---|
| Piano rag (60 s) | 52.3 dB | 40.7 dB | 39.8 dB |
| Pop song (17 s) | 49.0 dB | 33.6 dB | 32.7 dB |
| Speech (1.4 s) | 44.8 dB | 32.5 dB | 31.9 dB |
| Synth effect (3 s) | 53.5 dB | 42.8 dB | 45.0 dB |
| Noisy footsteps (2.5 s) | 33.4 dB | 27.2 dB | 26.4 dB |

Noise-like sounds compress worst, as with any predictive codec; keep them in `s16` when
the noise floor matters.

### Opus evaluation

Opus is a perceptual codec: much smaller than ADPCM, at a higher decode cost. This
evaluation led to the `opus` format above, decoded with `opus1`. Big Buck Bunny's 5.1 soundtrack (10 s, 48 kHz, AAC source) was
encoded with ffmpeg's libopus, then decoded with pure-Rust decoders on one core of the
Ryzen AI 9 HX 370 and compared with libopus's own decode of the same packets. ADPCM is
praxi's `adpcm` of the same audio.

| Audio | Size | Decode, per second of audio | Matches libopus |
|---|---|---|---|
| Stereo `s16` | 1536 kbps | none | |
| Stereo `adpcm` | 433 kbps | 0.14 ms | |
| Stereo Opus, 64 kbps CBR | 64 kbps | 0.63 ms (`opus1`), 0.99 ms (`opus-pure`) | 79 dB SNR |
| Stereo Opus, 96 kbps CBR | 96 kbps | 0.85 ms, 1.29 ms | 85 dB |
| Stereo Opus, 128 kbps CBR | 128 kbps | 1.17 ms, 1.49 ms | 96 dB |
| Stereo Opus, 160 kbps CBR | 160 kbps | 1.14 ms, 1.63 ms | 110 dB |
| 5.1 `adpcm` | 1299 kbps | 0.42 ms | |
| 5.1 Opus, 256 kbps VBR | 344 kbps | 4.70 ms, 6.04 ms | 99 dB |

- **Size**: 128 kbps stereo Opus is 3.4 times smaller than `adpcm` and 12 times smaller
  than `s16`. VBR overshoots on this dense soundtrack: `-b:a 64k` gave 93 kbps and 256k
  for 5.1 gave 344 kbps.
- **Decode cost**: about 1.2 ms per second of stereo, 0.02 ms per frame of a 60 Hz game,
  8 times ADPCM's cost but small next to video. The longest packet (20 ms of audio) took
  0.1 ms in stereo and 0.25 ms in 5.1.
- **Decoders**: `opus1` (0.3, a port of libopus 1.x) and `opus-pure` (0.2.3), both
  BSD-3-Clause, decode every packet like libopus, within floating-point rounding (SNR
  above 79 dB against libopus's output). `opus-decoder` (0.1.1) took 250 ms per second of
  audio, matched libopus only to 45 dB, and rejected the 5.1 packets.
- **Quality**: no perceptual metric was run here. SNR against the source says little about
  a perceptual codec. Published listening tests generally put stereo Opus at 128 kbps or
  more near transparent for most listeners.
- **Integration**: `opus1` was chosen for speed and for having an encoder, so praxi does
  not need ffmpeg's libopus. It is a young crate maintained by a small team; its output
  matched libopus's in this evaluation.

## Model

- An **audio track** is one logical sound source, for example `music` or `voice.en`.
- A track can have several **variants** that differ in channel layout, for example stereo
  and 5.1 downmixes of the same mix. Variants are streams with the same `group`; the
  player selects one per group (see [Variant selection](#variant-selection)). Audio groups
  are numbered independently of video groups.
- Each audio stream has:
  - **name**: free text, at most 32 bytes. It means nothing to praxi; the game uses it to
    pick tracks. Variants of one track should share the name.
  - **sample rate** in Hz.
  - **channels** and a **channel mask**: which speakers the channels feed, using the
    WAVE speaker bits (`FL = 0x1`, `FR = 0x2`, `FC = 0x4`, `LFE = 0x8`, `BL = 0x10`, ...).
    Channels are interleaved in increasing bit order, which is also ffmpeg's order. A mask
    of `0` means the layout is unspecified (for example ambisonics) and channels are used
    in stored order.
  - **sample format**: `s16`, `f32`, `adpcm` or `opus`.
  - **preload**: how far ahead of its time an audio chunk is stored in the file (default
    250 ms). See [Interleaving](#interleaving).
- Audio time stamps count sample frames: the time base is `1 / sample_rate`, and a chunk's
  pts is the index of its first frame.
- Chunks hold a run of consecutive frames (default 100 ms). A chunk starts at or after the
  end of the previous chunk of its stream; gaps play as silence.

The byte layout is in [PRAXI.md](../spec/PRAXI.md#audio-parameters-kind--2).

## Interleaving

The engine's audio thread needs samples before they are heard, while the player reads the
file sequentially and only as far as its clock. So audio chunks are stored **earlier** than
their time: a chunk whose first frame plays at `t` is placed in the file where the clock is
`t - preload`. The player decodes it then, which keeps `preload` of audio buffered ahead of
the clock without reading ahead in the file or buffering video.

Formally, chunks are ordered by their *decode time*: `t - preload` (at least 0) for audio,
`t` for video and cues.

## Variant selection

`Capabilities::max_audio_channels(n)` says how many channels the engine's output has
(default: unlimited). For each audio group the player selects the variant with the most
channels at or below `n`; when every variant has more, the one with the fewest channels.
Ties go to the earlier stream.

## Player

```rust
let caps = Capabilities::new([TextureFormat::Bc7]).max_audio_channels(2);
let mut player = Player::open(file, &caps)?;
for (i, track) in player.audio_tracks().iter().enumerate() {
    if track.params.name.starts_with("voice.") && track.params.name != "voice.en" {
        player.set_audio_track_enabled(i, false);
    }
}
let music = player.audio_output(0).unwrap(); // Clone + Send + Sync
engine_mixer.add_stream(music.sample_rate(), music.channels(), move |buf: &mut [f32]| {
    let n = music.read(buf);
    buf[n..].fill(0.0);
});
player.play()?;
// game loop: player.advance(dt)?;
```

- `audio_tracks()` lists the selected variant of each audio group, in group order: stream
  number, group and parameters (name, sample rate, channels, channel mask, sample format).
- `audio_output(track)` returns an `AudioOutput` handle for a track. Handles are cheap to
  clone and can be used from any thread, typically the engine's audio callback. All handles
  of a track share one queue.
- `AudioOutput::read(&mut [f32]) -> usize` fills the buffer with interleaved samples in
  `[-1, 1]` and returns how many samples it wrote, a multiple of `channels()`. It returns
  fewer than asked (possibly 0) when no more audio is decoded yet, and 0 while the player
  is not playing or the track is disabled; the engine plays silence for the rest.
  `sample_rate()`, `channels()` and `channel_mask()` describe the samples. `s16` samples
  are converted as `v / 32768`.
- `set_audio_track_enabled(track, bool)`: disabled tracks are not decoded and their outputs
  return nothing. All tracks start enabled. Enabling a track mid-way resumes it at the clock:
  its output plays silence until the next chunk decodes (at most `preload` plus one chunk).

### Sync

By default the game clock (`advance(dt)`) is the master clock and audio follows it. With
[`Clock::Audio`](#audio-master-clock), the clock follows one audio track instead.

- The player decodes audio chunks when their decode time is reached, so each output queues
  about `preload` of audio ahead of the clock.
- The engine reads at its own pace. Reading ahead of the clock is normal (audio callbacks
  fill buffers that play later), up to what is decoded.
- When an output's read position falls behind the clock by more than the **sync tolerance**
  (default 60 ms, `set_audio_sync_tolerance`), the next read first skips forward to the
  clock. This covers engines that stopped reading for a while and drift between the game
  clock and the audio device clock.
- When the engine stops reading entirely, the queue keeps at most `preload + tolerance +
  100 ms` plus one chunk; older samples are dropped.

### Audio-master clock

The game clock and the audio device's clock drift apart (typically 0.01-0.1%, a few ms
per minute), and the default mode corrects that by skipping audio forward once the drift
exceeds the tolerance, an audible glitch. For long videos with continuous sound it is
better to let the video follow the audio:

```rust
player.set_clock(Clock::Audio { track: 0, latency: Duration::from_millis(20) });
```

- `track` is the master audio track (index into `audio_tracks()`); `latency` is the
  engine's output latency: how long after `AudioOutput::read` returns a sample it is heard
  (device buffer plus mixer delay; for example, cpal's callback-to-playback timestamp
  difference). `set_clock(Clock::Game)` (the default) switches back.
- The *heard position* is the master's read position (frames returned by `read` across all
  its handles) minus `latency`.
- `advance(dt)` still moves the clock by `dt`, then steers it toward the heard position:
  with `err = heard - (clock + dt)`,
  - `|err|` within the sync tolerance: the clock becomes `clock + dt + err * min(1, dt /
    500 ms)`, a smooth correction that absorbs drift and the burstiness of audio callbacks
    (reads come in buffer-sized steps) without visible jumps;
  - `err` above the tolerance (audio ahead, for example after a burst of reads): the clock
    jumps forward to the heard position;
  - `err` below minus the tolerance (audio behind: the engine stopped reading, the device
    stalled, or the track's audio ended before the video): the clock runs on `dt` alone.
    When reads resume, they are behind the clock and skip forward to it as in the default
    mode, after which steering resumes.
- The clock never moves backward, so video frames and cues stay in order.
- While the master track is disabled or out of range, the clock runs on `dt` alone.
- Other audio tracks keep following the clock as in the default mode, so they stay in step
  with the master.
- Pause, seek and looping work as in the default mode. At the start of playback the heard
  position begins at the clock, so the engine should start reading within the tolerance
  (60 ms) of `play()`; a later start loses the first samples, as in the default mode.

### Transport

| Action | Audio outputs |
|---|---|
| `play()` | Reads start returning samples; the chunks due at the clock are queued at once. |
| `pause()` | Reads return 0; queued samples are kept and continue on resume. |
| `seek(t)` | Queues are cleared and refilled from `t`: each track decodes from the last chunk starting at or before `t`, dropping frames before `t`. |
| Looping | Seamless: the start of the file follows the end in the same queue. Within `preload` of the end, the player already queues the next pass's first chunks (those due by then in the next pass's timeline), so reads ahead of the clock do not run dry at the loop point. If the audio is shorter than `duration`, silence fills the gap; if longer, the excess is cut. |
| Finished | Reads return 0 and queues are cleared. |

## Encoder

```rust
let config = EncoderConfig::new(1920, 1080, 60, 1)
    .variant(VariantConfig::bc7())
    .audio(AudioTrackConfig::from_samples("music", 48_000, 2, samples).channel_mask(0x3))
    .audio(AudioTrackConfig::from_reader("music", 48_000, 6, ffmpeg_stdout).channel_mask(0x60f));
```

- `AudioTrackConfig::from_samples(name, rate, channels, Vec<f32>)` or `from_reader(name,
  rate, channels, impl Read + Send)`: interleaved `f32` samples, little-endian bytes for a
  reader (what `ffmpeg -f f32le` writes). The encoder pulls samples from readers as it needs
  them, so long tracks never sit in memory.
- `.format(SampleFormat::F32)` stores floats; the default `S16` rounds and clamps to
  `[-1, 1]`; `Adpcm` rounds and clamps the same way, then encodes
  ([ADPCM](#adpcm)); `Opus` encodes with `opus1` ([Opus](#opus)) and needs one of the
  Opus rates.
- `.bitrate(bits_per_second)`: the Opus bitrate of the whole track (default 64000 per
  channel); ignored by the other formats.
- `.channel_mask(mask)`: speaker bits, `0` (default) for unspecified. The mask must have
  `channels` bits set when non-zero.
- `.group(g)`: tracks with the same name are put in the same group automatically, so they
  become variants; `group` overrides that.
- `EncoderConfig::audio_preload(Duration)` (default 250 ms) and `audio_chunk(Duration)`
  (default 100 ms) apply to every audio stream.
- Audio streams come after the video streams and before the cue streams, in config order.
- The header duration covers the end of the longest audio track.
- Statistics report bytes and frames per audio stream.

## CLI

```sh
praxi encode input.mp4 -o intro.praxi --audio-layout stereo --audio-layout 5.1
praxi encode input.mp4 -o intro.praxi --no-audio
```

| Option | Default | Meaning |
|---|---|---|
| `--no-audio` | | Leave audio out. |
| `--audio-layout <L>` (repeatable) | source layout | Adds a variant downmixed (or upmixed) by ffmpeg to an ffmpeg layout name: `mono`, `stereo`, `2.1`, `3.0`, `quad`, `5.0`, `5.1`, `6.1`, `7.1`, ... |
| `--audio-rate <HZ>` | source rate (`opus`: 48000 unless the source rate is an Opus rate) | Resample to this rate. |
| `--audio-format <F>` | `s16` | `s16`, `f32`, `adpcm` or `opus`. |
| `--audio-bitrate <KBPS>` | 64 per channel | Opus bitrate of each variant. |
| `--audio-name <NAME>` | `main` | Track name. |

The CLI takes the first audio stream of the input (`ffprobe -select_streams a:0`). Each
variant is decoded by its own `ffmpeg` process writing `f32le` to a pipe. Channel masks
come from the ffmpeg layout name: `--audio-layout` takes the standard ffmpeg layouts
(`ffmpeg -layouts`) built from the 18 WAVE speakers, and a source layout outside that list
gets mask `0`. With `--frames N`, audio is
cut at the end of frame `N`. `praxi info` lists audio streams; `--chunks` shows the frame
count of each audio chunk.
