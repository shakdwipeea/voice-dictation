# macOS Port — Phase Tracker

Single source of truth for phase status, owned by the agent(s) working the
port. Parallel workers update their own row when the phase verifier passes.
The final integrator runs `scripts/macos-port/verify-all.sh`.

Authoritative plan: `docs/macos-port-plan.md`. Phase acceptance criteria and
verification: `scripts/macos-port/verify-phase.sh`.

| Phase | Status | Verifier | Owner | Notes |
| --- | --- | --- | --- | --- |
| 0 ASR feasibility (Nemotron offline MPS default) | ✅ done | `verify-phase.sh 0` | — | MPS is default; HF real-speech run: 1.015s p50 / 2.123s p95 over 5 clips, faster than CPU with same WER. See `docs/macos-phase-0-results.md` |
| 1 build skeleton (cfg-gate, facade, stubs) | ✅ done | `verify-phase.sh 1` | — | builds + clippy + tests green on macOS |
| 2 CoreAudio capture | ✅ done (compile) | `verify-phase.sh 2` | | live capture needs Microphone TCC |
| 3 CGEventTap hotkey | ✅ done (compile) | `verify-phase.sh 3` | | live tap needs Accessibility/Input Monitoring TCC |
| 4 CGEvent insertion + clipboard + focus | ✅ done (compile) | `verify-phase.sh 4` | | live insertion needs Accessibility TCC |
| 5 offline Nemotron sidecar + wiring | ✅ done | `verify-phase.sh 5` | — | `nemotron_offline` backend wired |
| 6 overlay UI (NSPanel) | ✅ done | `verify-phase.sh 6` | | native Swift NSPanel overlay sidecar wired via `overlay_backend=macos`; visual screenshot captured |
| 7 launchd + install-macos.sh | ✅ done (assets) | `verify-phase.sh 7` | | `launchctl load` is an interactive install step |
| 8 docs | ✅ done | `verify-phase.sh 8` | | plan + phase0 + README macOS section |
| 9 Parakeet-MLX ASR migration | ✅ done | — | — | `parakeet_mlx_streaming` is the config-init default macOS backend (`profile_ms=560`); `parakeet_mlx_offline` is the stable fallback. NeMo `nemotron_offline` demoted to legacy. See `docs/parakeet-mlx-migration-plan.md` |

Legend: ✅ done · 🚧 in progress · ⬜ not started · ⛔ blocked

## Parallelization notes

- Phases 2, 3, 4 all live in `crates/sunoto-macos` and touch overlapping FFI
  files; run them as a **chain** (one worker) rather than parallel, or split
  by file (capture.rs / hotkey.rs / insertion.rs) with careful merge.
- Phase 6 (overlay), 7 (launchd/install), 8 (docs) are independent of the
  `sunoto-macos` FFI and can run **in parallel** with 2/3/4.
- Every worker MUST keep the universal gate green: `cargo build --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` on macOS, plus all Python suites.
- Do not break Linux: `sunoto-linux` real code is unchanged; stubs only
  affect non-Linux targets.
