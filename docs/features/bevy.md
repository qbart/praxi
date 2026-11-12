# Bevy

`praxi-bevy` plays praxi files in [Bevy](https://bevyengine.org) 0.20 on top of
`praxi-wgpu` ([engine-integration.md](engine-integration.md)). Bevy 0.20 uses `wgpu` 30, the
version `praxi-wgpu` is built on, so the video is drawn with Bevy's own device. An example
cue editor built with `bevy_egui` shows the plugin in a tool.

## Plugin

```rust
App::new()
    .add_plugins((DefaultPlugins, praxi_bevy::PraxiPlugin))
    .add_systems(Startup, |mut commands: Commands, adapter: Res<RenderAdapter>, mut images: ResMut<Assets<Image>>| {
        let video = praxi_bevy::PraxiVideo::open("intro.praxi".as_ref(), &adapter, &mut images).unwrap();
        commands.spawn(Sprite::from_image(video.image.clone()));
        commands.spawn(video);
    })
```

- `PraxiVideo` (component) owns a `Player` on a file, the `praxi-wgpu` renderer and an
  `Image` asset (`image`, `Rgba8UnormSrgb`, the size of video track 0) that shows the
  video. The image can be used like any other: on a sprite, in UI, as a material texture,
  or in egui through `bevy_egui`'s user textures.
- `PraxiVideo::open(path, adapter, images)` opens the file with the texture formats the
  adapter supports (`praxi_wgpu::capabilities`). Bevy requests every feature and limit
  of the adapter by default, which includes BC, ASTC and what the motion decoder needs.
  It starts paused at the first frame.
- `PraxiVideo::player()` gives the player: play, pause, seek, loop, cue streams,
  `replace_cues` and the rest of the player API ([player.md](player.md)).
- Every `Update`, the plugin advances each video's player by Bevy's `Time` delta and
  sends each player event as a `PraxiEvent { entity, event }` message, in the system set
  `PraxiSystems::Advance`. Read them with `MessageReader<PraxiEvent>`.
- `PraxiCuePlugin::<T>` also sends cues parsed into a game type as `PraxiCue<T>` messages
  (`PraxiSystems::Cues`), and `position_on_sprite` / `position_in_rect` map cue positions
  to where the video is shown ([cue-runtime.md](cue-runtime.md)). The `cue_game` example
  consumes cues in a small game ([cue-runtime.md](cue-runtime.md#example-game)).
- In `PostUpdate`, the plugin uploads new frames (`VideoRenderer::prepare`) and draws video
  track 0 and the tracks blended over it into the video's own `Rgba8UnormSrgb` texture,
  through Bevy's `RenderDevice` and `RenderQueue`, which Bevy also puts in the main world.
- In the render world, before the frame is drawn, the plugin copies that texture into the
  `Image`'s GPU texture when a new frame was drawn. The copy stays on the GPU. The image
  asset is `RENDER_WORLD` only, so the main world keeps no pixels.
- Audio is not played: Bevy's audio cannot take a stream from `AudioOutput` without its
  own decoder. The player's clock runs on Bevy's time.
- `PraxiVideo::release_file()` closes the player's file, so that the file can be replaced
  (for example by writing cues into it), and `PraxiVideo::reopen()` opens it again at the
  same position, play state and loop setting, as the editor does
  ([cue-editor.md](cue-editor.md#saving)).

The player is not `Sync`, so the component holds it in a `SyncCell`: systems that query
`&mut PraxiVideo` get it; `&PraxiVideo` cannot reach it.

## Cue editor example

```sh
cargo run --release -p praxi-bevy --example cue_editor -- intro.praxi [intro.cues.json]
```

A simple cue editor: the full editor is `praxi-editor` ([cue-editor.md](cue-editor.md)).

- **Top bar**: play/pause, frame back and forward, position and duration, Save (writes the
  sheet), Write into file (writes the cues into the file through `release_file`,
  `replace_cues_in_file` and `reopen`), and the last status message.
- **Video** in the middle, letterboxed.
- **Timeline** at the bottom: a ruler and one lane per cue stream. Cues with a duration
  are bars, instantaneous cues are ticks, in their lane's colour from the same palette as
  the full editor ([cue-editor.md](cue-editor.md#window)). Click the ruler to seek, click
  a cue to select it, click an empty lane to select the stream. The playhead is a red
  line.
- **Inspector** on the right: the selected cue's start, duration, type and string fields
  (add, edit, remove), Delete, and "Set start to playhead". With a stream selected: its
  name, "Add cue at playhead", "Add stream". Below: the last 20 player events.
- **Screen positions**, as in the full editor
  ([cue-editor.md](cue-editor.md#screen-positions)):
  - Inspector: a Position row under Type. "On screen" gives the cue a position at the
    centre (`0.5, 0.5`) or removes `x` and `y`. When on, `x` and `y` are drag values in
    `0..=1` (three decimals), applied when the drag ends or the value loses focus, and
    **Pick** arms the picker. `x` and `y` also stay in the field list, read only.
  - Picker: while armed, the pointer over the video is a crosshair and a click sets the
    selected cue's position to the clicked point (clamped to the video) and disarms it.
    Pick again, Escape or selecting something else disarms it without a change.
  - Markers: every positioned cue active at the playhead (instantaneous cues for 0.5 s after
    their start), and the selected cue at any time, is a dot in its lane's colour with a
    dark outline and a label (type and first string field) on a dark box. The selected
    cue's dot is larger, with a ring.
  - Dragging the selected cue's dot moves it; the position is applied on release. Clicking
    another cue's dot selects that cue.
- Keys: Space plays or pauses, Left and Right step one frame, Delete removes the selected
  cue, Escape disarms the picker, when no text field has focus.
- The sheet is `FILE` with the extension `.cues.json` unless given. If it exists, the
  editor starts from it, otherwise from the cues in the file. Every edit plays back at once
  through `replace_cues`. There is no undo.
- Field values are edited as strings. Fields of other types are shown and kept, but not
  editable.

## Tests

`praxi-bevy` has a headless test that runs the plugin in a Bevy app without a window
(`MinimalPlugins`, `WindowPlugin` with no primary window, `AssetPlugin`, `RenderPlugin`,
`ImagePlugin`, `MeshPlugin`) when an adapter is available. For a motion clip, and a BC7
clip when the adapter samples BC, it plays for ten updates of 50 ms and checks that frames
were drawn, the latest drawn frame was copied into the image's GPU texture and the clip's
cue arrived as a `PraxiEvent` and, through `PraxiCuePlugin`, as one `PraxiCue<T>` of an
enum `T`. The copied pixels are not read back. Without an adapter it
skips, as the other GPU tests do ([testing.md](testing.md)).

The editor example is a manual check. It also runs under Xvfb on lavapipe:
`Xvfb :77 &`, then `DISPLAY=:77 cue_editor FILE`, with `xdotool` for input (`xdotool
windowfocus` first, since there is no window manager to give the window keyboard focus)
and `ffmpeg -f x11grab` for screenshots. Selecting, playing, editing a type, deleting,
Save and Write into file were checked that way, and so were positions: turning one on,
picking, dragging a marker, selecting a cue by its marker and saving the position to the
sheet.
