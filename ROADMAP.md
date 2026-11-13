# Roadmap

Work in order, top to bottom. Each item gets a spec in `docs/features/` before code
(AGENTS.md); file format changes go to `docs/spec/`. Done items move to the milestones in
[docs/features/overview.md](docs/features/overview.md#milestones).

Done so far (M7 to M11): cue editing after encoding, BC1, manual test targets, decode-ahead
of several frames, `praxi-audio`, Opus audio, the Bevy cue editor, consuming cues at
runtime, typed command-line values, positions in the Bevy cue editor, Bevy audio and
observable cues, the Godot runtime. Sound through `praxi-audio` was listened to on a desktop (Linux, PipeWire):
Opus at 48 kHz and ADPCM at 44.1 kHz resampled sound the same. In Bevy, `cue_game` on the tone files plays in
step with the picture (estimated latency 92 ms), mutes, seeks and loops smoothly. Windows is
not a priority: "Write cues into .praxi" works on Linux and type-checks for Windows
(`make check-windows`), and is assumed to work there.

## 1. Godot runtime: what is left

`praxi-godot` plays files in `VideoStreamPlayer` with audio, cues as signals and an
example project ([godot.md](docs/features/godot.md)). Checked in headless Godot 4.7.2 (test)
and under Xvfb on lavapipe (example). Still open:

- [ ] **Run it on a desktop**: the example (`make godot-example`) in a Godot editor on
  real hardware, with sound; and the release workflow's macOS and Windows libraries.
- [ ] **Audio clock**: the video follows the game clock; following what is heard needs
  Godot's output latency and when `VideoStreamPlayer` mixes.
- [ ] **Motion codec**: GPU decoding through Godot's `RenderingDevice` compute shaders,
  ported from `praxi-wgpu`; until then, files need a block-format variant.
- [ ] **More video tracks**: only track 0 is shown.

## 2. C API and Unity

A C API for engines that cannot call Rust, shaped by its first user, a Unity plugin. Specs
in `docs/features/` first.

- [ ] **`praxi-ffi`**: a C library and header (`praxi.h`) over the player: open from a
  path or from read/seek callbacks, advance, events, frames, audio outputs readable from
  the audio thread, cues with their fields. Explicit ownership (every handle has a free
  function), errors as codes plus a message, no Rust panics across the boundary.
- [ ] **Unity plugin**: a C# package over `praxi-ffi` (P/Invoke): frames into
  `Texture2D` in their compressed format, audio through `OnAudioFilterRead`, cues as C#
  events. Built for Linux, macOS and Windows by the release workflow.
- [ ] Unreal and custom C++ engines use the same header; a godot-cpp runtime could too.

## 3. Seeking cost

A seek decodes every frame from the preceding keyframe to the target: with the default
`--keyint 60`, up to 59 extra frames, an estimated 50-100 ms for a 1080p motion-codec seek
([motion-codec.md](docs/features/motion-codec.md#coded-frames),
[video-codec.md](docs/features/video-codec.md#keyframe-interval)). Today the only control is
`--keyint N` (frames between keyframes, for every variant), plus the motion encoder's
keyframes at scene cuts. Each needs a spec in `docs/features/` first.

- [ ] **More keyframes where they matter, from encoder parameters**:
  - `--keyint` in seconds as well as frames (`--keyint 0.5s`), so the seek cost does not
    change with the frame rate;
  - `--keyframe-at TIME` (repeatable): keyframes at chosen times, such as chapter starts
    and loop points, so seeking there decodes one frame;
  - keyframes at cue starts (`--keyframes-at-cues`, optionally per cue stream), so a game
    that seeks to its own markers seeks instantly.
- [ ] **Measure it**: `praxi bench` and `praxi info` report the worst and mean seek cost
  (frames and milliseconds to decode) per variant, next to the size a shorter `keyint`
  costs, so the trade-off is visible when choosing parameters.
- [ ] **Fast seek in the player**: a seek mode that lands on the nearest keyframe instead of
  the exact frame (`SeekMode::Keyframe`), for scrubbing and skipping, where precision
  matters less than speed.

## Not scheduled

- **Real hardware**: GPU decode time of the motion codec on discrete and integrated GPUs
  (`motion_timing`), macOS, a phone (ASTC).
- **More block formats**: BC4/BC5 for one- and two-channel content, ASTC 5x5.
- **Motion codec**: 4x4 transforms and deblocking at lower bitrates; `praxi encode --psnr`.
- **CPU fallback** for GPUs without a file's formats (not planned while playback is
  GPU-only).
