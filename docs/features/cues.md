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

## Authoring

- Encoder CLI: `--cue TIME[+DURATION]=TYPE[:KEY=VALUE[;KEY=VALUE...]]` adds a cue to the
  stream named `cues`. Values given on the command line are strings. See
  [encoder.md](encoder.md).
- Encoder library: any number of cue streams, typed field values.
- Cue sheets: `praxi cues export` and `praxi cues import` rewrite the cue streams of an
  existing file without re-encoding video; `praxi cues srt` converts subtitles to cues
  ([cue-editing.md](cue-editing.md)).
