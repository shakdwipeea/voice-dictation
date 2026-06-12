# Phase 0 Results and Observations

**Date:** June 11, 2026  
**Status:** Highest-risk feasibility paths pass; interactive latency exit gate
still requires a persistent streaming-sidecar benchmark.

## Scope

Phase 0 was intended to prove the two highest-risk paths:

1. Local cache-aware Nemotron streaming on the target RTX 3060.
2. Linux X11 hotkey/text injection and low-latency microphone capture.

The desktop integration path passed. After rebooting from an NVIDIA
recovery-required state and removing an unintended mixed CUDA 12/13 runtime,
Nemotron completed the 80, 160, and 560 ms profile runs on the target RTX 3060.

## Implemented Artifacts

- `services/asr/nemotron_benchmark.py`
  - Runtime and CUDA preflight
  - Real CUDA allocation/synchronization check
  - WAV input validation
  - Wrapper around NeMo's official cache-aware streaming inference script
  - 80, 160, 560, and 1120 ms profile support
  - Transcription, streaming-process time, and real-time-factor JSON output
  - No fake WER when a reference transcript is not supplied
- `services/asr/setup_phase0_runtime.sh`
  - Reproducible isolated NeMo/CUDA 12.8 environment setup
- `tools/phase0/system_probe.py`
  - GPU, audio source, session, and X11 diagnostics
- `tools/phase0/audio_probe.py`
  - Auto-selects a physical microphone instead of a monitor source
  - Captures 16 kHz, mono, 16-bit PCM with 20 ms requested latency
- `tools/phase0/x11_probe.c`
  - X11/XTEST capability check
  - Isolated text-injection self-test
  - Ctrl+F8 global-hotkey probe
- Unit tests and `Makefile` Phase 0 commands
- Persistent pinned NeMo example and one-command `make phase0-nemotron` rerun

## Target Machine

| Item | Observed |
| --- | --- |
| OS | Linux Mint 22.2, kernel 6.8.0-88 |
| Desktop | Cinnamon on X11 |
| CPU | Intel i7-8700K, 6 cores / 12 threads |
| RAM | 32 GiB |
| GPU | NVIDIA GeForce RTX 3060, 12,288 MiB |
| Driver | 570.153.02 |
| Driver CUDA capability | CUDA 12.8 |
| Python | 3.12.3 |

## Results

### X11 Text Injection: Pass

- X11 display connection succeeded.
- XTEST extension version 2.2 is available.
- The isolated self-test created a private X11 window, injected
  `sunoto phase zero`, read the resulting key events, and verified an exact
  match.
- The C probe compiles with `-Wall -Wextra -Werror`.

**Conclusion:** X11 text injection is viable for the first vertical slice.

### Microphone Capture: Pass

The initial probe produced no frames because:

- The system default source was the speaker monitor rather than the physical
  microphone.
- The default PulseAudio buffering delayed frame delivery by roughly two
  seconds.

The probe was changed to select the first non-monitor source and request 20 ms
capture/process latency.

| Metric | Observed |
| --- | --- |
| Source | `alsa_input.pci-0000_00_1f.3.analog-stereo` |
| Requested duration | 1.5 seconds |
| Captured duration | 1.46 seconds |
| Format | 16 kHz, mono, 16-bit PCM WAV |
| Frames | 23,360 |
| RMS | 1,632.59 |
| Non-silent | Yes |

**Conclusion:** The machine can provide Nemotron-compatible low-latency audio.
The product must not trust the OS default source without checking whether it is
a monitor.

### Nemotron Runtime Setup: Pass

The model checkpoint was already present in the local Hugging Face cache:

- Model: `nvidia/nemotron-speech-streaming-en-0.6b`
- Checkpoint size: approximately 2.4 GiB

Installed isolated runtime:

- NeMo `3.1.0+c9040511b`
- PyTorch `2.7.1+cu128`
- CUDA runtime 12.8

Installing NeMo main initially selected PyTorch `2.12.0+cu130`, which the
installed NVIDIA 570 driver cannot run. Pinning PyTorch to the CUDA 12.8 build
allowed NeMo imports, CUDA discovery, and an initial CUDA preflight to succeed.
The setup must install both PyTorch and `cuda-python` 12.8 before NeMo;
replacing PyTorch after NeMo installation leaves CUDA 13 packages behind and
forces NeMo's full CUDA-graph decoder to use a slower fallback.

**Conclusion:** Runtime setup is feasible, but it must pin a CUDA build
compatible with the host driver. Unconstrained NeMo installation is not
acceptable for the product installer.

### Nemotron Streaming Benchmark: Compatibility Pass

The initial run failed after the GPU reported NVIDIA `Xid 31` and `Xid 154`
with **Node Reboot Required**. After reboot, all required profiles completed.
The setup was also corrected to remove CUDA 13 packages that forced NeMo's
optimized CUDA-graph decoder onto a slower fallback.

Results for one 13.69-second sample:

| Profile | Attention context | Streaming process | Real-time factor | Result |
| --- | --- | --- | --- | --- |
| 80 ms | `[70,0]` | 6.17 s | 0.4507 | Pass |
| 160 ms | `[70,1]` | 3.62 s | 0.2644 | Pass |
| 560 ms | `[70,6]` | 1.63 s | 0.1191 | Pass |

All three runs produced coherent transcriptions. No reference transcript was
supplied, so no WER claim is made. No CUDA-graph fallback warning or new NVIDIA
Xid error appeared during the corrected final run.

**Conclusion:** Nemotron compatibility and faster-than-real-time processing are
proven on this machine. These numbers are offline process timings, not
time-to-first-partial or release-to-final latency. The 600 ms product exit gate
therefore remains unproven until the model stays loaded and receives paced live
audio through a persistent sidecar.

## Storage Observation

| Item | Size |
| --- | --- |
| Workspace NeMo virtual environment | Approximately 7.5 GiB |
| Existing Nemotron checkpoint | Approximately 2.4 GiB |
| Temporary uv cache after repair | Approximately 6 MiB |
| Free disk after setup | Approximately 187 GiB |

The development environment is large. Production packaging should not ship the
full NeMo development stack without evaluating a smaller runtime such as Riva,
an exported engine, or a trimmed Python environment.

## Test Summary

- Python unit tests: **13 passed**
- Python files compile successfully.
- X11 C probe compiles with warnings treated as errors.
- X11 injection self-test: **passed**
- Microphone format/capture probe: **passed**
- Nemotron CUDA preflight after reboot: **passed**
- Nemotron 80/160/560 ms profile transcription: **passed**
- Interactive first-partial and release-to-final latency: **not yet measured**

## Phase Decision

Proceed with the X11 and audio architecture and keep the 160 ms profile as the
provisional default. The next Phase 0 task is a persistent streaming-sidecar
latency harness that measures time to first partial and release to final text;
the current process-per-profile wrapper cannot validate the product latency
gate.

Re-run the completed compatibility benchmark with:

```bash
make phase0-test
.venv-nemotron/bin/python services/asr/nemotron_benchmark.py preflight
make phase0-nemotron
```
