# Cues

Cues are timed events stored in a praxi file. The player emits them while playing, so the
engine can react: shake the camera, play a sound, show text. Cues are generic: praxi stores
and times them but gives no meaning to their types or fields; that is up to the game. They
are meant to be authored by tools: the encoder CLI, the encoder library or cue sheets
([cue-editing.md](cue-editing.md)).

## Model

A cue has:

- **time**: when it starts (the chunk pts).
- **duration**: how long it lasts. `0` makes an instantaneous cue (a marker).
- **type**: a non-empty UTF-8 string chosen by the author, for example `explosion`,
  `camera.shake`, `light.flash`.
- **fields**: an ordered list of key/value pairs with unique keys. Values are typed:
  string, integer (i64), float (f64), bool or bytes.

Cues live in **cue streams**. A file can have any number of them. A stream only has a
**name** (free text, at most 32 bytes, for example `cues`, `gameplay`, `director`) used to
group cues; streams carry no other meaning. Anything specific to a kind of cue is a field
of the cue.

Cues in one stream may overlap and may share a start time.

The byte layout is in [PRAXI.md](../spec/PRAXI.md#cue-payload).

## Screen position

A cue can be placed on the screen, for annotations: a marker, a speech bubble, the target
of a camera move. The position is not a separate part of the format but two fields with
reserved names:

| Field | Value | Meaning |
|---|---|---|
| `x` | float (or integer) in `0..=1` | Horizontal position: `0` left edge, `1` right edge of video track 0 |
| `y` | float (or integer) in `0..=1` | Vertical position: `0` top edge, `1` bottom edge |

- The position is in video coordinates, independent of the video's pixel size and of how
  the game scales it. It is a point; what is drawn there (and its anchor) is up to the game.
- A cue has a position only when both fields are present and in range; otherwise it has
  none, and `x` / `y` are ordinary fields. Integers count so that hand-written sheets can
  say `0` and `1`.
- `Cue::position()` and `CueEvent::position()` return `Option<(f32, f32)>`;
  `Cue::set_position(Some((x, y)))` writes both fields as floats (clamped to `0..=1`)
  and `set_position(None)` removes them. The fields keep their order; new fields go last.
- The encoder CLI's `--cue` and `praxi cues add` write string fields, which do not count as
  a position. Use the editor or a cue sheet (`"x": 0.25, "y": 0.6`).
- The cue editor shows positioned cues on the video and picks positions with the mouse
  ([cue-editor.md](cue-editor.md#screen-positions)).

## Player behaviour

- `cue_streams()` lists the cue streams (stream number and name). All are enabled when the
  file is opened; `set_cue_stream_enabled(stream, bool)` turns one on or off.
- When the clock reaches a cue's start, the player emits `Event::Cue` with an `id`, the
  stream, time, duration, type and fields.
- When the clock reaches the end of a cue with a non-zero duration, the player emits
  `Event::CueEnd { id, stream }`. Instantaneous cues have no end event.
- `id` identifies the cue within the file (it is the offset of the cue's chunk), so the
  engine can match `CueEnd` to the `Cue` that started it, even with overlapping cues.
- Cue starts and ends are emitted in time order. At the same time, ends come before starts.
- **Seeking** ends every active cue (`CueEnd`), emits `Seeked`, then emits `Cue` for every
  cue with a non-zero duration that is active at the target time (started at or before it
  and ending after it). Instantaneous cues at or before the target are not emitted.
- **Looping** ends every active cue before `Looped`.
- **Finishing** ends every cue still active before `Finished`.
- Disabling a cue stream ends its active cues immediately (emitting `CueEnd`).
- `replace_cues(tracks)` plays other cues in place of the file's cue streams, for example
  from cue sheets; their streams are numbered after the file's streams and their ids are
  `2^63` and above ([cue-preview.md](cue-preview.md#player)).

## Authoring

- Encoder CLI: `--cue TIME[+DURATION]=TYPE[:KEY=VALUE[;KEY=VALUE...]]` adds a cue to the
  stream named `cues`. Values given on the command line are strings. See
  [encoder.md](encoder.md).
- Encoder library: any number of cue streams, typed field values.
- Cue sheets: `praxi cues export` and `praxi cues import` rewrite the cue streams of an
  existing file without re-encoding video, `praxi cues add` and `praxi cues remove` change
  cues without a sheet, and `praxi cues srt` converts subtitles to cues
  ([cue-editing.md](cue-editing.md)). The viewer previews sheets live
  ([cue-preview.md](cue-preview.md)).
