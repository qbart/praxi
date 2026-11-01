# Cue preview

Timing cues against the picture is a loop: edit a time, watch, adjust. With cue sheets
alone ([cue-editing.md](cue-editing.md)) every round trip is an import, which rewrites the
whole file, and a viewer restart. Cue preview shortens the loop: the viewer plays the file
with the cues of one or more sheets in place of the file's cues, and reloads the sheets
while it plays whenever they change on disk.

```sh
praxi cues export intro.praxi -o intro.cues.json
praxi-viewer intro.praxi --cues intro.cues.json --loop
# edit and save intro.cues.json: the viewer picks it up
praxi cues import intro.praxi intro.cues.json -o intro.praxi
```

The same runtime call is open to games: cues can ship next to a video instead of inside it,
for example one subtitle sheet per language, loaded at run time.

## Viewer

| Option / key | Does |
|---|---|
| `--cues <SHEET>` (repeatable) | Plays with the cue streams of the sheets, in sheet order then stream order, in place of the file's cue streams, as `praxi cues import FILE SHEET...` would write them. |
| `R` | Reloads the sheets now. |

- The viewer checks the modification time of every sheet four times a second. When one
  changed (or a missing sheet reappears), it reads all sheets again and replaces the cues.
- On every load it prints the sheets, stream and cue counts, for example
  `cues: 2 streams, 14 cues from intro.cues.json, subs.en.json`, and how many cues end
  after the file's duration, which import would extend but preview does not play.
- A sheet that cannot be read or parsed fails start-up. During playback the error is
  printed and the previous cues stay, so a half-saved file does not stop the preview.
- Events are printed as usual; their stream numbers and ids are the preview's (below).
- `--screenshot` ignores `--cues`.

## Player

`Player::replace_cues(tracks: Vec<CueTrack>)` (`CueTrack` and `TimedCue` from
`praxi_format`, re-exported by `praxi`) replaces the cue streams of the playing file:

- The file's cue streams are no longer read. `cue_streams()` lists the new streams,
  numbered from the file's stream count up (`streams + i` for track `i`); all are enabled.
  Tracks that would get a number above 65535 are dropped.
- Cues are emitted as if the file had been imported with these tracks: each stream's cues
  in time order (stable), streams merged by time, and a cue at the same decode time as a
  chunk comes before that chunk. Starts, ends, seeking, looping and finishing follow
  [cues.md](cues.md#player-behaviour).
- `id`s are `2^63 + n`, where `n` is the cue's position among all replaced cues in
  emission order. Chunk offsets, the ids of file cues, are below `2^63`.
- Cues starting after the file's duration are never emitted, and cues still active at the
  end are ended there, since the duration is the file's.
- It can be called at any time and any number of times. It never touches video or audio
  and emits no `Seeked`; for cues it acts like a seek to the current position:
  - every active cue ends (`CueEnd`);
  - cues the player has not reached yet are emitted when the clock gets to them, including
    those at the current position if `play()` has not decoded it yet;
  - cues with a non-zero duration active at the current position are emitted right away,
    unless the player is finished;
  - instantaneous cues already passed are not emitted;
  - stream enable states reset (all enabled).

Differences from an imported file: stream numbers (import puts cue streams after the other
streams and renumbers them), ids, and the duration (import extends it to the latest cue
end).

## Cue sheet library

Reading and writing cue sheets moves from the CLI into `praxi_format::cue_sheet`, behind
the feature `cue-sheet` (dependencies `serde`, `serde_json`), so the viewer, the CLI and
games share one implementation. `praxi` forwards the feature as `cue-sheet`.

- `cue_sheet::parse(&str) -> Result<Vec<CueTrack>, String>`.
- `cue_sheet::write(&[CueTrack]) -> Result<String, String>`: pretty-printed JSON with a
  final line break.
- `parse` checks every rule of [CUESHEET.md](../spec/CUESHEET.md), including stream name,
  cue type and field key lengths, which import used to report only when writing the file.
