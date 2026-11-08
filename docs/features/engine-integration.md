# Engine integration

How a game engine plays praxi files, with runnable examples (milestone M4). The player API
is in [player.md](player.md), audio in [audio.md](audio.md), cues in [cues.md](cues.md).

## The frame loop

Each game frame, on the thread that owns the player:

1. `player.advance(dt)`: decodes every chunk that became due (video frames, audio, cues).
2. `while let Some(event) = player.poll_event()`: react to `Cue`, `CueEnd`, `Finished`,
   `Looped`...; events of one `advance` come in the order they happened.
3. For each video track, `player.video_frame(track)`: when its `index` differs from the
   last uploaded one, upload `data` to the track's texture. Otherwise skip the upload: at
   30 fps video and a 120 Hz game, three frames out of four upload nothing.
4. Draw the textures like any other.

Audio is read separately, from the engine's audio callback, through `AudioOutput` handles.

## Textures

- Build `Capabilities` from what the GPU samples: BC formats on desktop and consoles, ASTC
  on mobile and Apple silicon ([texture-variants.md](texture-variants.md)). The player
  picks one variant per video group and `tracks()` reports it.
- Create one texture per video track in the track's format (`*Srgb` when
  `VideoParams::srgb()`), sized up to whole blocks (`blocks_x * block_width` by `blocks_y *
  block_height`); crop the padding through texture coordinates.
- Upload the whole frame: rows of `blocks_x * block_bytes` bytes, `blocks_y` rows. The data
  is in the GPU's own layout; there is no conversion. A 1080p BC7 frame is 2 MB.
- Alpha is straight (not premultiplied): blend with `src_alpha, 1 - src_alpha`.

## Threads

The player is `Send`: it can live on the game thread or on a worker.

