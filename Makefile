SAMPLES_DIR := samples

.PHONY: build test samples

build:
	cargo build --release

test:
	cargo test --workspace

samples: $(SAMPLES_DIR)/big_buck_bunny_1080_10s.mp4

$(SAMPLES_DIR)/big_buck_bunny_1080_10s.mp4:
	mkdir -p $(SAMPLES_DIR)
	curl -fL -o $@ https://test-videos.co.uk/vids/bigbuckbunny/mp4/h264/1080/Big_Buck_Bunny_1080_10s_10MB.mp4
