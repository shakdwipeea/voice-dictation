.PHONY: phase0-check phase0-test phase0-x11 phase0-nemotron-script phase0-nemotron

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
