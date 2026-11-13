# praxi-cli

The `praxi` command line tool: encode videos into `.praxi` files, inspect them, export
frames and edit their cues.

```sh
cargo install praxi-cli   # installs the `praxi` binary
praxi encode input.mp4 -o intro.praxi --variant bc7 --variant astc6x6 --cue 2.5=explosion
praxi info intro.praxi
praxi frame intro.praxi --at 2.5 -o frame.png
praxi cues export intro.praxi -o intro.cues.json
praxi cues import intro.praxi intro.cues.json -o intro.praxi
```

Encoding reads any video ffmpeg can decode and needs `ffmpeg` and `ffprobe` on `PATH`.
Watch files with `praxi-viewer`, edit cues on a timeline with `praxi-editor`.
