.PHONY: phase0-check phase0-test phase0-x11 phase0-nemotron-script phase0-nemotron \
	phase1-test phase1-check phase1-selftest phase1-run phase1-run-nemotron \
	phase1-bench-mock phase1-bench-nemotron phase2-test phase2-eval \
	phase2-record phase2-transcribe phase2-eval-recorded ui-test ui-demo test

PYTHON ?= python3
BUILD_DIR ?= build/phase0/nemotron
NEMO_PYTHON ?= .venv-nemotron/bin/python
NEMO_COMMIT ?= c9040511b2dbefe64767d9b8853b3a20d63a2cd2
NEMO_SOURCE_ROOT ?= $(BUILD_DIR)/nemo-source
NEMO_STREAMING_SCRIPT := $(NEMO_SOURCE_ROOT)/examples/asr/asr_cache_aware_streaming/speech_to_text_cache_aware_streaming_infer.py
NEMOTRON_MODEL ?= nvidia/nemotron-speech-streaming-en-0.6b
NEMOTRON_AUDIO ?= tests/corpus/hf-sample1.wav

phase0-x11:
	mkdir -p $(BUILD_DIR)
	cc -O2 -Wall -Wextra -Werror tools/phase0/x11_probe.c -o $(BUILD_DIR)/x11_probe $$(pkg-config --cflags --libs x11 xtst)

phase0-check: phase0-x11
	mkdir -p $(BUILD_DIR)
	$(PYTHON) tools/phase0/system_probe.py --x11-probe $(BUILD_DIR)/x11_probe --output $(BUILD_DIR)/system-probe.json
	$(PYTHON) services/asr/nemotron_benchmark.py preflight --output $(BUILD_DIR)/nemotron-preflight.json

phase0-audio:
	mkdir -p $(BUILD_DIR)
	$(PYTHON) tools/phase0/audio_probe.py --output $(BUILD_DIR)/audio-probe.wav --report $(BUILD_DIR)/audio-probe.json

phase0-test:
	$(PYTHON) -m unittest discover -s tests/phase0 -v

$(NEMO_STREAMING_SCRIPT):
	mkdir -p $(@D)
	curl --fail --location --retry 3 --output $@ https://raw.githubusercontent.com/NVIDIA/NeMo/$(NEMO_COMMIT)/examples/asr/asr_cache_aware_streaming/speech_to_text_cache_aware_streaming_infer.py

phase0-nemotron-script: $(NEMO_STREAMING_SCRIPT)

phase0-nemotron: phase0-nemotron-script
	$(NEMO_PYTHON) services/asr/nemotron_benchmark.py benchmark \
		--nemo-root $(NEMO_SOURCE_ROOT) \
		--model $(NEMOTRON_MODEL) \
		--python $(NEMO_PYTHON) \
		--audio $(NEMOTRON_AUDIO) \
		--profiles 80 160 560 \
		--output-dir $(BUILD_DIR)

phase1-test:
	cargo test --workspace --offline
	cargo clippy --workspace --offline --all-targets -- -D warnings
	$(PYTHON) -m unittest discover -s tests/phase1 -v

phase1-check:
	cargo run --offline -p sunoto-daemon -- check

phase1-selftest:
	cargo run --offline -p sunoto-daemon -- selftest

phase1-run:
	cargo run --offline -p sunoto-daemon -- run --backend mock

phase1-run-nemotron:
	cargo run --offline -p sunoto-daemon -- run --backend nemotron

phase1-bench-mock:
	cargo run --offline -p sunoto-daemon -- bench --backend mock --sessions 5 --unpaced \
		--output build/phase1/bench-mock.json

phase1-bench-nemotron:
	cargo run --offline -p sunoto-daemon -- bench --backend nemotron --sessions 5 \
		--output build/phase1/bench-nemotron-160-paced.json

phase2-test:
	cargo test -p sunoto-polish --offline
	$(PYTHON) -m unittest discover -s tests/phase2 -v

phase2-eval:
	cargo run --offline -p sunoto-daemon -- eval --output build/phase2/eval-scripted.json

phase2-record:
	$(PYTHON) tools/phase2/record_corpus.py

phase2-transcribe:
	$(PYTHON) tools/phase2/transcribe_corpus.py

phase2-eval-recorded:
	cargo run --offline -p sunoto-daemon -- eval \
		--corpus tests/corpus/phase2-recorded/corpus-recorded.json \
		--output build/phase2/eval-recorded.json

ui-test:
	$(PYTHON) -m unittest discover -s tests/ui -v

# Drive the overlay by hand: shows the pill, animates the meter, hides.
ui-demo:
	printf '%s\n' \
		'{"type":"show"}' \
		'{"type":"recording","elapsed_s":0.5,"peak":0.3,"rms":0.04,"segments":1}' \
		'{"type":"recording","elapsed_s":1.0,"peak":0.6,"rms":0.08,"segments":1}' \
		'{"type":"status","text":"transcribing"}' \
		'{"type":"hide"}' \
		'{"type":"shutdown"}' \
		| { while read -r line; do echo "$$line"; sleep 1; done; } \
		| PYTHONPATH=src $(PYTHON) -m voice_dictation.ui_sidecar

test: phase0-test phase1-test phase2-test ui-test
