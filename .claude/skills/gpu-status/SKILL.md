---
name: gpu-status
description: Quick read of NVIDIA GPU health for this project. Shows current state, Xid events across current and previous boots, the watcher log, and the last action we attempted. Read-only — does NOT load CUDA or touch the GPU beyond a metadata query. Use after any suspected crash, before any GPU-loading work, or when the user asks about Xid status.
---

# gpu-status

A read-only diagnostic for the voice-dictation project. Wraps `bin/gpu-status.sh`.

## When to use

- User asks "what's the GPU doing" / "any Xid?" / "is the GPU healthy"
- After ANY reboot in this project — to inspect what happened on the previous boot
- Before stepping up the Whisper model size in the GPU step-up plan
- Anytime the user is nervous about a repeat crash

## How to use

Run the script directly:

```bash
bash bin/gpu-status.sh
```

That's it. The script prints:

1. Uptime and recent boot history
2. Current GPU state via `nvidia-smi --query-gpu` (read-only metadata)
3. Xid events in the **current** boot
4. Xid events in the **previous** boot (critical for post-mortem after a crash)
5. Tail of our own `xid-events.log` watcher
6. The `last-action.txt` — what we were doing right before any crash
7. Tail of `actions.log` — recent test history
8. Whether the `gpu-watch.sh` background watcher is currently running

## Important context

This project had an **Xid 79 "GPU has fallen off the bus"** event on 2026-05-23 at 21:07:23 IST. That's a hardware-level error (typically 12VHPWR connector or PSU transient on RTX 4090 systems). Software cannot trigger it via read-only queries. See `bin/README.md` if present.

## What it does NOT do

- Does not run gpu-burn, furmark, or any compute workload
- Does not load CUDA / cuDNN
- Does not allocate VRAM beyond what was already allocated
- Cannot itself cause a crash

## State location

All persistent logs live in `~/.local/state/voice-dictation/`:
- `xid-events.log` — continuous watcher output
- `gpu-snapshots.log` — historical nvidia-smi snapshots
- `last-action.txt` — most recent attempted action
- `actions.log` — rolling history of all attempted actions
