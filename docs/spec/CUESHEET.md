# Cue sheet format

Version 1. File extension `.json` (suggested: `NAME.cues.json`). Written by
`praxi cues export` and `praxi cues srt`, read by `praxi cues import` and
`praxi-viewer --cues` ([cue-editing.md](../features/cue-editing.md),
[cue-preview.md](../features/cue-preview.md)). The `praxi_format::cue_sheet` module
(feature `cue-sheet`) reads and writes it.

A cue sheet is a UTF-8 JSON document ([RFC 8259](https://www.rfc-editor.org/rfc/rfc8259))
holding the cue streams of a praxi file ([PRAXI.md](PRAXI.md#cue-payload)).

## Document

The top level is an object:

| Key | Type | Required | Description |
|---|---|---|---|
| `version` | integer | yes | `1`. Readers reject other versions. |
| `streams` | array of streams | yes | Cue streams, in order. May be empty. |

Readers reject unknown keys at every level, so a typo does not silently drop data.

## Stream

| Key | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Stream name, at most 32 bytes of UTF-8, no zero bytes. Names need not be unique. |
| `cues` | array of cues | yes | The stream's cues, in any order. May be empty. |

## Cue

| Key | Type | Required | Description |
|---|---|---|---|
| `time` | number | yes | Start in seconds, at least `0`. |
| `duration` | number | no | Length in seconds, at least `0`; default `0` (an instantaneous cue). |
| `type` | string | yes | Cue type, not empty, at most 65535 bytes. |
| `fields` | object | no | Fields in order; default none. |

`time` and `duration` are rounded to the nearest microsecond. Writers write them with at
most six decimals, so a sheet written from a file reads back to the same microseconds.

## Fields

Each key of `fields` is a field key: not empty, at most 65535 bytes. Keys are unique (JSON
objects with duplicate keys are rejected). The order of the keys is the order of the fields.
Values map to cue value types:

| JSON value | Cue value |
|---|---|
| string | string |
| number without fraction or exponent, in `i64` range | integer |
| number with a fraction or an exponent | float |
| `true`, `false` | bool |
| object `{"bytes": "<hex>"}` | bytes: an even number of hex digits, either case |

Readers reject other values (`null`, arrays, other objects, integers outside `i64`).
Writers write floats with a fraction or exponent (`3.0`, not `3`) so they read back as
floats, and lowercase hex for bytes. Floats that JSON cannot hold (NaN, infinities) cannot
be written; writers report an error.

## Example

```json
{
  "version": 1,
  "streams": [
    {
      "name": "cues",
      "cues": [
        { "time": 2.5, "type": "explosion", "fields": { "power": 0.8, "count": 3 } },
        { "time": 4.0, "duration": 2.5, "type": "light.flash", "fields": { "color": "red", "seed": { "bytes": "00ff" } } }
      ]
    }
  ]
}
```
