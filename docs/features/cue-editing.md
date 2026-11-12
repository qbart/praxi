# Cue editing

Cues ([cues.md](cues.md)) are authored after the video is encoded: timings get adjusted,
gameplay hooks added, subtitles converted. Re-encoding video for that takes minutes, so
praxi edits cues in place: export the cues of a file to a text **cue sheet**, edit it by
hand or with a tool, and import it back. Import rewrites the file's cue streams and copies
every video and audio chunk byte for byte.

```sh
praxi cues export intro.praxi -o intro.cues.json
$EDITOR intro.cues.json
praxi cues srt subtitles.en.srt --stream subtitles --field lang=en -o subs.en.json
praxi cues import intro.praxi intro.cues.json subs.en.json -o intro.praxi
```

## Cue sheets

A cue sheet is a JSON file listing cue streams by name, each with its cues. Times and
durations are seconds, kept to the microsecond. Field values keep their types: JSON
strings, integers, floats and booleans map to the cue value types, and bytes are written
as `{"bytes": "<hex>"}`.

```json
{
  "version": 1,
  "streams": [
    {
      "name": "cues",
      "cues": [
        { "time": 2.5, "type": "explosion", "fields": { "power": 0.8 } },
        { "time": 4.0, "duration": 2.5, "type": "light.flash", "fields": { "color": "red" } }
      ]
    },
    {
      "name": "subtitles",
      "cues": [
        { "time": 1.0, "duration": 3.0, "type": "subtitle", "fields": { "text": "Hello.", "lang": "en" } }
      ]
    }
  ]
}
```

The format is specified in [CUESHEET.md](../spec/CUESHEET.md).

## Commands

| Command | Does |
|---|---|
| `praxi cues export FILE [-o SHEET]` | Writes the cues of every cue stream of `FILE` as a cue sheet, to `SHEET` or standard output. Streams and cues are in file order. |
| `praxi cues import FILE SHEET... -o OUT` | Writes `OUT`: `FILE` with its cue streams replaced by the streams of the sheets, in sheet order then stream order. `OUT` may be `FILE`: the result goes to a temporary file next to it, renamed over it when complete. |
| `praxi cues srt SRT [-o SHEET]` | Converts SubRip subtitles to a cue sheet with one stream. |
| `praxi cues add FILE --cue CUE... [--stream NAME] [-o OUT]` | Adds cues to the stream `NAME` (default `cues`) without a sheet. `CUE` is the encoder's syntax, `TIME[+DURATION]=TYPE[:KEY=VALUE[;KEY=VALUE...]]` ([encoder.md](encoder.md)), with [typed values](cues.md#values-on-the-command-line). If several streams have that name the first gets the cues; if none has it, a new stream is added after the others. |
| `praxi cues remove FILE [--stream NAME] [--type TYPE] [--from T] [--to T] [-o OUT]` | Removes the cues matching every filter given: in a stream named `NAME`, of type `TYPE`, starting at `T` seconds or later (`--from`), starting before `T` (`--to`). At least one filter is required. Streams left without cues are removed. |

`cues add` and `cues remove` write `OUT`, by default `FILE` itself, through the same
temporary file as `cues import`, and print how many cues they added or removed. They
follow the [import rules](#import-rules), so a file edited with them is the same as one
imported from an edited sheet:

```sh
praxi cues add intro.praxi --cue 2.5=explosion:power=0.8 --cue "4+2.5=light.flash:color=red"
praxi cues add intro.praxi --stream markers --cue "5.5=marker:label=Look here;x=0.5;y=0.5"
praxi cues remove intro.praxi --type explosion --from 2 --to 3
```

`cues srt` options:

| Option | Default | Meaning |
|---|---|---|
| `--stream <NAME>` | `subtitles` | Name of the stream. |
| `--type <TYPE>` | `subtitle` | Type of every cue. |
| `--text-key <KEY>` | `text` | Field holding the subtitle text. |
| `--field <KEY=VALUE>` (repeatable) | | A field added to every cue after the text, for example `lang=en`, with [typed values](cues.md#values-on-the-command-line). |

Subtitles are not special to praxi: the converter only produces generic cues that start and
end with each subtitle and carry its text. The text keeps its line breaks (`\n`) and any
SRT markup (`<i>`, `{\an8}`); interpreting it is up to the game. The converter accepts a
UTF-8 byte order mark, CRLF line ends, `.` as the millisecond separator and missing
sequence numbers; subtitles with an end before their start are rejected.

## Import rules

- Video, audio and streams of unknown kinds keep their order, parameters and chunks; their
  stored bytes are copied as they are, without decompressing or recompressing. Cue streams
  come after them, with a microsecond time base.
- Cues may be listed in any order in a sheet; within each stream they are written in time
  order (stable, so cues at the same time keep their sheet order).
- Chunks are merged in decode-time order, cues first when times are equal, as the encoder
  writes them; the index is rebuilt.
- The header duration becomes the larger of the old duration and the latest cue end. It
  never shrinks, because video and audio ends are not recorded separately.
- A sheet with no streams removes every cue stream.

## Library

The CLI is built on `praxi_format`, so an editor application can do the same in process:

- `read_cues(&mut Reader) -> Result<Vec<CueTrack>>`: the cues of every cue stream.
- `replace_cues(Reader, W, &[CueTrack]) -> Result<W>`: writes the file with the given cue
  streams.
- `CueTrack { name, cues: Vec<TimedCue> }`, `TimedCue { time_us, cue }`; the `Cue`'s
  `duration` is in microseconds.
- `Reader::read_stored` and `Writer::copy_chunk` copy chunks in their stored form.
- `cue_sheet::parse` and `cue_sheet::write` (feature `cue-sheet`) read and write cue
  sheets.

- `replace_cues_in_file(input, output, tracks)`: the same through a temporary file, as
  `cues import` does. Both files are closed before the rename. On Windows a rename that
  fails with "access denied" is retried up to 10 times, 100 ms apart, since antivirus and
  search indexers briefly open new files; the file being replaced must not be open
  elsewhere (the editors close theirs first).

To watch edits to a sheet without importing it, see [cue-preview.md](cue-preview.md). To
edit cues on a timeline, see [cue-editor.md](cue-editor.md).
