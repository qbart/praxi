# Cues at runtime

How a game consumes cues while a video plays, beyond reading `Event::Cue` and matching
strings ([cues.md](cues.md#player-behaviour)): typed cues, the cues active at the playhead,
the cues coming up, screen positions in the drawn video, and typed messages in Bevy. An
example game in `praxi-bevy` uses all of them.

## Cue table

The player keeps every cue of the file in memory, sorted by start time (ties in file
order). `Player::open` reads them through the index: one read per cue chunk. Cues are
small (a type and a few fields), so this costs little memory even with thousands of
subtitles; the reads add to the time `open` takes. A malformed cue, or an index entry of a
cue stream pointing at a chunk of another stream, makes `open` fail, where a malformed cue
used to fail the `advance` that reached it.

Playback emits cues from the table and skips cue chunks while reading, so seeking needs no
reads to find the cues active at the target. Cues given to `replace_cues` replace the
table. Events, their order and the cue ids do not change ([cues.md](cues.md)).

## Typed cues

With the `serde` feature (on `praxi` and `praxi-format`; `cue-sheet` turns it on too), a
cue converts into a game type with serde:

```rust
#[derive(Deserialize)]
enum GameCue {
    #[serde(rename = "subtitle")]
    Subtitle { text: String, speaker: Option<String> },
    #[serde(rename = "camera.shake")]
    Shake { strength: f32 },
    #[serde(rename = "sound")]
    Sound(SoundCue),
    #[serde(rename = "fade")]
    Fade,
}

match cue.parse::<GameCue>() {
    Ok(GameCue::Shake { strength }) => shake(strength),
    Ok(_) => {}
    Err(e) if e.is_unknown_type() => {}        // a cue type this game does not use
    Err(e) => warn!("{e}"),                    // cue "camera.shake": field "strength": ...
}
```

`CueEvent::parse::<T>()` and `Cue::parse::<T>()` (`praxi_format::cue::parse(kind, fields)`
underneath) read the cue as follows:

- **Enum** (serde's default, externally tagged representation): the cue type picks the
  variant by its serde name. A struct variant reads the fields as its members, a newtype
  variant reads them into its inner type, a unit variant ignores them. Tuple variants are
  an error. A type that is no variant is an "unknown type" error (`is_unknown_type()`),
  unless the enum has a `#[serde(other)]` unit variant, which then catches it.
- **Struct** or **map**: the fields are its members; the cue type is not looked at.
  `CueEvent::parse::<Shake>()` reads the fields of any cue.
- This replaces registering a handler per cue type: one enum lists the types the game
  knows, `match` dispatches them, and the compiler checks every arm.

Field values map to serde as follows:

| Value | Deserializes into |
|---|---|
| string | `String`, `&str` (borrowed from the cue), `char`, unit enum variants by name |
| integer | any integer type that holds it, `f32`, `f64` |
| float | `f32`, `f64` |
| bool | `bool` |
| bytes | `Vec<u8>`, `&[u8]` |
| any | `Option<T>` as `Some`; a missing field is `None` for `Option` members |

Everything else is serde's usual behaviour: `#[serde(default)]`, `rename`, `alias`,
`deny_unknown_fields` (an `x` / `y` position is two fields like any other, so a struct with
`deny_unknown_fields` must list them), `flatten`. A position reads into `x: f32, y: f32`
members; `CueEvent::position()` checks the range ([cues.md](cues.md#screen-position)).

Errors (`praxi_format::cue::ParseError`) name the cue type and, when there is one, the
field: `cue "camera.shake": field "strength": invalid type: string "hard", expected f32`,
`cue "subtitle": field "text": missing`. `kind`, `field` and `message` are public.

## Active cues

`Player::active_cues()` iterates over the cues active at the playhead: those with a
non-zero duration that have started and not ended, in start order, as the `CueEvent`s
they were emitted with. It follows the events: a cue is in it from the `advance` (or
`play`, `seek`, `replace_cues`) that queues its `Cue` event until the one that queues its
`CueEnd`, whether or not the events have been polled yet. Instantaneous cues are never
active. Disabled streams have no active cues.

A subtitle overlay is then

```rust
let text: Vec<&str> = player.active_cues().filter(|c| c.kind == "subtitle").filter_map(|c| c.get_str("text")).collect();
```

with no bookkeeping of `Cue` / `CueEnd` pairs.

## Upcoming cues

`Player::upcoming_cues(within)` lists the cues of enabled streams that have not been
emitted yet and start within `within` of the clock, in start order, each with the time
until it starts: `impl Iterator<Item = (Duration, &CueEvent)>`. A game preloads what they
trigger (a sound, a model) before they fire:

```rust
for (until, cue) in player.upcoming_cues(Duration::from_secs(2)) {
    if let Ok(GameCue::Sound(s)) = cue.parse() { sounds.preload(&s.name) }
}
```

- "Not emitted yet": after an `advance`, cues at or before the clock have been emitted and
  are not listed. Before the first `play`, cues at time 0 are listed with `until` zero.
  After a `seek`, cues at or before the target count as passed, as they do for emitting
  ([cues.md](cues.md#player-behaviour)).
- When looping, the window continues into the next pass: cues from the start of the file
  are listed after the end, with `until` counted across the loop. Each cue is listed at
  most once, however long `within` is.
- A finished player lists nothing.
- `CueEvent::time` is the cue's time in the file, as in its `Cue` event; the ids are the
  ones the `Cue` event will carry.

## Screen positions

A cue's position ([cues.md](cues.md#screen-position)) is in video coordinates; these
helpers map it to where the video is drawn.

**`praxi-wgpu`**: `Rect` is the pixel rectangle `draw` fills (for example
`Rect::fit(width, height, target_w, target_h)`, letterboxed).

- `Rect::point((x, y)) -> (f32, f32)`: the pixel of the render target at that position,
  `(rect.x + x * rect.w, rect.y + y * rect.h)`, y down.
- `Rect::position((px, py)) -> Option<(f32, f32)>`: the inverse, `None` outside the
  rectangle (for picking a position with the mouse).

**`praxi-bevy`**: the video image goes wherever the game puts it.

- `position_in_rect(position, rect: bevy::math::Rect) -> Vec2`: a point in a rectangle
  with y down, such as a UI node's rectangle or the window in logical pixels.
- `position_on_sprite(position, size: Vec2, anchor: Vec2, transform: &GlobalTransform) ->
  Vec3`: the world point on a sprite of `size` (its `custom_size`, or the video size) with
  `anchor` (the value of its `Anchor`, `Vec2::ZERO` for the centre) and transform: local
  `((x - 0.5) * w, (0.5 - y) * h) - anchor * size`, through the transform. World y is up,
  so `y = 0` is the top edge of the sprite.

## Bevy: typed cue messages

`PraxiCuePlugin::<T>` (with `T: DeserializeOwned + Clone + Send + Sync + 'static`, usually the
game's cue enum) sends every cue that parses into `T` as a `PraxiCue<T>` message:

```rust
#[derive(Message)]
pub struct PraxiCue<T> {
    pub entity: Entity,   // the PraxiVideo
    pub id: u64,          // matches the CueEnd in PraxiEvent
    pub stream: u16,
    pub time: Duration,
    pub duration: Duration,
    pub position: Option<(f32, f32)>,
    pub cue: T,
}
```

- Cue types `T` does not know are skipped silently; other parse errors are printed to
  stderr (naming the file, the cue type and the field), as the plugin's other errors are,
  and skipped.
- Several plugins with different `T` can be added (for example one per game system).
- System sets order it: `PraxiSystems::Advance` advances the players and sends
  `PraxiEvent`s, then `PraxiSystems::Cues` sends `PraxiCue<T>`s, both in `Update`. Systems
  that read the messages run `.after(PraxiSystems::Cues)` to see them in the same frame.

## Example game

`crates/praxi-bevy/examples/cue_game.rs` plays a video as a cutscene and consumes its
cues:

```sh
cargo run --release -p praxi-bevy --example cue_game -- intro.praxi [intro.cues.json]
make cue-game    # the 10 s Big Buck Bunny sample with crates/praxi-bevy/examples/cue_game.cues.json
```

- The video is a sprite fitted to the window, letterboxed, and refitted on resize. It
  starts playing, looping. Space plays or pauses, Left / Right seek 2 s, L toggles
  looping, Escape quits.
- With a sheet, its cues replace the file's (`replace_cues`), so the example runs on any
  file.
- Cue types, read through a `GameCue` enum and `PraxiCuePlugin<GameCue>`:

  | Type | Fields | The example |
  |---|---|---|
  | `subtitle` | `text`, optional `speaker` | Shown at the bottom while active (`speaker: text`), from `active_cues()`. With a position, shown there instead as a speech bubble (text on a dark background). |
  | `camera.shake` | `strength` (pixels), optional `frequency` (Hz, default 12) | Shakes the camera for the cue's duration, fading out, from `PraxiCue<GameCue>`; its `CueEnd` stops it. The HUD and subtitles are UI and stay still. |
  | `marker` | `label`, position | A ring (gizmo circles) at the position with its label, while active; instantaneous markers from `PraxiCue<GameCue>` for 1 s. |
  | `sound` | `name` | Listed in the HUD as "preloading NAME in 1.2 s" from `upcoming_cues(2 s)`, then "playing NAME" for 2 s (no audio is played). |

- A HUD at the top left shows the playhead, the play state, the types of the active cues
  and the sounds.
- Overlays (bubbles, markers) are `Text2d` entities kept per cue id, in world space, so
  they move with the video when it shakes. Seeking and looping clear the instantaneous
  markers and the sounds played.
- Other cue types are ignored; malformed ones are reported on stderr.
- The example needs Bevy's sprite, text, UI and gizmo features, which only the
  dev-dependencies turn on; the library keeps its small feature set.

## Tests

- `praxi-format`: parsing into enums (struct, newtype, unit, `#[serde(other)]`), structs,
  borrowed strings, bytes, options, integer to float, and the errors (unknown type, wrong
  type, missing field, out-of-range integer) with their messages.
- `praxi`: `active_cues` through play, overlapping cues, seek, loop, finish, disabling a
  stream and `replace_cues`; `upcoming_cues` before play, during play, with a disabled
  stream, at the loop seam and when finished, with the ids of the later events;
  `CueEvent::parse` on the cues of a file. The existing cue tests check that file cues and
  replaced cues still give the same events.
- `praxi-wgpu`: `Rect::point` and `Rect::position` round trip, outside points.
- `praxi-bevy`: `position_on_sprite` and `position_in_rect`; the headless app test also
  checks `PraxiCue<T>` messages with an enum `T`.
- The example game is a manual check, under Xvfb like the cue editor
  ([testing.md](testing.md#window-checks-under-xvfb)): subtitles, a positioned bubble,
  markers, the shake and sounds announced ahead, in screenshots of a full loop.
