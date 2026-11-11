# Roadmap

Work in order, top to bottom. Each item gets a spec in `docs/features/` before code
(AGENTS.md); file format changes go to `docs/spec/`. Done items move to the milestones in
[docs/features/overview.md](docs/features/overview.md#milestones).

Done so far (M7 to M9): cue editing after encoding, BC1, manual test targets, decode-ahead
of several frames, `praxi-audio`, Opus audio, the Bevy cue editor and consuming cues at
runtime.

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
  First listen (Linux, PipeWire, RTX 5090): Opus at 48 kHz right, ADPCM at 44.1 kHz pitched
  high, because the device was opened at 44.1 kHz and played at 48 kHz. `praxi-audio` now
  always opens the device's default format and resamples; listen again.

## 2. Opus

- [x] `opus1` chosen: pure Rust, with an encoder, so praxi encodes Opus itself.
- [x] `--audio-format opus` ([audio.md](docs/features/audio.md#opus)): format 0.10 with
  sample format `4` ([PRAXI.md](docs/spec/PRAXI.md#opus-payload)), encoder, player decode
  with 200 ms pre-roll after seeks and seamless loops, tests. Stereo is 3.3 times smaller
  than `adpcm`.
- [ ] Listen to it on a real device, with the sound check above.

## 3. Consuming cues at runtime

Done ([cue-runtime.md](docs/features/cue-runtime.md)).

- [x] Typed fields: `CueEvent::parse::<T>()` with serde (feature `serde`), errors naming
  the cue and field.
- [x] Dispatch by type: `parse` into an enum whose variants are the cue types, then
  `match`; unknown types are a distinct error or a `#[serde(other)]` variant.
- [x] Active cues: `Player::active_cues()`. The player now reads every cue at open.
- [x] Lookahead: `Player::upcoming_cues(within)`, across the loop seam when looping.
- [x] Bevy: `PraxiCuePlugin<T>` sends `PraxiCue<T>` messages.
- [x] Positions at runtime: `praxi_wgpu::Rect::point` / `position`,
  `praxi_bevy::position_on_sprite` / `position_in_rect`.
- [x] An example game: `cue_game` in `praxi-bevy` (`make cue-game`) with subtitles, a
  speech bubble, camera shake, markers and sounds announced ahead; checked under Xvfb.

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
