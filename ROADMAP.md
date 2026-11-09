# Roadmap

Work in order, top to bottom. Each item gets a spec in `docs/features/` before code
(AGENTS.md); file format changes go to `docs/spec/`. Done items move to the milestones in
[docs/features/overview.md](docs/features/overview.md#milestones).

Done so far (M7, M8): cue editing after encoding, BC1, manual test targets, decode-ahead
of several frames, `praxi-audio`, the Opus evaluation and the Bevy cue editor.

## 1. Checks on real machines

The code is done and tested; these were never run where they matter.

- [ ] **Editor "Write into file" on Windows**: releases the file, renames over it and
  reopens it at the same position, retrying while antivirus holds the new file
  ([cue-editor.md](docs/features/cue-editor.md#saving)). Checked on Linux under Xvfb and
  type-checked for Windows (`make check-windows`); run the editor and the Bevy cue editor
  on Windows and write cues into a playing file.
- [ ] **Sound on a real device**: the viewer and the editor through `praxi-audio`, with a
  track the device plays natively and one that is resampled and mapped
  ([audio-output.md](docs/features/audio-output.md)). An ALSA file capture on a null sink
  runs much faster than real time and showed nothing; listen on a desktop, or record a
  PipeWire virtual sink and check the tones' frequencies.

## 2. Opus integration (decision needed)

The evaluation is done ([audio.md](docs/features/audio.md#opus-evaluation)): 128 kbps
stereo is 3.4 times smaller than `adpcm`, decoded in 1.2 ms per second of audio by the
pure-Rust `opus1`, bit-close to libopus.

- [ ] Decide whether to add it, and on `opus1` or `opus-pure`.
- [ ] `--audio-format opus`: format code in [PRAXI.md](docs/spec/PRAXI.md), encoder,
  player decode with pre-roll after seeks, tests.

## 3. Consuming cues at runtime

Today a game reads cues as `Event::Cue` / `Event::CueEnd` from `Player::poll_event`
(or `PraxiEvent` messages in Bevy), matches the type string and reads the fields by hand
([cues.md](docs/features/cues.md#player-behaviour)). Scope to be decided; candidates:

- [ ] Typed fields: deserialize a cue's fields into a game struct (serde, behind a
  feature), with errors naming the cue and field.
- [ ] Dispatch by type: register a handler per cue type instead of matching strings.
- [ ] Active cues: query the cues active at the playhead (for UI such as subtitles)
  without tracking `Cue` / `CueEnd` pairs.
- [ ] Lookahead: list the cues starting within the next N ms, so the game can preload
  what they trigger (sounds, assets).
- [ ] Bevy: typed cue messages per cue type in `praxi-bevy`.
- [ ] Positions at runtime: helpers to map a cue's position (`x`, `y` in `0..=1`,
  [cues.md](docs/features/cues.md#screen-position)) to the rectangle the video is drawn in,
  for `praxi-wgpu` and `praxi-bevy`.
- [ ] An example game consuming cues: subtitles, camera shake, and markers drawn at cue
  positions during playback; tests.

## 4. Cue authoring follow-ups

- [ ] **Positions in the Bevy cue editor**: the Position row, Pick, markers on the video and
  dragging, as in `praxi-editor` ([cue-editor.md](docs/features/cue-editor.md#screen-positions)).
- [ ] **Typed values on the command line**: `--cue` and `praxi cues add` store every value
  as a string, so `x=0.5;y=0.3` is not a position. Read numbers and `true` / `false` as
  typed values (with a way to force a string, for example quotes), keeping cue sheets as
  they are.

## Not scheduled

- **CI on Forgejo**: check the workflow since the editor joined it; run `make check`.
- **Real hardware**: GPU decode time of the motion codec on discrete and integrated GPUs
  (`motion_timing`), macOS, a phone (ASTC).
- **More block formats**: BC4/BC5 for one- and two-channel content, ASTC 5x5.
- **Motion codec**: 4x4 transforms and deblocking at lower bitrates; `praxi encode --psnr`.
- **Godot integration**.
- **Audio in `praxi-bevy`**: Bevy's audio cannot take `AudioOutput` samples without a
  custom source.
- **CPU fallback** for GPUs without a file's formats (not planned while playback is
  GPU-only).
