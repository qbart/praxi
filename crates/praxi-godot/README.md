# praxi for Godot

Plays [praxi](https://github.com/qbart/praxi) video files in Godot 4.4 and newer. Frames
stay block-compressed on the GPU (BC7, BC1, ASTC 4x4, ASTC 8x8, and BC6H for HDR), audio plays through
`VideoStreamPlayer`, and timed cues stored in the video arrive as signals.

## Install

Copy `addons/praxi/` into your project and restart the editor. `.praxi` files then load as
`VideoStreamPraxi` and can be dropped onto a `VideoStreamPlayer`.

## Use

```gdscript
@onready var player: VideoStreamPlayer = $VideoStreamPlayer

func _ready():
    var stream: VideoStreamPraxi = load("res://intro.praxi")
    stream.looping = true                  # seamless, unlike VideoStreamPlayer.loop
    stream.cue.connect(_on_cue)
    player.stream = stream
    player.play()

func _on_cue(id, type, fields, time, duration, position):
    print(type, fields)                    # position: Vector2 in 0..1 of the video, or null
```

`stream.active_cues()` lists the cues active now, `stream.upcoming_cues(seconds)` those
about to start. Encode files with the `praxi` command line tool (`praxi encode input.mp4
-o intro.praxi --variant bc7 --variant astc4x4`); Godot cannot show ASTC 6x6 or the motion
codec yet.

Licensed under MIT or Apache-2.0.
