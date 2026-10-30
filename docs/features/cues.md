# Cues

Cues are timed events stored in a praxi file. The player emits them while playing, so the
engine can react: shake the camera, play a sound, show a subtitle. They are meant to be
authored by tools: the encoder CLI, a future cue editor, or a subtitle converter.

## Model

A cue has:

- **time**: when it starts (the chunk pts).
- **duration**: how long it lasts. `0` makes an instantaneous cue (a marker).
- **type**: a non-empty UTF-8 string chosen by the author, for example `explosion`,
  `subtitle`, `camera.shake`. praxi gives meaning to none of them except the conventions
  below.
- **fields**: an ordered list of key/value pairs with unique keys. Values are typed:
  string, integer (i64), float (f64), bool or bytes.

Cues live in **cue streams**. A file can have any number of them, each with:

- **name**: free text, at most 32 bytes, for example `cues`, `subtitles`, `director`.
- **language**: optional BCP 47 tag, at most 8 bytes, for example `en`, `pl`, `pt-BR`.
  Used to pick a subtitle language.

Cues in one stream may overlap and may share a start time.

The byte layout is in [PRAXI.md](../spec/PRAXI.md#cue-payload).

## Player behaviour

- `cue_streams()` lists the cue streams (stream number, name, language). All are enabled
  when the file is opened; `set_cue_stream_enabled(stream, bool)` turns one on or off, for
  example to show only the subtitle language the player picked.
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

## Conventions

### Subtitles

- One cue stream per language, named `subtitles`, with the language set.
- Cue type `subtitle`, duration set to how long the text is shown.
- Fields:
  - `text` (string, required): the text, lines separated by `\n`. No markup.
  - `speaker` (string, optional): who is speaking.
  - `position` (string, optional): `bottom` (default) or `top`.

The engine shows `text` on `Cue` and hides it on `CueEnd` with the same `id`.

### Markers

An instantaneous cue with no fields, whose type names the moment (`explosion`, `fade-out`).

## Authoring

- Encoder CLI: `--cue TIME[+DURATION]=TYPE[:KEY=VALUE[;KEY=VALUE...]]` adds a cue to the
  stream named `cues`. Values given on the command line are strings. See
  [encoder.md](encoder.md).
- Encoder library: any number of cue streams, typed field values.
- Planned: `--subtitles FILE.srt[:LANG]` to convert SRT files into subtitle cue streams,
  and a cue editor that rewrites the cue streams of an existing file without re-encoding
  video.
