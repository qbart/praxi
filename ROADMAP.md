# Roadmap

Work in order, top to bottom. Each item gets a spec in `docs/features/` before code
(AGENTS.md); file format changes go to `docs/spec/`. Done items move to the milestones in
[docs/features/overview.md](docs/features/overview.md#milestones).

Done so far (M7 to M10): cue editing after encoding, BC1, manual test targets, decode-ahead
of several frames, `praxi-audio`, Opus audio, the Bevy cue editor, consuming cues at
runtime, typed command-line values and positions in the Bevy cue editor.

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
  always opens the device's default format and resamples; listen again. This is also the
  listening check of Opus audio ([audio.md](docs/features/audio.md#opus)).

## Not scheduled

- **Real hardware**: GPU decode time of the motion codec on discrete and integrated GPUs
  (`motion_timing`), macOS, a phone (ASTC).
- **More block formats**: BC4/BC5 for one- and two-channel content, ASTC 5x5.
- **Motion codec**: 4x4 transforms and deblocking at lower bitrates; `praxi encode --psnr`.
- **Godot integration**.
- **Audio in `praxi-bevy`**: Bevy's audio cannot take `AudioOutput` samples without a
  custom source.
- **CPU fallback** for GPUs without a file's formats (not planned while playback is
  GPU-only).
