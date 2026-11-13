# Roadmap

Work in order, top to bottom. Each item gets a spec in `docs/features/` before code
(AGENTS.md); file format changes go to `docs/spec/`. Done items move to the milestones in
[docs/features/overview.md](docs/features/overview.md#milestones).

Done so far (M7 to M11): cue editing after encoding, BC1, manual test targets, decode-ahead
of several frames, `praxi-audio`, Opus audio, the Bevy cue editor, consuming cues at
runtime, typed command-line values, positions in the Bevy cue editor, Bevy audio and
observable cues. Sound through `praxi-audio` was listened to on a desktop (Linux, PipeWire):
Opus at 48 kHz and ADPCM at 44.1 kHz resampled sound the same. In Bevy, `cue_game` on the tone files plays in
step with the picture (estimated latency 92 ms), mutes, seeks and loops smoothly. Windows is
not a priority: "Write cues into .praxi" works on Linux and type-checks for Windows
(`make check-windows`), and is assumed to work there.

## 1. Godot runtime

Play praxi files in Godot 4 the way `praxi-bevy` does in Bevy: a GDExtension crate
(`praxi-godot`, on [godot-rust](https://github.com/godot-rust/gdext)) that Godot games
add without writing Rust. Spec in `docs/features/godot.md` first.

- [ ] **`VideoStreamPraxi`**: a `VideoStream` / `VideoStreamPlayback` pair, so the built-in
  `VideoStreamPlayer` node plays `.praxi` files: play, pause, seek, loop, `stream_position`,
  and a resource loader for the `.praxi` extension so files can be dropped into the editor.
- [ ] **Frames as compressed textures**: each new frame uploaded as an `Image` in its block
  format (`FORMAT_BPTC_RGBA` for BC7, `FORMAT_DXT1` for BC1, `FORMAT_ASTC_4x4` and
  `FORMAT_ASTC_8x8`) into a `Texture2D` updated in place, chosen by what the rendering
  device supports. Godot has no ASTC 6x6 image format, so files need another variant.
- [ ] **Audio**: the player's audio tracks mixed through `VideoStreamPlayback`'s audio
  (`mix_audio`), with the video clock following what is heard, as in Bevy.
- [ ] **Cues as signals**: `cue(id, type, fields: Dictionary, time, duration, position)` and
  `cue_ended(id)` signals on a `PraxiCues` node next to the player, plus `active_cues()`
  and `upcoming_cues(within)`, so GDScript reacts to cues as they happen.
- [ ] **Example project**: a Godot project like `cue_game` (subtitles, markers, camera
  shake from cues), built for Linux, macOS and Windows by the release workflow along with
  the extension libraries.
- [ ] **Motion codec**: GPU decoding through Godot's `RenderingDevice` compute shaders,
  ported from `praxi-wgpu`; until then, files need a block-format variant.

## 2. Seeking cost

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
