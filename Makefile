SAMPLES_DIR := samples
BENCH_INPUT := $(SAMPLES_DIR)/big_buck_bunny_1080_10s.mp4
BENCH_ARGS := --frames 180 --variant bc7 --variant astc6x6 --variant astc8x8

.PHONY: build test samples bench bench-baseline

build:
	cargo build --release

test:
	cargo test --workspace

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
