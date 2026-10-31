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
        Event::Cue(cue) if cue.kind == "explosion" => shake_camera(),
        Event::Cue(cue) if cue.kind == "light.flash" => flash_on(cue.id, cue.get_str("color")),
        Event::CueEnd { id, .. } => flash_off(id),
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
- `seek(time)`: jump to `time`. Decodes from the preceding keyframe of each track.
  Instantaneous cues at or before `time` are not emitted; cues active at `time` are
  ([cues.md](cues.md#player-behaviour)).
- `advance(dt)`: when playing, move the clock forward by `dt` and decode every chunk whose
  time is at or before the clock. Paused players ignore `dt`.
- `position()` and `duration()`.
- `video_frame(track)`: the latest decoded frame of a track (`None` before its first
  frame): texture format, sRGB flag, width, height, block data, frame index and timestamp.
  `index` changes only when a new frame was decoded, so engines can skip redundant uploads.
- `cue_streams()` and `set_cue_stream_enabled(stream, bool)`: see [cues.md](cues.md).
- `audio_tracks()`, `audio_output(track)`, `set_audio_track_enabled(track, bool)` and
  `set_audio_sync_tolerance(d)`: see [audio.md](audio.md). Audio follows the game clock.
- `Capabilities::max_audio_channels(n)` limits the channels of selected audio variants.
- `poll_event()`: the next queued event, oldest first.

## Events

| Event | When |
|---|---|
| `Started` | `play()` from the beginning (stopped or finished). |
| `Paused` | `pause()` while playing. |
| `Resumed` | `play()` while paused mid-way. |
| `Seeked { to }` | After `seek()`. |
| `Cue(CueEvent)` | The clock reaches the start of a cue: `id`, `stream`, `time`, `duration`, `kind` (the cue type) and `fields`. |
| `CueEnd { id, stream }` | The clock reaches the end of a cue with a non-zero duration, or the cue is ended by seeking, looping, finishing or disabling its stream. |
| `Looped` | Looping playback reached the end and restarted. |
| `Finished` | Non-looping playback reached the end. The last frame stays available. |

Events are queued in the order they happen within one `advance` call. Cues and their
ordering rules are described in [cues.md](cues.md).

## Timing

Clock time starts at 0. A chunk with timestamp `pts` in a stream with time base `num/den`
is due at `pts * num / den` seconds; audio chunks are due `preload` earlier
([audio.md](audio.md#interleaving)). The end of the file is `duration` from the header.
When a loop wraps, the remaining `dt` carries over into the next pass, so long frames do not
drift.

## Errors

`open` fails on a malformed file, an unsupported version, or a video group with no
supported variant (every audio group has a selectable variant). `advance` and `seek` fail on I/O errors or corrupt chunks (for example a
delta frame without a preceding keyframe).
