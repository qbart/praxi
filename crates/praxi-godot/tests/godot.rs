//! Runs the extension in headless Godot (`docs/features/godot.md#tests`).

use praxi_encoder::{AudioTrackConfig, ColorConfig, CueConfig, CueStreamConfig, Encoder, EncoderConfig, SampleFormat, VariantConfig};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Skips a test that cannot run here, or fails it when `PRAXI_TEST_STRICT` is set.
fn skip(why: &str) {
    assert!(std::env::var_os("PRAXI_TEST_STRICT").is_none(), "PRAXI_TEST_STRICT: {why}");
    eprintln!("skipped: {why}");
}

/// `GODOT`, or `godot` / `godot4` on `PATH`.
fn godot() -> Option<PathBuf> {
    if let Some(g) = std::env::var_os("GODOT") {
        return Some(g.into());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).flat_map(|d| ["godot", "godot4"].map(|n| d.join(n))).find(|p| p.is_file())
}

/// Builds the extension and returns the library.
fn build_extension() -> PathBuf {
    let status = Command::new(env!("CARGO")).args(["build", "-p", "praxi-godot"]).status().expect("cargo runs");
    assert!(status.success(), "building praxi-godot failed");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let lib = target.join("debug").join(format!("{}praxi_godot{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX));
    assert!(lib.is_file(), "{} is missing", lib.display());
    lib
}

/// A 2 s 64x32 BC7 clip at 30 fps with a stereo 48 kHz track and two cues.
fn clip(path: &Path) {
    let audio: Vec<f32> = (0..2 * 2 * 48_000).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.2).collect();
    let cues = CueStreamConfig::new("events")
        .cue(CueConfig::new(0.5, "marker").duration(0.5).field("label", "a").field("x", 0.25).field("y", 0.75))
        .cue(CueConfig::new(1.0, "camera.shake").field("strength", 0.8).field("count", 3i64).field("flag", true));
    let config = EncoderConfig::new(64, 32, 30, 1)
        .variant(VariantConfig::bc7())
        .audio(AudioTrackConfig::from_samples("main", 48_000, 2, audio).format(SampleFormat::F32))
        .cue_stream(cues);
    let mut enc = Encoder::new(std::fs::File::create(path).unwrap(), config).unwrap();
    for i in 0..60u8 {
        enc.push_frame(&[i.wrapping_mul(4); 64 * 32 * 4]).unwrap();
    }
    enc.finish().unwrap();
}

/// A short 64x32 clip with a BC6H and a BC7 variant.
fn hdr_clip(path: &Path) {
    let config = EncoderConfig::new(64, 32, 30, 1).color(ColorConfig::hdr()).variant(VariantConfig::bc6h()).variant(VariantConfig::bc7());
    let mut enc = Encoder::new(std::fs::File::create(path).unwrap(), config).unwrap();
    for _ in 0..3 {
        enc.push_frame_hdr(&[2.0, 1.0, 0.5, 1.0].repeat(64 * 32)).unwrap();
    }
    enc.finish().unwrap();
}

const SCRIPT: &str = r#"extends SceneTree

var player: VideoStreamPlayer
var stream
var events := []
var frames := 0
var failed := ""
var paused_at := 0.0
var step := 0
var first: Texture2D
var wait_until := 0

func check(ok: bool, what: String) -> void:
	if not ok and failed == "":
		failed = what

func _initialize() -> void:
	stream = load("res://clip.praxi")
	check(stream != null and stream.get_class() == "VideoStreamPraxi", "load() gives a VideoStreamPraxi: %s" % [stream])
	if failed != "":
		return
	stream.formats = PackedStringArray(["bc7"])
	stream.cue.connect(func(id, t, fields, time, duration, position): events.append(["cue", id, t, fields, time, duration, position]))
	stream.cue_ended.connect(func(id): events.append(["end", id]))
	stream.looped.connect(func(): events.append(["looped"]))
	player = VideoStreamPlayer.new()
	player.stream = stream
	# VideoStreamPlayer draws the texture it gets here, so it must exist before any frame.
	first = player.get_video_texture()
	check(first != null and first.get_width() == 64 and first.get_height() == 32, "texture when the stream is set: %s" % [first])
	player.autoplay = true  # starts when it enters the tree, after _initialize
	root.add_child(player)
	# The HDR clip: BC6H with hdr, BC7 without (docs/features/hdr.md#godot).
	for hdr in [true, false]:
		var hdr_stream = load("res://hdr.praxi").duplicate()
		hdr_stream.formats = PackedStringArray(["bc7", "bc6h"])
		hdr_stream.hdr = hdr
		var hdr_player := VideoStreamPlayer.new()
		hdr_player.stream = hdr_stream
		var tex = hdr_player.get_video_texture()
		var format = tex.get_image().get_format() if tex != null and tex.get_image() != null else -1
		var expected = Image.FORMAT_BPTC_RGBFU if hdr else Image.FORMAT_BPTC_RGBA
		check(format == expected, "hdr %s: image format %d, expected %d" % [hdr, format, expected])
		hdr_player.free()
	check(is_equal_approx(player.get_stream_length(), 2.0), "length %f" % player.get_stream_length())

func _process(_delta: float) -> bool:
	if player == null:
		check(false, "setup did not finish")
	frames += 1
	check(frames < 1500, "timed out in step %d" % step)
	if failed != "":
		return finish()
	var pos := player.stream_position
	if step == 0 and pos >= 0.6:
		var active = stream.active_cues()
		check(active.size() == 1 and active[0].type == "marker", "active at %f: %s" % [pos, active])
		var upcoming = stream.upcoming_cues(1.0)
		check(upcoming.size() == 1 and upcoming[0].type == "camera.shake" and upcoming[0].until > 0.0, "upcoming at %f: %s" % [pos, upcoming])
		var tex = player.get_video_texture()
		check(tex != null and tex == first, "the same texture, updated in place: %s" % [tex])
		# The clip's tone peaks at 0.2 (-14 dB); silence is -200 dB.
		var peak := AudioServer.get_bus_peak_volume_left_db(0, 0)
		check(peak > -40.0, "audio on the master bus: %f dB" % peak)
		step = 1
	elif step == 1 and pos >= 1.3:
		check(events.size() == 3, "events at %f: %s" % [pos, events])
		if events.size() == 3:
			var marker = events[0]
			check(marker[0] == "cue" and marker[2] == "marker" and marker[3].label == "a", "marker %s" % [marker])
			check(is_equal_approx(marker[4], 0.5) and is_equal_approx(marker[5], 0.5), "marker times %s" % [marker])
			check(marker[6] is Vector2 and marker[6].is_equal_approx(Vector2(0.25, 0.75)), "marker position %s" % [marker[6]])
			check(events[1][0] == "end" and events[1][1] == marker[1], "end %s" % [events[1]])
			var shake = events[2]
			check(shake[0] == "cue" and shake[2] == "camera.shake" and shake[6] == null, "shake %s" % [shake])
			check(typeof(shake[3].strength) == TYPE_FLOAT and shake[3].count == 3 and typeof(shake[3].count) == TYPE_INT and shake[3].flag == true, "shake fields %s" % [shake[3]])
		player.stream_position = 0.2
		step = 2
	elif step == 2:
		check(pos >= 0.2 and pos < 0.4, "after seeking to 0.2: %f" % pos)
		player.paused = true
		paused_at = pos
		wait_until = frames + 10
		step = 3
	elif step == 3 and frames >= wait_until:
		check(is_equal_approx(pos, paused_at), "paused at %f, now %f" % [paused_at, pos])
		player.paused = false
		stream.looping = true
		step = 4
	elif step == 4 and events.has(["looped"]):
		check(player.is_playing() and pos < 1.0, "playing after the loop at %f" % pos)
		player.stop()
		step = 5
	elif step == 5:
		check(not player.is_playing(), "stopped")
		return finish()
	return false

func finish() -> bool:
	print("FAIL: " + failed if failed != "" else "PASS")
	return true
"#;

#[test]
fn plays_in_godot() {
    let Some(godot) = godot() else { return skip("no Godot: set GODOT or put godot on PATH") };
    let lib = build_extension();
    let project = std::env::temp_dir().join(format!("praxi-godot-test-{}", std::process::id()));
    let addon = project.join("addons/praxi");
    std::fs::create_dir_all(addon.join("bin")).unwrap();
    std::fs::create_dir_all(project.join(".godot")).unwrap();
    let lib_name = lib.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::copy(&lib, addon.join("bin").join(&lib_name)).unwrap();
    let platform = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    std::fs::write(
        addon.join("praxi.gdextension"),
        format!(
            "[configuration]\nentry_symbol = \"gdext_rust_init\"\ncompatibility_minimum = \"4.4\"\n\n[libraries]\n{platform} = \"res://addons/praxi/bin/{lib_name}\"\n"
        ),
    )
    .unwrap();
    // Godot loads the extensions listed here; the editor writes it when it scans a project.
    std::fs::write(project.join(".godot/extension_list.cfg"), "res://addons/praxi/praxi.gdextension\n").unwrap();
    std::fs::write(project.join("project.godot"), "config_version=5\n\n[application]\nconfig/name=\"praxi test\"\n").unwrap();
    std::fs::write(project.join("test.gd"), SCRIPT).unwrap();
    clip(&project.join("clip.praxi"));
    hdr_clip(&project.join("hdr.praxi"));

    let mut child = Command::new(&godot)
        .args(["--headless", "--max-fps", "60", "--path"])
        .arg(&project)
        .args(["--script", "res://test.gd"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", godot.display()));
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(120) {
            child.kill().unwrap();
            panic!("Godot did not finish in 120 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    std::fs::remove_dir_all(&project).ok();
    let passed = stdout.lines().any(|l| l == "PASS") && !stderr.contains("SCRIPT ERROR") && !stdout.contains("SCRIPT ERROR");
    assert!(passed, "Godot output:\n{stdout}\n{stderr}");
}
