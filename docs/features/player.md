# Player

The `praxi` crate plays a `.praxi` file. It is pull based: the engine advances it once per
game frame and reads the current video frames and events. It owns no threads and does not
allocate per frame after start-up.

## API

```rust
let caps = Capabilities::new([TextureFormat::Bc7, TextureFormat::Astc6x6]);
let mut player = Player::open(File::open("intro.praxi")?, &caps)?;
player.set_looping(false);
player.play();

// every game frame
player.advance(dt)?;
while let Some(event) = player.poll_event() {
    match event {
        Event::Marker { name, .. } if name == "explosion" => shake_camera(),
        Event::Finished => switch_to_gameplay(),
        _ => {}
    }
}
for track in 0..player.tracks().len() {
    if let Some(frame) = player.video_frame(track) {
        if frame.index != last_uploaded[track] {
            upload(frame.format, frame.width, frame.height, frame.data);
        }
    }
}
```

- `Player::open(reader, &caps)` reads the header and stream table and selects one variant
  per group ([texture-variants.md](texture-variants.md)). The reader needs `Read + Seek`.
- `tracks()` lists the selected video streams, one per group, in group order.
- `play()`, `pause()`, `is_playing()`, `is_finished()`.
- `set_looping(bool)`: on reaching the end, restart from the beginning instead of finishing.
- `seek(time)`: jump to `time`. Decodes from the preceding keyframe of each track. Markers
  between the keyframe and `time` are not emitted.
- `advance(dt)`: when playing, move the clock forward by `dt` and decode every chunk whose
  time is at or before the clock. Paused players ignore `dt`.
- `position()` and `duration()`.
- `video_frame(track)`: the latest decoded frame of a track (`None` before its first
  frame): texture format, sRGB flag, width, height, block data, frame index and timestamp.
  `index` changes only when a new frame was decoded, so engines can skip redundant uploads.
- `poll_event()`: the next queued event, oldest first.

## Events

| Event | When |
|---|---|
| `Started` | `play()` from the beginning (stopped or finished). |
| `Paused` | `pause()` while playing. |
| `Resumed` | `play()` while paused mid-way. |
| `Seeked { to }` | After `seek()`. |
| `Marker { name, time }` | The clock passes a marker stored in the file. |
| `Looped` | Looping playback reached the end and restarted. |
| `Finished` | Non-looping playback reached the end. The last frame stays available. |

Events are queued in the order they happen within one `advance` call. Markers are stored
in the file as a marker stream ([PRAXI.md](../spec/PRAXI.md#marker-payload)) and are added
at encode time ([encoder.md](encoder.md)).

## Timing

Clock time starts at 0. A chunk with timestamp `pts` in a stream with time base `num/den`
is due at `pts * num / den` seconds. The end of the file is `duration` from the header.
When a loop wraps, the remaining `dt` carries over into the next pass, so long frames do not
drift.

## Errors

`open` fails on a malformed file, an unsupported major version, or a group with no
supported variant. `advance` and `seek` fail on I/O errors or corrupt chunks (for example a
delta frame without a preceding keyframe).
