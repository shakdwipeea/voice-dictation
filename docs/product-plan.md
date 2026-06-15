# Sunoto: Local-First Voice for Coding Agents on Linux

Talk to terminal coding agents instead of typing at them. Hold a global
shortcut, speak, release — clean, agent-aware text lands at the focused cursor.
Local-first: no cloud, no API keys.

> Hold shortcut → speak → release → clean text at the cursor, with spoken file
> references rewritten to the focused agent's mention syntax (e.g. Claude Code
> `@path`).

Sunoto began as a Wispr-Flow-style Linux dictation app; that engine remains and
works system-wide, but the **product is now a voice layer for coding agents** —
Claude Code shipped, OpenAI Codex / Aider / Gemini CLI next. Talking beats
typing for long agent prompts, those agents are terminal-native, and none ships
first-class voice input on Linux.

*(Detailed feasibility, phase results, and the agent design live in the other
`docs/` files — `claude-code-integration-plan.md`, `phase-*-results.md` — and in
git history.)*

## Capabilities

- Always-warm local ASR on the latency path (no cloud)
- Filler / abandoned-phrase removal, mid-sentence corrections, punctuation
- **Agent awareness:** detect the focused agent + its working directory, rewrite
  spoken file references to that agent's syntax
- Personal dictionary, snippets, app-aware and developer-aware formatting
- Reliable insertion across Linux apps; local processing and storage

## Coding-agent integration (current focus)

An *agent adapter* describes each agent and is **config-driven** (settings, not
code):

- **Detection** — process name under the focused terminal (`claude`, `codex`,
  …), read from `/proc`.
- **Working directory** — in order: (1) the agent's own hook writes an active-cwd
  hint (Claude Code: `UserPromptSubmit`/`SessionStart` →
  `bin/sunoto-claude-cwd-hook.sh`); (2) `/proc/<pid>/cwd` for a single session;
  (3) a shell hint or pinned project otherwise.
- **Reference syntax** — per-agent template (Claude Code `@{path}`; others
  configurable, confirm on integration).
- **Commands** *(later)* — spoken agent commands (accept / reject / run),
  injection-safe.

The spoken-file matcher is **agent-agnostic** (four tiers, unique-match-only;
see `claude-code-integration-plan.md`) — only the cwd and output syntax differ.

| Agent | process | cwd source | file ref → | status |
| --- | --- | --- | --- | --- |
| Claude Code | `claude` | hook hint | `@path` | **shipped** |
| OpenAI Codex CLI | `codex` | `/proc`; hook if any | `@path` *(verify)* | planned |
| Aider | `aider` | `/proc` | `/add path` *(verify)* | planned |
| Gemini CLI | `gemini` | `/proc` | `@path` *(verify)* | planned |
| generic | configurable | `/proc` or shell hint | configurable | config-only |

**Limitation:** gnome-terminal shares one process across tabs, so multiple
*hookless* sessions under one terminal can't be told apart — the hint
disambiguates when present, else the daemon refuses to guess.

## How it works

```
hotkey → mic capture (+pre-roll) → streaming ASR (Nemotron) → deterministic polish → insert at cursor
                                                            ↘ agent-aware file-ref resolution ↗
```

- **ASR:** `nvidia/nemotron-speech-streaming-en-0.6b`, a cache-aware streaming
  RNNT kept warm on the GPU. Profiles: 80 ms (fastest) / 160 ms (default) /
  560 ms (most accurate); English-only, native punctuation. The backend is a
  replaceable interface with a non-NVIDIA fallback.
- **Daemon:** Rust, owns hotkey / capture / insertion; Python sidecars for ASR
  and the GTK overlay, managed over typed NDJSON pipes.
- **Polish (deterministic, <20 ms):** normalize → corrections → fillers →
  dictionary → snippets → app style. File-reference resolution runs *after*
  polish, gated on the focused agent. No LLM on the latency path; optional
  local-LLM polish, if enabled, runs only on demand after insertion.
- **Insertion:** X11 XTEST direct; Wayland paste / `wtype`; clipboard fallback
  with restore.

**Latency-first:** model warm at startup (never on hotkey); mic stream stays
open with a short pre-roll; push-to-talk release ends the utterance (no VAD
wait); resampling/IPC/cleanup/insertion off the UI thread; only the final result
is inserted.

**Injection safety:** Enter/Tab neutralized by default; focus captured at
release and revalidated before typing (else parked on the clipboard); held
modifiers released around injection; password fields disable insertion where
detectable; resolved `@paths` are repo-relative and local-only.

## Status & roadmap

- **Done:** Rust daemon, X11 push-to-talk + insertion, always-warm Nemotron,
  deterministic polish (dictionary / snippets / app-styles), GTK overlay,
  systemd packaging, and the **Claude Code adapter** (default-off).
- **Now — coding-agent layer:** generalize `DevTool` into the config-driven
  adapter registry; add Codex / Aider / Gemini; overlay "↳ N file refs"; spoken
  developer formatting; a reference-resolution corpus + eval.
- **Later:** Wayland adapters (GNOME / KDE / Hyprland via portals + AT-SPI),
  settings UI + tray, `.deb` / AppImage packaging, dictation history, warm-start
  reduction (16–32 s → <10 s goal).

## Performance (measured, RTX 3060, X11, 160 ms profile)

- Release-to-insertion **151 ms p95** (104 ms at the 80 ms profile)
- Release-to-final ASR 125 ms p95 · deterministic polish microseconds ·
  insertion 26 ms p95
- Zero-edit rate on the scripted corpus: **96 % polished vs 28 % raw**
- Sidecar VRAM ~3.6 GiB · warm start 16–32 s (optimization pending)
- No network after model download; local storage by default

## Main risks

| Risk | Mitigation |
| --- | --- |
| Per-agent file syntax varies / unknown | Config-driven adapter + unique-match-only resolution; verify on integration |
| Multiple sessions under one terminal | Active-cwd hook hint; refuse to guess otherwise |
| ASR mangles file names | Four-tier matcher tolerant of spoken separators and a one-word slip |
| Wayland blocks universal insertion | Portals + AT-SPI with capability-specific fallbacks |
| GPU / NVIDIA unavailable | Replaceable backend + fallback; clear diagnostics |
| Wrong or unwanted rewrite | Default-off, unique-match-only, repo-relative, local-only |
