SAMPLES_DIR := samples
BENCH_INPUT := $(SAMPLES_DIR)/big_buck_bunny_1080_10s.mp4
BENCH_ARGS := --frames 180 --variant bc7 --variant bc1 --variant astc6x6 --variant astc8x8

.PHONY: build test test-strict check samples bench bench-baseline examples

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

samples: $(BENCH_INPUT)

$(SAMPLES_DIR)/big_buck_bunny_1080_10s.mp4:
	mkdir -p $(SAMPLES_DIR)
	curl -fL -o $@ https://test-videos.co.uk/vids/bigbuckbunny/mp4/h264/1080/Big_Buck_Bunny_1080_10s_10MB.mp4

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
