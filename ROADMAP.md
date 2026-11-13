# Roadmap

Work in order, top to bottom. Each item gets a spec in `docs/features/` before code
(AGENTS.md); file format changes go to `docs/spec/`. Done items move to the milestones in
[docs/features/overview.md](docs/features/overview.md#milestones).

Done so far (M7 to M11): cue editing after encoding, BC1, manual test targets, decode-ahead
of several frames, `praxi-audio`, Opus audio, the Bevy cue editor, consuming cues at
runtime, typed command-line values, positions in the Bevy cue editor, Bevy audio and
observable cues.

## 1. Checks on real machines

The code is done and tested; these were never run where they matter. Windows is not a
priority: "Write into file" works on Linux and type-checks for Windows (`make check-windows`),
and is assumed to work there.

- [ ] **Sound on a real device**: the viewer and the editor through `praxi-audio`, with a
  track the device plays natively and one that is resampled and mapped
  ([audio-output.md](docs/features/audio-output.md)). An ALSA file capture on a null sink
  runs much faster than real time and showed nothing; listen on a desktop, or record a
  PipeWire virtual sink and check the tones' frequencies.
  First listen (Linux, PipeWire, RTX 5090): Opus at 48 kHz right, ADPCM at 44.1 kHz pitched
  high, because the device was opened at 44.1 kHz and played at 48 kHz. `praxi-audio` now
  always opens the device's default format and resamples; listen again. This is also the
  listening check of Opus audio ([audio.md](docs/features/audio.md#opus)).
- [ ] **Sound in Bevy**: `cue_game` on the tone files (`PraxiAudio`,
  [bevy.md](docs/features/bevy.md#audio)): the tones play at the same pitch as in the
  viewer, M mutes, and seeking and looping stay smooth. Checked under Xvfb on an ALSA
  device here: the clock follows the audio at real speed with an estimated latency of
  85-96 ms, but nothing was listened to.

## Not scheduled

- **Real hardware**: GPU decode time of the motion codec on discrete and integrated GPUs
  (`motion_timing`), macOS, a phone (ASTC).
- **More block formats**: BC4/BC5 for one- and two-channel content, ASTC 5x5.
- **Motion codec**: 4x4 transforms and deblocking at lower bitrates; `praxi encode --psnr`.
- **Godot integration**.
- **CPU fallback** for GPUs without a file's formats (not planned while playback is
  GPU-only).
