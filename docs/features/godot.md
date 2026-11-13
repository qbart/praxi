# Godot

`praxi-godot` plays praxi files in [Godot](https://godotengine.org) 4.4 and newer. It is a
GDExtension: a native library Godot loads, written in Rust with
[godot-rust](https://github.com/godot-rust/gdext) on top of the `praxi` player. Godot
projects use it from GDScript, C# or C++ like any built-in class; nobody writes Rust.

The built-in `VideoStreamPlayer` node plays `.praxi` files, frames stay block-compressed on
the GPU as in the other integrations, audio plays through the node, and cues arrive as
signals.

```gdscript
@onready var player: VideoStreamPlayer = $VideoStreamPlayer

func _ready():
    var stream: VideoStreamPraxi = load("res://intro.praxi")
    stream.looping = true
    stream.cue.connect(_on_cue)
    player.stream = stream
    player.play()

func _on_cue(id: int, type: String, fields: Dictionary, time: float, duration: float, position: Variant):
    if type == "camera.shake":
        $Camera2D.shake(fields.get("strength", 1.0))
```

## Addon

A release ships `praxi-godot-vX.Y.Z.zip` with one directory to copy into a project:

```
addons/praxi/
  praxi.gdextension
  bin/libpraxi_godot.linux.x86_64.so
  bin/libpraxi_godot.macos.arm64.dylib
  bin/praxi_godot.windows.x86_64.dll
  README.md, LICENSE-MIT, LICENSE-APACHE
```

- `praxi.gdextension` names the entry symbol `gdext_rust_init`, the libraries per platform,
  and `compatibility_minimum = "4.4"`.
- The libraries are built by the release workflow on the same runners as the binaries
  ([releasing.md](releasing.md#release-binaries)): Linux x86_64, macOS Apple Silicon,
  Windows x64.
- From a checkout, `make godot-addon` builds the library for this machine into
  `target/godot/addons/praxi/`.

## Crate

`crates/praxi-godot`: `crate-type = ["cdylib"]`, `publish = false` (it is a Godot addon,
not a Rust library, so it is not on crates.io). It depends on `godot` 0.5 with the
`api-4-4` feature: built against Godot 4.4's API, it loads in 4.4 and every later 4.x.
Tested with Godot 4.7.

## Classes

### `VideoStreamPraxi` (extends `VideoStream`)

The resource for a `.praxi` file, set as a `VideoStreamPlayer`'s `stream`.

| Member | Meaning |
|---|---|
| `file: String` | The file (inherited from `VideoStream`); `res://`, `user://` or an absolute path. |
| `looping: bool` | Seamless looping in the player (default `false`). `VideoStreamPlayer.loop` restarts the stream instead, with a gap. |
| `formats: PackedStringArray` | Texture formats to accept, by name (`bc7`, `bc1`, `astc4x4`, `astc8x8`). Empty (default): what the rendering device supports. |
| `active_cues() -> Array[Dictionary]` | Cues active at the clock of the playback that plays this stream ([cue-runtime.md](cue-runtime.md#active-cues)), each as in [Cue dictionaries](#cue-dictionaries). Empty when nothing plays. |
| `upcoming_cues(within: float) -> Array[Dictionary]` | Cues starting within `within` seconds, each with an extra `until` (seconds) ([cue-runtime.md](cue-runtime.md#upcoming-cues)). |
| signal `cue(id: int, type: String, fields: Dictionary, time: float, duration: float, position: Variant)` | A cue started. `position` is a `Vector2` in `0..1` of the video (`x`, `y` fields, [cues.md](cues.md#screen-position)) or `null`. |
| signal `cue_ended(id: int)` | A cue with a duration ended. |
| signal `looped()` | Playback continued from the start (with `looping`). |

The signals are emitted from `VideoStreamPlayer`'s processing, on the main thread, in the
player's event order: in the frame where the cue's time is reached, before the frame is
drawn. Signals are on the stream because Godot does not expose the playback object of a
`VideoStreamPlayer`; when several players play the same stream resource at once, use one
resource per player (`stream.duplicate()`).

### `VideoStreamPlaybackPraxi` (extends `VideoStreamPlayback`)

Created by `VideoStreamPraxi` for each `VideoStreamPlayer.play()`; not used directly. It
owns a `praxi::Player` and implements the playback methods:

| Godot | praxi |
|---|---|
| `_play()` | `play()`; from the start after `_stop()` or the end. |
| `_stop()` | `pause()`, `seek(0)`; `_is_playing()` becomes false. |
| `_set_paused(p)` / `_is_paused()` | `pause()` / `play()` |
| `_is_playing()` | Playing or paused, and not finished. |
| `_get_length()` | `duration()` in seconds. |
| `_get_playback_position()` | The player's clock in seconds. |
| `_seek(t)` | `seek(t)`. |
| `_update(delta)` | `advance(delta)`, then the texture, audio and signals below. |
| `_get_texture()` | The video texture. |
| `_set_audio_track(i)` | Plays audio track `i` (`VideoStreamPlayer.audio_track`), disables the others. |
| `_get_channels()`, `_get_mix_rate()` | Of the audio track played. |

### `ResourceFormatLoaderPraxi`

Registered when the extension loads: files ending in `.praxi` load as `VideoStreamPraxi`,
so they can be dragged onto a `VideoStreamPlayer` in the editor and `load()`ed from
scripts. The loader does not read the file; opening happens on `play()`. Exported games
include `.praxi` files as resources.

## Files

The player reads through Godot's `FileAccess`, so `res://` paths work inside exported
`.pck` files and on Android. A small adapter implements `Read` and `Seek` over `FileAccess`
(`get_buffer`, `seek`, `get_position`). A file that cannot be opened, or has no variant
the device can play, prints an error with `push_error` and the playback reports not
playing.

## Texture formats

`formats` empty: the accepted formats follow `RenderingServer.has_os_feature`:

| Feature | praxi format | Godot `Image` format |
|---|---|---|
| `bptc` | BC7 | `FORMAT_BPTC_RGBA` |
| `s3tc` | BC1 | `FORMAT_DXT1` |
| `astc` | ASTC 4x4, ASTC 8x8 | `FORMAT_ASTC_4x4`, `FORMAT_ASTC_8x8` |

The player picks among the file's variants as everywhere else
([texture-variants.md](texture-variants.md)). Godot has no ASTC 6x6, 5x5 or other block
sizes, and the motion codec needs a GPU decoder (roadmap): files for Godot need one of the
formats above.

## Video

Video track 0 is shown. Each `_update` that decodes a new frame (`VideoFrame::index`
changed) copies its blocks into an `Image` (`Image.create_from_data` with the format above,
no mipmaps) and updates an `ImageTexture` with it; the first frame, or a size change,
creates the texture. The texture keeps the video's size. The copy costs one memcpy of the
frame's blocks (about 2 MB for 1080p BC7) per new frame. Other video tracks are ignored.

Colors: BC and ASTC data is sRGB-encoded, which is what Godot expects of a texture drawn
in 2D or used as albedo; alpha is straight, as in Godot.

## Audio

- The player is opened with `max_audio_channels(2)`, so a stereo (or mono) variant is
  picked where the file has layout variants.
- The audio track played (`VideoStreamPlayer.audio_track`, default 0) is enabled, the
  others disabled.
- Each `_update` reads what its `AudioOutput` has decoded and hands it to
  `VideoStreamPlayback.mix_audio`; samples the node does not accept yet are kept and
  offered first next time. `VideoStreamPlayer` resamples to the mix rate and plays them
  on its bus.
- The clock is the game clock (`Clock::Game`), advanced by `_update`'s delta; audio
  follows it with the player's sync tolerance ([audio.md](audio.md#sync)). The audio
  clock of the other integrations needs to know when samples are heard, which Godot's
  video mixing does not tell; it is a later step.

## Cue dictionaries

`active_cues()` and `upcoming_cues()` return dictionaries with the signal's arguments as
keys: `id`, `type`, `fields`, `time`, `duration`, `position` (and `until` for upcoming).
Field values convert as: string to `String`, integer to `int`, float to `float`, bool to
`bool`, bytes to `PackedByteArray`.

## Example project

`examples/godot/` is a small Godot project, like the Bevy `cue_game`
([cue-runtime.md](cue-runtime.md#example-game)): it plays a file with
`VideoStreamPlayer`, shows subtitles, markers at cue positions and a camera shake from
the cues of `crates/praxi-bevy/examples/cue_game.cues.json`, with Space, arrows, L and M
as in the Bevy example.

- `make godot-example` builds the addon, copies it into the project, encodes the sample
  with BC7 and the cue sheet into `examples/godot/intro.praxi`, and opens the project
  with `godot` (`GODOT` overrides the binary).
- The addon and `intro.praxi` are build output, ignored by git.

## Tests

`crates/praxi-godot/tests/godot.rs` runs Godot headless (`GODOT`, or `godot` / `godot4` on
`PATH`; skipped otherwise, failing under `PRAXI_TEST_STRICT`, [testing.md](testing.md)).
It builds the extension, writes a temporary project with the addon, the
`.godot/extension_list.cfg` the editor would write, a generated `.praxi` (64x32 BC7, 2 s at
30 fps, a stereo 48 kHz tone, a marker cue with a position and duration and an instant
`camera.shake` cue with float, int and bool fields) and a test script, and runs
`godot --headless --max-fps 60 --path PROJECT --script res://test.gd`. Headless Godot mixes
audio only in real time, so the test runs paced at 60 fps (about 4 s) rather than with
`--fixed-fps`. Steps follow the playback position:

- `load()` returns a `VideoStreamPraxi` of the clip's length; with `formats = ["bc7"]`
  (the headless renderer reports no texture formats), a `VideoStreamPlayer` with
  `autoplay` plays it.
- At 0.6 s: `active_cues()` lists the marker, `upcoming_cues(1.0)` the shake with a
  positive `until`, the texture has the video's size, and the master bus peaks above
  -40 dB (the tone is -14 dB), so audio reached `mix_audio`.
- At 1.3 s: `cue` (marker, with its fields, times and `Vector2` position), `cue_ended`
  (the marker's id) and `cue` (shake, `null` position, typed fields) arrived in that order.
- Seeking to 0.2 s lands there; pausing holds the position for 10 frames; with `looping`
  the `looped` signal arrives while the player keeps playing; `stop()` stops it.

The script prints `PASS` or `FAIL: <check>`. The test passes only on `PASS` with no
`SCRIPT ERROR` in Godot's output, so a script that stops early cannot pass.
