# Benchmarks

`praxi bench` encodes a video and measures what praxi trades against each other: file
size, image quality and decode time, per texture variant. CI runs it on every push to
`main` and fails when size or quality get worse than the committed baseline (milestone M1).

## CLI

```sh
praxi bench samples/big_buck_bunny_1080_10s.mp4 --frames 180 \
    --variant bc7 --variant astc6x6 --variant astc8x8 \
    --json target/bench.json --baseline bench/baseline.json
```

`praxi bench <INPUT>` takes the video options of `praxi encode`: `--variant` (repeatable,
default `bc7`), `--quality`, `--rdo`, `--keyint`, `--skip-threshold`, `--plane-min-saving`,
`--scale`, `--fps` and `--frames`, plus `--qp`, `--chroma` and `--slices` for `motion`.
With `--scale` or `--fps` the quality is measured against the scaled and resampled source,
which ffmpeg produces again with the same filters. Audio
and cues are left out. Further options:

| Option | Default | Meaning |
|---|---|---|
| `--passes <N>` | `3` | Playbacks per variant for decode timing. |
| `--json <FILE>` | | Write the results ([BENCH.md](../spec/BENCH.md)). |
| `--baseline <FILE>` | | Compare with earlier results and fail on regressions. |
| `--max-size-increase <PCT>` | `1` | Allowed growth of a variant's bytes over the baseline, in percent. |
| `--max-psnr-drop <DB>` | `0.1` | Allowed PSNR drop below the baseline, in dB. |
| `--keep <FILE>` | | Keep the encoded `.praxi` file here (default: a temporary file, deleted at the end). |

`make bench` runs the command above after `make samples`; `make bench-baseline` rewrites
`bench/baseline.json` from a fresh run.

## Measurements

The input is encoded once with all variants, like `praxi encode` (one encoder, one ffmpeg
decode). Then for each variant:

- **Size**: bytes of the variant's video chunks (`VariantStats::bytes`), MB/s over the
  encoded duration, share of temporally skipped blocks and keyframe count.
- **Quality**: the file is played frame by frame with the player (keyframes plus deltas,
  after temporal skip, so exactly what a game shows) and every frame is decoded on the CPU
  ([frame-export.md](frame-export.md)). ffmpeg decodes the input a second time and each
  frame is compared with the source:
  - `psnr_db`: PSNR over all frames, `10 * log10(255^2 / MSE)` with the MSE averaged over
    every R, G, B sample of every frame (and A when the stream has alpha).
  - `psnr_min_db`: PSNR of the worst frame.
  - PSNR is capped at 100 dB (identical images).
- **Decode time**: the player plays the file `--passes` times from the start, one
  `advance` per frame interval, each advance decoding exactly one frame (reading the chunk,
  LZ4, applying the delta). Mean, 95th percentile and maximum of all advances, in
  milliseconds. The file is read through the OS page cache; there is no GPU upload.
- **Encode time**: wall-clock seconds of the encode, all variants together.

Results are printed as a table and, with `--json`, written to a file.

## Baseline comparison

With `--baseline`, the results are compared with a JSON file from an earlier run:

- The input file name, size, frame rate, frame count and encoder settings must be equal,
  otherwise the command fails with an error: the numbers would not be comparable.
- For every variant of the baseline, the new run must have the variant, its bytes must not
  exceed the baseline by more than `--max-size-increase` percent, and its `psnr_db` must
  not be lower by more than `--max-psnr-drop`. Variants new in this run are reported
  without a check.
- Decode times depend on the machine, so they are only reported, as a ratio to the
  baseline. They never fail the run.
- Each failed check prints a line starting with `regression:`, and the command exits with
  status 1 after writing `--json`.

Size and PSNR are deterministic for a given encoder, input and ffmpeg build. An intended
change in size or quality is committed together with a new baseline (`make
bench-baseline`).

## CI

`.forgejo/workflows/ci.yml` runs on every push to `main` and every pull request:

| Job | Steps |
|---|---|
| `test` | `cargo clippy` and `cargo test` for the workspace, with `praxi-viewer` built without the `audio` feature (no ALSA on the runner). |
| `bench` | Installs ffmpeg, `make samples`, `make bench`, uploads `target/bench.json` as an artifact. |

The jobs run on a runner with the `docker` label, in the runner's default image, and
install Rust with rustup. The baseline is recorded on a developer machine; only size and
quality are checked, so the runner's CPU does not matter.
