# Roadmap

Work in order, top to bottom. Each item gets a spec in `docs/features/` before code
(AGENTS.md); file format changes go to `docs/spec/`. Done items move to the milestones in
[docs/features/overview.md](docs/features/overview.md#milestones).

## 1. Cues after encoding

Cues can already be edited after the video is encoded, for every texture format, motion
included: `praxi cues export` / `import` ([cue-editing.md](docs/features/cue-editing.md)),
live preview in the viewer ([cue-preview.md](docs/features/cue-preview.md)) and the
timeline editor ([cue-editor.md](docs/features/cue-editor.md)). Video and audio chunks are
copied byte for byte, nothing is re-encoded. Missing:

- [x] **`praxi cues add` / `praxi cues remove`**: change the cues of an encoded file from
  the command line with the encoder's `--cue` syntax, without writing a sheet by hand.
- [x] **Editor "Write into file" on Windows**: the editor renames over the file its player
  has open, which Windows refuses. Release the file before the rename, reopen it after and
  keep the position. Not yet run on Windows.
- [x] **Tests** for both, plus a cue round trip through `cues import` on a motion file.
- [ ] Editor close dialog under a window manager (needs a desktop: manual check).

## 2. BC1

Half the size of BC7 for opaque video
([texture-variants.md](docs/features/texture-variants.md#formats)).

- [x] Format code and stream parameters ([PRAXI.md](docs/spec/PRAXI.md)).
- [x] Encoder variant `bc1` (`--variant bc1`), temporal skip and byte planes as for BC7.
- [x] Player selection, `praxi-wgpu` upload (`Bc1RgbaUnormSrgb`), viewer, `praxi frame`.
- [x] `praxi bench` row and tests: 24.0 MB/s at 36.2 dB against BC7's 55.6 MB/s at
  41.0 dB.

## 3. Tests without CI

CI on Forgejo is skipped for now; every check must run by hand instead.

- [x] `make test` runs every test of the workspace, GPU tests included (they skip with a
  message when no adapter is available); `make check` adds clippy; `make test-strict`
  fails instead of skipping.
- [x] Tests for the paths that only examples cover today: the CLI end to end through
  ffmpeg (audio with `--fps` included), block formats drawn on the GPU.
- [x] [testing.md](docs/features/testing.md): what each target runs, what needs ffmpeg, a
  GPU or the sample video, and the manual checks.

## 4. Decode-ahead of several frames

Motion decode-ahead works on one frame
([motion-codec.md](docs/features/motion-codec.md#decode-ahead)), so a 30 Hz game playing
60 fps video only gets half the benefit.

- [x] Read the headers of the next frames in order and decode their slices at once, each
  frame with its own tables and output (up to 4 frames).
- [x] Measure at 30 Hz against 60 Hz; tests against normal decodes: a 30 Hz game pays
  0.22 ms per `advance` instead of 1.2-1.7 ms, a 60 Hz game 0.15 ms as before.

## 5. Audio

- [ ] **`praxi-audio` crate**: the editor includes `crates/praxi-viewer/src/audio.rs` by
  path. Move the output into a shared crate, and resample to the device rate so the viewer
  and the editor play tracks the device does not support natively (today they skip them).
- [ ] **Opus evaluation**: smaller than ADPCM at a higher decode cost
  ([audio.md](docs/features/audio.md)). Measure size and decode time; only with a
  pure-Rust decoder.

## 6. Bevy + egui cue editor

- [ ] `praxi-bevy`: a Bevy plugin on top of `praxi-wgpu` (video as a Bevy image, events
  as Bevy messages). It needs a Bevy release on the same `wgpu` version.
- [ ] A simple cue editor in Bevy with `bevy_egui`: a video, a cue list and a timeline,
  reusing the editing model of `praxi-editor` and cue preview through
  `Player::replace_cues`.

## Not scheduled

- **CI on Forgejo**: check the workflow since the editor joined it.
- **Real hardware**: GPU decode time of the motion codec on discrete and integrated GPUs
  (`motion_timing`), macOS, a phone (ASTC).
- **More block formats**: BC4/BC5 for one- and two-channel content, ASTC 5x5.
- **Motion codec**: 4x4 transforms and deblocking at lower bitrates; `praxi encode --psnr`.
- **Godot integration**.
- **CPU fallback** for GPUs without a file's formats (not planned while playback is
  GPU-only).
