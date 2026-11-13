# A cutscene that consumes the cues of a praxi file: subtitles, markers on the video, camera
# shakes and sounds announced ahead (docs/features/godot.md#example-project).
extends Control

const FILE := "res://intro.praxi"

var stream: VideoStreamPraxi
var player := VideoStreamPlayer.new()
var frame := AspectRatioContainer.new()
var subtitle := Label.new()
var hud := Label.new()
var markers := Control.new()
## Instant cues shown for a moment: [label, position, until (msec)].
var flashes := []
var log_lines: Array[String] = []

func _ready() -> void:
	set_anchors_preset(PRESET_FULL_RECT)
	var background := ColorRect.new()
	background.color = Color.BLACK
	background.set_anchors_preset(PRESET_FULL_RECT)
	add_child(background)

	frame.set_anchors_preset(PRESET_FULL_RECT)
	add_child(frame)
	player.expand = true
	frame.add_child(player)
	markers.set_anchors_preset(PRESET_FULL_RECT)
	markers.mouse_filter = MOUSE_FILTER_IGNORE
	add_child(markers)

	subtitle.set_anchors_preset(PRESET_CENTER_BOTTOM)
	subtitle.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	subtitle.add_theme_font_size_override("font_size", 30)
	subtitle.add_theme_constant_override("outline_size", 8)
	subtitle.add_theme_color_override("font_outline_color", Color.BLACK)
	add_child(subtitle)
	hud.position = Vector2(12, 8)
	hud.add_theme_constant_override("outline_size", 6)
	hud.add_theme_color_override("font_outline_color", Color.BLACK)
	add_child(hud)

	stream = load(FILE)
	if stream == null:
		hud.text = "cannot load %s: run make godot-example" % FILE
		return
	stream.looping = true
	stream.cue.connect(_on_cue)
	stream.looped.connect(func(): log_event("looped"))
	player.stream = stream
	player.play()
	var tex := player.get_video_texture()
	if tex:
		frame.ratio = float(tex.get_width()) / tex.get_height()

## Cues as they happen: instant markers and sounds; the rest is read from active_cues().
func _on_cue(_id: int, type: String, fields: Dictionary, _time: float, duration: float, position: Variant) -> void:
	if duration == 0.0 and type == "marker" and position != null:
		flashes.append([fields.get("label", ""), position, Time.get_ticks_msec() + 1000])
	if type == "sound":
		log_event("sound: %s" % fields.get("name", "?"))

func log_event(line: String) -> void:
	log_lines.append(line)
	if log_lines.size() > 4:
		log_lines.pop_front()

## A point in 0..1 of the video, in this control's coordinates.
func on_video(p: Vector2) -> Vector2:
	var r := player.get_global_rect()
	return r.position + p * r.size - global_position

func _process(_delta: float) -> void:
	if stream == null:
		return
	var tex := player.get_video_texture()
	if tex:
		frame.ratio = float(tex.get_width()) / tex.get_height()
	var lines: Array[String] = []
	var shake := Vector2.ZERO
	var t := Time.get_ticks_msec() / 1000.0
	for child in markers.get_children():
		child.queue_free()
	for cue in stream.active_cues():
		var f: Dictionary = cue.fields
		match cue.type:
			"subtitle":
				var text: String = f.get("text", "")
				if cue.position != null:
					add_marker(text, cue.position, Color.WHITE)
				elif f.has("speaker"):
					lines.append("%s: %s" % [f.speaker, text])
				else:
					lines.append(text)
			"marker":
				if cue.position != null:
					add_marker(f.get("label", ""), cue.position, Color.ORANGE)
			"camera.shake":
				var strength: float = f.get("strength", 10.0)
				var freq: float = f.get("frequency", 20.0)
				shake += Vector2(sin(t * freq * TAU), cos(t * freq * TAU * 1.3)) * strength
	var now := Time.get_ticks_msec()
	flashes = flashes.filter(func(fl): return fl[2] > now)
	for fl in flashes:
		add_marker(fl[0], fl[1], Color.ORANGE)
	frame.position = shake
	subtitle.text = "\n".join(lines)
	subtitle.position = Vector2((size.x - subtitle.size.x) / 2, size.y - subtitle.size.y - 40)
	var upcoming := stream.upcoming_cues(2.0).filter(func(c): return c.type == "sound")
	var soon := ", ".join(upcoming.map(func(c): return "%s in %.1f s" % [c.fields.get("name", "?"), c.until]))
	hud.text = "%.2f / %.2f s  %s%s\n%s\n%s\nSpace play/pause  Left/Right seek 2 s  L loop  M mute  Esc quit" % [
		player.stream_position, player.get_stream_length(),
		"paused" if player.paused else "playing", ", looping" if stream.looping else "",
		"preloading " + soon if soon != "" else "",
		"  ".join(log_lines)]

func add_marker(text: String, p: Vector2, color: Color) -> void:
	var label := Label.new()
	label.text = "◎ " + text
	label.add_theme_color_override("font_color", color)
	label.add_theme_constant_override("outline_size", 6)
	label.add_theme_color_override("font_outline_color", Color.BLACK)
	label.add_theme_font_size_override("font_size", 24)
	markers.add_child(label)
	label.position = on_video(p) - Vector2(12, 16)

func _unhandled_key_input(event: InputEvent) -> void:
	var key := event as InputEventKey
	if key == null or not key.pressed or key.echo or stream == null:
		return
	match key.keycode:
		KEY_SPACE:
			player.paused = not player.paused
		KEY_LEFT:
			player.stream_position = maxf(player.stream_position - 2.0, 0.0)
		KEY_RIGHT:
			player.stream_position = minf(player.stream_position + 2.0, player.get_stream_length())
		KEY_L:
			stream.looping = not stream.looping
		KEY_M:
			AudioServer.set_bus_mute(0, not AudioServer.is_bus_mute(0))
		KEY_ESCAPE:
			get_tree().quit()
