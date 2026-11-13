# praxi-viewer

Plays `.praxi` files in a window on the GPU, with audio, and renders screenshots.

```sh
cargo install praxi-viewer
praxi-viewer intro.praxi
praxi-viewer intro.praxi --cues intro.cues.json   # preview a cue sheet, reloaded on save
praxi-viewer intro.praxi --screenshot frame.png --at 2.5
```

Space plays or pauses, Left / Right seek, Home goes to the start, L loops, M mutes, Q quits.
On Linux, building needs the ALSA headers (`libasound2-dev`); `--no-default-features`
builds without audio.
