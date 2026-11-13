# Releasing

How praxi is versioned, tagged and published to [crates.io](https://crates.io).

## Versions

- Every crate in the workspace has the workspace version (`[workspace.package] version`),
  and they are released together. Crates depend on each other at exactly that version.
- Crate versions follow semver. While the major version is `0`, a minor version may break
  the API.
- The file format has its own version ([PRAXI.md](../spec/PRAXI.md), 0.10 at release
  0.1.1). A crate release lists the format version it reads and writes in the README;
  crate and format versions are not tied.
- A release is the commit tagged `vX.Y.Z` (annotated) with the workspace version `X.Y.Z`.

## Crates

Published, in dependency order:

| Crate | What users get |
|---|---|
| `praxi-format` | Container reader and writer, cue sheets |
| `praxi` | The player library for games |
| `praxi-encoder` | Encoder library |
| `praxi-wgpu` | Texture upload and drawing for `wgpu` engines |
| `praxi-audio` | Audio output through `cpal` |
| `praxi-bevy` | Bevy plugin (the crate to depend on from a Bevy game) |
| `praxi-cli` | Binary `praxi`: encode, inspect, export frames, edit cues |
| `praxi-viewer` | Binary `praxi-viewer`: plays files in a window |
| `praxi-editor` | Binary `praxi-editor`: cue timeline editor |

The command line tool, the viewer and the editor are three separate binaries, so the tool
stays free of window and GPU dependencies:

```sh
cargo install praxi-cli praxi-viewer praxi-editor
```

## Manifest

Every crate has, besides `name`, `version` and `description`:

- `license = "MIT OR Apache-2.0"` from the workspace, with `LICENSE-MIT` and
  `LICENSE-APACHE` at the repository root.
- `repository = "https://github.com/qbart/praxi"` and `homepage` (the same) from the
  workspace. The repository is mirrored from the private Forgejo remote (`origin`) to the
  `gh` remote, which is public.
- `readme = "README.md"`: a short README per crate (what it is, a usage example, links to
  the docs). Images are only in the repository README.
- `rust-version = "1.98"` from the workspace: the highest minimum Rust version among the
  dependencies (`opus1`), checked with `cargo +1.98 check --workspace --all-targets`.
- `keywords` and `categories` for crates.io search.
- `[package.metadata.docs.rs] all-features = true` in `praxi`, `praxi-format` and
  `praxi-bevy`, so docs.rs documents the feature-gated APIs (cue parsing, cue sheets,
  audio).
- Dependencies on other praxi crates through `[workspace.dependencies]` with both `path`
  and `version`, so the published manifest points at crates.io.

Packages contain the crate's sources, tests and examples; `praxi-bevy` also ships
`cue_game.cues.json` for its example.

## Make targets

- `make package`: `cargo publish --workspace --dry-run`. Packages and builds every crate
  as crates.io would, with unpublished praxi crates resolved from the workspace.
- `make publish`: refuses unless the working tree is clean, the tag `v$(VERSION)` (the
  workspace version) exists, and nothing that goes into the crates (`crates/`,
  `Cargo.toml`, `Cargo.lock`) changed since that tag; docs and the Makefile may have, then runs `make check` and
  `cargo publish --workspace`, which publishes the crates in dependency order and waits for
  each to be available before the next. Needs `cargo login` first.
- `make publish-CRATE` (for example `make publish-praxi-format`): publishes one crate with
  `cargo publish -p CRATE`, with the same clean tree and tag checks as `make publish`, but
  without `make check`. Crates must go in the order of the table above, as each needs the
  praxi crates it depends on to be on crates.io already; `make crates` prints that order.
  Use it to resume a `make publish` that stopped part way, or to publish one by one.
- `make package-CRATE`: `cargo publish -p CRATE --dry-run`. For a crate whose praxi
  dependencies are not on crates.io yet, this fails; `make package` checks them together.
- `make tag`: creates the annotated tag `v$(VERSION)` on `HEAD` and pushes it.

## Release binaries

`.github/workflows/release.yml` runs on GitHub when a `v*` tag is pushed to the `gh`
remote. It builds `praxi`, `praxi-viewer` and `praxi-editor` in release mode
(`cargo build --release --locked`) for three targets, each on its own runner:

| Target | Runner | Archive |
|---|---|---|
| Linux x86_64 (`x86_64-unknown-linux-gnu`) | `ubuntu-22.04`, so the binaries run on glibc 2.35 and newer | `praxi-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon (`aarch64-apple-darwin`) | `macos-latest` | `praxi-vX.Y.Z-aarch64-apple-darwin.tar.gz` |
| Windows x64 (`x86_64-pc-windows-msvc`) | `windows-latest` | `praxi-vX.Y.Z-x86_64-pc-windows-msvc.zip` |

- Each archive holds the three binaries (`.exe` on Windows), `README.md`, `LICENSE-MIT`
  and `LICENSE-APACHE` in a directory named like the archive.
- When all three builds pass, a release named `praxi X.Y.Z` is created on the tag with the
  archives, a `SHA256SUMS` file and notes generated by GitHub. If one build fails, no
  release is made.
- The Linux runner installs `pkg-config` and `libasound2-dev` for audio. The binaries need
  ALSA (`libasound.so.2`) and a Vulkan or GL driver at run time.
- The macOS binaries are not signed. Gatekeeper blocks binaries downloaded with a browser;
  `xattr -d com.apple.quarantine praxi*` allows them.
- Started by hand (`workflow_dispatch`), the workflow builds the archives as workflow
  artifacts without making a release, to check the builds before tagging.
- The workflow uses only `actions/checkout`, `actions/upload-artifact`,
  `actions/download-artifact` and the `gh` CLI on the runner.

## Versions released

- 0.1.0: tagged, never published (the release binaries workflow did not exist yet).
- 0.1.1: the first release on crates.io and GitHub.

## Release steps

1. Set `[workspace.package] version` and the versions in `[workspace.dependencies]`.
2. Commit and push; `make check` and `make package` pass.
3. `make tag`, then `make publish`.
4. Push `main` and the tag to GitHub: `git push gh main v$(VERSION)`. This starts the
   release binaries build.
