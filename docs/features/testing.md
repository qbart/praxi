# Testing

There is no CI for now ([ROADMAP.md](../../ROADMAP.md)); every check runs by hand from the
repository root.

## Make targets

| Target | Runs | Needs |
|---|---|---|
| `make test` | `cargo test --workspace`: unit tests, integration tests, doc tests; builds the examples | Rust. ffmpeg and a GPU adapter for some tests (below), which skip without them |
| `make check` | `cargo clippy` with `-D warnings` over the workspace, then again over the viewer and the editor without default features (no audio device), then `make test` | Rust with clippy |
| `make check-windows` | `cargo check` of the workspace for `x86_64-pc-windows-gnu`, so code that only Windows compiles differently (cpal's WASAPI backend, winit) is checked from Linux | `rustup target add x86_64-pc-windows-gnu`; no linker, nothing runs |
| `make test-strict` | `make test` with `PRAXI_TEST_STRICT=1`: tests that would skip fail instead | ffmpeg, a GPU adapter with compute shaders and BC or ASTC |
| `make bench` | `praxi bench` on the sample video, checked against [bench/baseline.json](../../bench/baseline.json) ([benchmarks.md](benchmarks.md)) | ffmpeg, the sample video (`make samples` downloads it) |
| `make examples` | Encodes part of the sample and runs the `game_loop` and `decode_thread` examples on it | ffmpeg, the sample video |

Code is not formatted with rustfmt; `make check` does not run `cargo fmt`. The first
`make test` also builds Bevy and `bevy_egui` for `praxi-bevy` and its example, which
takes a few minutes.

## Tests that skip

A test that cannot run on the machine prints `skipped: <reason>` to standard error and
passes, so `make test` works without a GPU or ffmpeg. `cargo test` hides standard error of
passing tests; `cargo test -- --nocapture` shows it. With `PRAXI_TEST_STRICT` set to any
value these tests fail instead, which shows they all ran.

| Test | Needs |
|---|---|
| `praxi-wgpu`: `motion_decoder_matches_reference` | An adapter that runs compute shaders (lavapipe/llvmpipe works). `WGPU_ADAPTER_NAME` picks one. |
| `praxi-wgpu`: `block_formats_draw_like_the_cpu_decoder` | An adapter with BC or ASTC compression; draws every encoded block format the adapter samples (BC7, BC1, ASTC 4x4, ASTC 6x6) and compares with the CPU decoder. |
| `praxi-cli`: `tests/cli.rs` | `ffmpeg` and `ffprobe` on `PATH` |
| `praxi-bevy`: `plays_into_the_image_and_sends_events` | An adapter (the motion clip also needs compute shaders) |

## What the tests cover

| Crate | Tests |
|---|---|
| `praxi-format` | Container round trips, corrupt input, byte planes, cue payloads and cue sheets, cue replacement, ADPCM, the motion entropy decoder and decode-ahead |
| `praxi` | Player: variant selection, events, cues, seeking, looping, audio outputs and clocks |
| `praxi-encoder` | Encoding every variant and playing it back, CPU decoding, BC1, motion frames, audio, cue replacement on a motion file |
| `praxi-wgpu` | Format mapping, the GPU motion decoder against the CPU reference, block formats drawn against the CPU decoder |
| `praxi-cli` | Argument parsing, SRT conversion, cue add and remove; `tests/cli.rs` runs the binary end to end on a video ffmpeg generates: `encode` (BC1, BC7, motion, `--scale`, `--fps`, audio, cues), `info`, `frame`, `cues export / import / add / remove / srt` and `bench --json` |
| `praxi-viewer`, `praxi-editor` | Argument parsing, the editor's document, drafts and file closing |
| `praxi-audio` | Resampling and channel mapping, without a device |
| `praxi-bevy` | The plugin in a headless Bevy app: frames drawn and copied into the image, events as messages |

## Manual checks

These need a desktop and are not covered by tests:

- The viewer window: playback, keys, audio (`praxi-viewer FILE`).
- The editor: timeline input, inspector, saving, "Write into file" (on Windows too), the
  close dialog under a window manager ([cue-editor.md](cue-editor.md)).
- The `cutscene` example (`cargo run -p praxi-wgpu --example cutscene -- FILE`).
- The Bevy cue editor (`cargo run --release -p praxi-bevy --example cue_editor -- FILE`),
  also under Xvfb ([bevy.md](bevy.md#tests)).
- Sound from the viewer and the editor, including converted tracks
  ([audio-output.md](audio-output.md)).
- GPU decode time on real hardware: `motion_timing` ([motion-codec.md](motion-codec.md#tools)).
