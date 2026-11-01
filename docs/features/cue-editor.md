# Cue editor

`praxi-editor` is a desktop editor for the cues of a `.praxi` file: it plays the video
with sound, shows every cue stream as a lane on a timeline with the audio waveform, and
edits cues in place. It reads and writes cue sheets ([CUESHEET.md](../spec/CUESHEET.md))
and can write the cues into the file, like `praxi cues import`
([cue-editing.md](cue-editing.md)). Edits play back immediately through
`Player::replace_cues` ([cue-preview.md](cue-preview.md#player)).

```sh
praxi-editor intro.praxi                   # edits intro.cues.json next to the file
praxi-editor intro.praxi subs.en.json      # edits another sheet
```

## Crate

`crates/praxi-editor`, binary `praxi-editor`.

| Dependency | Use |
|---|---|
| `eframe` (egui, wgpu backend) | Window, widgets, timeline drawing |
| `praxi-wgpu` | Video textures and drawing ([engine-integration.md](engine-integration.md#praxi-wgpu)) |
| `praxi` with `cue-sheet` | Player, cue sheets, cue replacement |
| `cpal` (feature `audio`, default) | Audio output, through the viewer's output module (`crates/praxi-viewer/src/audio.rs`, included by path) |
| `clap` | Arguments |

The video is drawn into an `Rgba8UnormSrgb` texture and copied into the `Rgba8Unorm`
texture egui samples, since egui expects sRGB-encoded bytes in a linear format and GL
backends cannot give one texture both views.

egui rather than a game engine such as Bevy: the editor is panels, text fields and a
timeline, which egui provides directly, and eframe's renderer uses the same `wgpu` and
`winit` versions as `praxi-wgpu`, so the video goes through the same upload path as in
the viewer.

## Opening

`praxi-editor FILE [SHEET]`

- `SHEET` defaults to `FILE` with its extension replaced by `.cues.json`.
- If `SHEET` exists, the editor opens it (an unreadable sheet is an error); otherwise it
  starts from the cues stored in `FILE` and saving creates `SHEET`.
- Options: `--format <F>` as in the viewer, `--no-audio`.

## Window

- **Toolbar**: Save, Write into file, Undo, Redo, Add stream, Snap to frames (on by
  default), and the save state.
- **Video**: track 0 and the tracks blended over it, letterboxed. Under it, the transport:
  play/pause, previous and next frame, position and duration, loop, mute.
- **Inspector** (right): the selected cue's stream (moving it to another stream), start,
  duration, end, type and fields. Each field has a key, a value type (string, int, float,
  bool, bytes) and a value; fields can be added, removed and reordered. With no cue
  selected, the selected stream's name. Below: **Active cues**, the cues active at the
  playhead with their fields, as the player emits them, and **Events**, the last 200
  player events.
- **Timeline** (bottom): a time ruler, a waveform of audio track 0 (peaks per pixel, all
  channels, scaled to the loudest peak), and one lane per cue stream with its name. Cues with a duration are bars,
  instantaneous cues are diamonds; each shows its type and its first string field (a
  subtitle's text, for example). The playhead is a vertical line.

### Timeline input

| Input | Does |
|---|---|
| Click or drag on the ruler or waveform | Seek / scrub |
| Click a cue | Select it (and its stream) |
| Drag a cue | Move it in time |
| Drag the right end of a bar, or a diamond with Shift | Change its duration |
| Double-click a lane | Add a cue there (type `cue`) |
| Click a lane name | Select the stream |
| Mouse wheel | Scroll in time |
| Ctrl + mouse wheel | Zoom around the pointer |

With Snap to frames on, dragged times snap to video frame starts of track 0; holding Alt
while dragging turns snapping off.

### Keys

Keys act when no text field has focus.

| Key | Does |
|---|---|
| Space | Play / pause |
| Left / Right | Previous / next frame (with Shift: 1 s) |
| Home / End | Seek to the start / end |
| C | Add a cue at the playhead in the selected stream |
| I / O | Set the selected cue's start / end to the playhead, keeping the other end |
| Delete | Delete the selected cue |
| Ctrl+D | Duplicate the selected cue at the playhead |
| Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y | Undo, redo |
| Ctrl+S | Save the sheet |
| Ctrl+Shift+S | Write into file |

## Editing model

- The document is a list of cue streams (`CueTrack`). Cues are kept sorted by start time
  within each stream (stable), so a saved sheet lists them in time order.
- Every finished edit (a drag released, a text field left, a button pressed) is one undo
  step; there are up to 200. While dragging, the timeline shows the edit live and the
  player gets it on release.
- After every finished edit the player plays the new cues (`replace_cues`): playback goes
  on, cues active at the playhead restart as described in
  [cue-preview.md](cue-preview.md#player).
- Problems that would make the sheet invalid (empty type or key, duplicate key, stream name
  over 32 bytes, unparseable number or hex) are shown in red where they are. They are not
  committed to the document until fixed, so the document always holds a valid sheet.
- Times and durations are typed in seconds and kept to the microsecond.

## Saving

- **Save** writes the sheet with `cue_sheet::write`.
- **Write into file** writes `FILE` with its cue streams replaced, through a temporary file
  renamed over it, exactly like `praxi cues import FILE SHEET -o FILE`. The editor keeps
  playing the file it opened. It does not save the sheet.
- The title shows the file, the sheet and `*` when the document differs from the last saved
  or written state. Closing the window with unsaved changes asks to save, discard or
  cancel.

## Library

Writing cues into a file through a temporary file moves from the CLI into `praxi_format`:
`replace_cues_in_file(input: &Path, output: &Path, tracks: &[CueTrack]) -> Result<()>`.
`output` may be `input`; the temporary file `.{name}.praxi-tmp` next to `output` is
removed on failure.