- **Game thread** (simplest): call `advance` in the frame loop. Decoding a 1080p frame
  costs 0.6-1.5 ms depending on variant and encoder settings
  ([video-codec.md](video-codec.md#decode-cost-and-writer-policy)), measured back to back.
  On a core that sleeps between frames, power management makes it slower: `game_loop` on
  the BC7 sample takes 2.5 ms per `advance` when it sleeps until the next 60 Hz frame and
  1.5-1.7 ms when it busy-waits instead.
- **Worker thread**: the worker owns the player, advances it on its own clock (or the game
  clock sent over a channel) and passes decoded frames back in recycled buffers. The game
  thread only uploads. The `decode_thread` example shows this.
- `AudioOutput` handles are `Clone + Send + Sync` and are read from the audio thread in
  either case.

## Seamless transitions

- **Preroll**: open the next video while the current scene still runs, `seek` it to the
  start (this decodes the first frame) and upload its first frame. When the scene changes,
  call `play()`: the first frame is already on screen, and no frame shows black while
  the file is opened. Without preroll, `play()` itself decodes the first frame and the
  first audio, so they are ready in the same game frame, but that frame pays the decode.
- **Fades**: draw the video over the game scene with an opacity that ramps up, and back
  down after `Finished` (the last frame stays available). With alpha video, the game shows
  through where the video is transparent.
- **Cues** drive gameplay from the timeline: a `light.flash` cue can flash the scene, a
  `subtitle` cue can show text; their meaning is up to the game.
- **Looping** backgrounds use `set_looping(true)`; the wrap is seamless for video and audio.

## Clocks

`Clock::Game` (default) advances by the `dt` the engine passes. For long videos with
continuous sound, `Clock::Audio` lets the video follow the sound card
([audio.md](audio.md#audio-master-clock)).

## `praxi-wgpu`

`crates/praxi-wgpu` is the texture management of the list above for engines built on
`wgpu`. The viewer ([viewer.md](viewer.md)) uses it as well.

```rust
let caps = praxi_wgpu::capabilities(&adapter, None)?;
let device_features = praxi_wgpu::required_features(&adapter);               // request these
let device_limits = praxi_wgpu::required_limits(&adapter, wgpu::Limits::default()); // and these
let mut player = Player::open(file, &caps)?;
let mut video = praxi_wgpu::VideoRenderer::new(&device, surface_format, &player);
// every frame:
player.advance(dt)?;
video.prepare(&device, &queue, &mut player, opacity);   // uploads or decodes changed frames, sets opacity
// inside the engine's render pass:
video.draw(&mut pass, praxi_wgpu::Rect::fit(width, height, target_w, target_h));
```

| Item | Meaning |
|---|---|
| `supported_formats(features)` | Texture formats `wgpu` features allow, most preferred first: BC7, BC3, BC1, BC5, BC4, then ASTC 4x4, 5x5, 6x6, 8x8. |
| `supports_motion(adapter)` | Whether the adapter can run the motion decoder's compute shaders ([motion-codec.md](motion-codec.md#gpu-decoder-praxi-wgpu)). |
| `capabilities(adapter, only)` | `Capabilities` from the adapter's features, then `motion` when supported, or only `only` (error if not supported). |
| `required_features(adapter)` | The texture compression features to request with the device. |
| `required_limits(adapter, base)` | `base` raised to what the motion decoder needs when the adapter supports it (`base` unchanged otherwise). Request the device with it whenever `capabilities` may contain `motion`: a device made with lower limits fails validation when a motion track's pipelines are created. |
| `wgpu_format(format, srgb)` | The `wgpu` texture format of a praxi texture format. |
| `Rect` | A pixel rectangle of the render target; `Rect::fit(w, h, target_w, target_h)` letterboxes. |
| `VideoRenderer::new(device, target_format, player)` | Pipelines for `target_format` and one texture per video track of `player`. One renderer per player. |
| `prepare(device, queue, player, opacity)` | Uploads the tracks whose frame changed and reconstructs the queued frames of `motion` tracks on the GPU; returns how many tracks changed. Sets the opacity (0 to 1) for the next draws. |
| `set_gpu_timing(device, enabled)`, `gpu_times(device, queue)` | Profiling: GPU time of each motion pass from timestamp queries (needs `Features::TIMESTAMP_QUERY`; `gpu_times` waits for the GPU) ([motion-codec.md](motion-codec.md#gpu-timing)). |
| `draw(pass, rect)` | Draws every track that has a frame into `rect`, in track order, each blended over what is already there (straight alpha times opacity). Sets the pass's viewport to `rect`. |
| `draw_replace(pass, rect)` | Like `draw`, but track 0 replaces the target pixels, alpha included, for offscreen rendering such as screenshots. |

## Examples

| Example | Run | Shows |
|---|---|---|
| `game_loop` (crate `praxi`, no dependencies) | `cargo run --release -p praxi --example game_loop -- FILE` | The frame loop above at a fixed 60 Hz without a GPU: variant selection, uploads skipped when the frame index did not change, events, an audio "device" pulling from `AudioOutput` on another thread. Prints a summary. |
| `decode_thread` (crate `praxi`, no dependencies) | `cargo run --release -p praxi --example decode_thread -- FILE [NEXT]` | The player on a worker thread, frames handed to the game thread in recycled buffers, events forwarded; with `NEXT`, a second file prerolled during the first and started on the next frame after the first finishes. |
| `cutscene` (crate `praxi-wgpu`, window) | `cargo run --release -p praxi-wgpu --example cutscene -- FILE` | Works with block and `motion` variants alike. An animated stand-in for gameplay; Space starts the prerolled video with a fade-in over it, Escape skips with a fade-out, the end fades out by itself. `light.flash` cues flash the screen in their `color` field (`red`, `green`, `blue`, `white`). `--auto` starts at once and quits after the fade-out. |

`motion_timing` (crate `praxi-wgpu`) times the GPU decoder on a `motion` file
([motion-codec.md](motion-codec.md#tools)).

`make examples` encodes the first 2 seconds of the sample video with two cues and runs
`game_loop` and `decode_thread` on it; CI runs it in the bench job.
