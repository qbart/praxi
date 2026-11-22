SAMPLES_DIR := samples
BENCH_INPUT := $(SAMPLES_DIR)/big_buck_bunny_1080_10s.mp4
BENCH_ARGS := --frames 180 --variant bc7 --variant bc1 --variant astc6x6 --variant astc8x8

.PHONY: build test test-strict check check-windows samples samples-hdr bench bench-baseline examples examples-hdr cue-game video-box video-box-bunny package publish tag crates release-ready godot-addon godot-example

# The workspace version (docs/features/releasing.md).
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

build:
	cargo build --release

test:
	cargo test --workspace

# Tests that would skip without ffmpeg or a GPU fail instead (docs/features/testing.md).
test-strict:
	PRAXI_TEST_STRICT=1 cargo test --workspace

check:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy -p praxi-viewer -p praxi-editor --no-default-features --all-targets -- -D warnings
	$(MAKE) test

# Type-checks everything for Windows from another OS (rustup target add x86_64-pc-windows-gnu).
check-windows:
	cargo check --target x86_64-pc-windows-gnu --workspace --all-targets

samples: $(BENCH_INPUT)

$(SAMPLES_DIR)/big_buck_bunny_1080_10s.mp4:
	mkdir -p $(SAMPLES_DIR)
	curl -fL -o $@ https://test-videos.co.uk/vids/bigbuckbunny/mp4/h264/1080/Big_Buck_Bunny_1080_10s_10MB.mp4

# Real HDR10 footage, 685 MB (docs/features/hdr.md#sample-footage): Cosmos Laundromat
# (Blender Foundation, CC BY 3.0), HDR remaster by Netflix, from the AOM test set on Xiph.
HDR_INPUT := $(SAMPLES_DIR)/CosmosCaterpillar_2048x858p24_hdr10.y4m
HDR_MD5 := 793c7f42b657ca2b5755a46c853ce4a8

samples-hdr: $(HDR_INPUT)

$(HDR_INPUT):
	mkdir -p $(SAMPLES_DIR)
	curl -fL -o $@.part https://media.xiph.org/video/aomctc/test_set/hdr2_2k/CosmosCaterpillar_2048x858p24_hdr10.y4m
	(md5sum $@.part 2>/dev/null || md5 -r $@.part) | grep -q $(HDR_MD5)
	mv $@.part $@

HDR_EXAMPLE := target/cosmos.praxi

examples-hdr: samples-hdr
	mkdir -p target
	cargo run --release -p praxi-cli -- encode $(HDR_INPUT) -o $(HDR_EXAMPLE) --input-transfer pq --input-primaries bt2020 \
		--variant bc6h --variant bc7
	cargo run --release -p praxi-cli -- info $(HDR_EXAMPLE)

bench: samples
	mkdir -p target
	cargo run --release -p praxi-cli -- bench $(BENCH_INPUT) $(BENCH_ARGS) --json target/bench.json --baseline bench/baseline.json

bench-baseline: samples
	mkdir -p bench
	cargo run --release -p praxi-cli -- bench $(BENCH_INPUT) $(BENCH_ARGS) --json bench/baseline.json

EXAMPLE := target/example.praxi

examples: samples
	mkdir -p target
	cargo run --release -p praxi-cli -- encode $(BENCH_INPUT) -o $(EXAMPLE) --frames 120 --variant bc7 --variant astc6x6 \
		--cue "0.5+0.3=light.flash:color=red" --cue 1.2=marker
	cargo run --release -p praxi --example game_loop -- $(EXAMPLE)
	cargo run --release -p praxi --example decode_thread -- $(EXAMPLE) $(EXAMPLE)

# The cue game example (docs/features/cue-runtime.md#example-game) on the sample.
CUE_GAME := target/cue_game.praxi

$(CUE_GAME): $(BENCH_INPUT)
	mkdir -p target
	cargo run --release -p praxi-cli -- encode $(BENCH_INPUT) -o $@ --scale 1280x-1 --variant bc7 --variant motion

cue-game: $(CUE_GAME)
	cargo run --release -p praxi-bevy --example cue_game -- $(CUE_GAME) crates/praxi-bevy/examples/cue_game.cues.json

# The video on spinning boxes, without and with mipmaps (docs/features/bevy.md#video-box-example),
# on a moving test pattern: the sample's single slow shot looks still on a small box.
TEST_PATTERN := target/testsrc.praxi

