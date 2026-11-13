# Roadmap

Work in order, top to bottom. Each item gets a spec in `docs/features/` before code
(AGENTS.md); file format changes go to `docs/spec/`. Done items move to the milestones in
[docs/features/overview.md](docs/features/overview.md#milestones).

Done so far (M7 to M11): cue editing after encoding, BC1, manual test targets, decode-ahead
of several frames, `praxi-audio`, Opus audio, the Bevy cue editor, consuming cues at
runtime, typed command-line values, positions in the Bevy cue editor, Bevy audio and
observable cues. Sound through `praxi-audio` was listened to on a desktop (Linux, PipeWire):
Opus at 48 kHz and ADPCM at 44.1 kHz resampled sound the same.

## 1. Checks on real machines

The code is done and tested; these were never run where they matter. Windows is not a
priority: "Write into file" works on Linux and type-checks for Windows (`make check-windows`),
and is assumed to work there.

- [ ] **Sound in Bevy**: `cue_game` on the tone files (`PraxiAudio`,
  [bevy.md](docs/features/bevy.md#audio)): the tones play at the same pitch as in the
  viewer, M mutes, and seeking and looping stay smooth. Checked under Xvfb on an ALSA
  device here: the clock follows the audio at real speed with an estimated latency of
  85-96 ms. On a desktop the cues fire; the sound, M and seeking were not reported yet.

## Not scheduled

- **Real hardware**: GPU decode time of the motion codec on discrete and integrated GPUs
  (`motion_timing`), macOS, a phone (ASTC).
- **More block formats**: BC4/BC5 for one- and two-channel content, ASTC 5x5.
- **Motion codec**: 4x4 transforms and deblocking at lower bitrates; `praxi encode --psnr`.
- **Godot integration**.
- **CPU fallback** for GPUs without a file's formats (not planned while playback is
  GPU-only).
