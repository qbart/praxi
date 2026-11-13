# praxi-editor

A cue editor for `.praxi` files: a timeline of the cue streams over the audio waveform,
an inspector for cue fields and screen positions, live playback with sound, and saving to
a cue sheet or straight into the playing file.

```sh
cargo install praxi-editor
praxi-editor intro.praxi                 # edits intro.cues.json, created from the file's cues
praxi-editor intro.praxi subs.en.json
```

Space plays, C adds a cue at the playhead, I / O set its start and end, Ctrl+Z undoes,
Ctrl+S saves the sheet, Ctrl+Shift+S writes the cues into the file. On Linux, building
needs the ALSA headers (`libasound2-dev`).