$(TEST_PATTERN):
	mkdir -p target
	ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=1280x720:rate=30 -t 10 -pix_fmt yuv420p target/testsrc.mp4
	cargo run --release -p praxi-cli -- encode target/testsrc.mp4 -o $@ --variant bc7

video-box: $(TEST_PATTERN)
	cargo run --release -p praxi-bevy --example video_box -- $(TEST_PATTERN)

# The same on the Big Buck Bunny sample (make cue-game's file).
video-box-bunny: $(CUE_GAME)
	cargo run --release -p praxi-bevy --example video_box -- $(CUE_GAME)

# The Godot addon for this machine (docs/features/godot.md#addon).
GODOT_ADDON := target/godot/addons/praxi
ifeq ($(shell uname -s),Darwin)
GODOT_LIB := libpraxi_godot.dylib
GODOT_LIB_NAME := libpraxi_godot.macos.arm64.dylib
else
GODOT_LIB := libpraxi_godot.so
GODOT_LIB_NAME := libpraxi_godot.linux.x86_64.so
endif

godot-addon:
	cargo build --release -p praxi-godot
	mkdir -p $(GODOT_ADDON)/bin
	cp crates/praxi-godot/praxi.gdextension crates/praxi-godot/README.md LICENSE-MIT LICENSE-APACHE $(GODOT_ADDON)/
	cp target/release/$(GODOT_LIB) $(GODOT_ADDON)/bin/$(GODOT_LIB_NAME)

# The Godot example on the cue game file and its cues (docs/features/godot.md#example-project).
GODOT ?= godot
GODOT_EXAMPLE := examples/godot

godot-example: godot-addon $(CUE_GAME)
	mkdir -p $(GODOT_EXAMPLE)/addons/praxi $(GODOT_EXAMPLE)/.godot
	cp -r $(GODOT_ADDON)/. $(GODOT_EXAMPLE)/addons/praxi/
	cargo run --release -p praxi-cli -- cues import $(CUE_GAME) crates/praxi-bevy/examples/cue_game.cues.json -o $(GODOT_EXAMPLE)/intro.praxi
	# Godot loads the extensions listed here; the editor writes it when it scans the project.
	echo 'res://addons/praxi/praxi.gdextension' > $(GODOT_EXAMPLE)/.godot/extension_list.cfg
	$(GODOT) --path $(GODOT_EXAMPLE)

# Releasing (docs/features/releasing.md).
package:
	cargo publish --workspace --dry-run

tag:
	@test -z "$$(git status --porcelain)" || { echo "working tree not clean"; exit 1; }
	git tag -a v$(VERSION) -m "praxi $(VERSION)"
	git push origin v$(VERSION)

publish: release-ready
	$(MAKE) check
	@for crate in $(CRATES); do $(MAKE) --no-print-directory publish-$$crate || exit 1; done

# Crates in publishing order: each after the praxi crates it depends on.
CRATES := praxi-format praxi praxi-encoder praxi-wgpu praxi-audio praxi-bevy praxi-cli praxi-viewer praxi-editor

crates:
	@echo $(CRATES)

release-ready:
	@test -z "$$(git status --porcelain)" || { echo "working tree not clean"; exit 1; }
	@git rev-parse -q --verify "refs/tags/v$(VERSION)" > /dev/null || { echo "no tag v$(VERSION); run make tag"; exit 1; }
	@git diff --quiet "v$(VERSION)" HEAD -- crates Cargo.toml Cargo.lock || { echo "the crates changed since v$(VERSION)"; exit 1; }

# Whether $(VERSION) of crate $1 is in the crates.io index (index.crates.io/pr/ax/praxi-format).
published = curl -sf -A "praxi-release" "https://index.crates.io/$(shell echo $1 | cut -c1-2)/$(shell echo $1 | cut -c3-4)/$1" | grep -q '"vers":"$(VERSION)"'

publish-%: release-ready
	@echo " $(CRATES) " | grep -q " $* " || { echo "unknown crate $*; one of: $(CRATES)"; exit 1; }
	@if $(call published,$*); then echo "$* $(VERSION) is already published, skipping"; else cargo publish -p $*; fi

package-%:
	@echo " $(CRATES) " | grep -q " $* " || { echo "unknown crate $*; one of: $(CRATES)"; exit 1; }
	cargo publish -p $* --dry-run
