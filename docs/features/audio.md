# Audio

praxi files can carry any number of audio tracks next to the video: music, voice, effects,
in any channel layout (mono to 7.1 and beyond). The player hands decoded samples to the
engine's own mixer; praxi does not mix, resample or talk to audio devices.

## Codec

Audio is stored as **PCM**, interleaved, 16-bit integer (`s16`, default) or 32-bit float
(`f32`). Chunks are LZ4 compressed when that makes them smaller (silence, quiet passages).

PCM is a deliberate choice: 48 kHz stereo `s16` is 0.19 MB/s and 5.1 is 0.58 MB/s, under 2%
of the smallest video variant (ASTC 8x8, about 26 MB/s for 1080p60), and decoding costs
nothing beyond a copy. A compressed codec (ADPCM, Opus) may follow if audio-only files
become a use case.

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
  - **sample format**: `s16` or `f32`.
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
player.play();
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

The game clock (`advance(dt)`) is the master clock; audio follows it.

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

Planned: an audio-master mode where the clock follows the samples read from one track.

### Transport

| Action | Audio outputs |
|---|---|
| `play()` | Reads start returning samples. |
| `pause()` | Reads return 0; queued samples are kept and continue on resume. |
| `seek(t)` | Queues are cleared and refilled from `t`: each track decodes from the last chunk starting at or before `t`, dropping frames before `t`. |
| Looping | Seamless: the start of the file follows the end in the same queue. If the audio is shorter than `duration`, silence fills the gap; if longer, the excess is cut. |
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
  `[-1, 1]`.
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
| `--audio-rate <HZ>` | source rate | Resample to this rate. |
| `--audio-format <F>` | `s16` | `s16` or `f32`. |
| `--audio-name <NAME>` | `main` | Track name. |

The CLI takes the first audio stream of the input (`ffprobe -select_streams a:0`). Each
variant is decoded by its own `ffmpeg` process writing `f32le` to a pipe. Channel masks
come from the ffmpeg layout name; unknown layouts get mask `0`. With `--frames N`, audio is
cut at the end of frame `N`. `praxi info` lists audio streams; `--chunks` shows the frame
count of each audio chunk.
