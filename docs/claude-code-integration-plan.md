# Plan: Developer-Context Dictation & Claude Code File References

*June 15, 2026. A design + delivery plan for making dictation aware of the
focused developer tool — starting with the headline feature: when you dictate
into a terminal running **Claude Code**, spoken file references resolve to
Claude Code `@path` mentions. Mechanism generalizes to any tool with a
discoverable working directory.*

Scope and phase gates elsewhere live in `product-plan.md`; this covers one
feature family the product plan already names ("developer-aware formatting for
variables, file names, and CLI commands"; "use of nearby text as context").

## Status — June 15, 2026

Phases A and B are implemented on branch `feat/claude-code-file-references`,
shipped **default-off** (`polish.file_references.enabled = false`):

- **`sunoto-context`** (new crate): `/proc`-based `claude` detection + cwd, and
  a `git ls-files` / bounded-walk file index.
- **`sunoto-linux`**: focused-window `_NET_WM_PID` and `_NET_WM_NAME` reads via
  a new `window_context` (the WM_CLASS climb now reads all three at once).
- **`sunoto-polish`**: `resolve_file_references` (conservative,
  unique-match-only) plus `FileReferenceConfig`.
- **`sunoto-daemon`**: detects Claude Code at finalize (terminal windows only,
  so the `/proc` scan is skipped otherwise), caches the index per cwd, and
  rewrites references after the polish pass.

Verified: `cargo test --workspace` (including new resolver and `/proc` tests),
`cargo clippy --workspace -- -D warnings`, and the GPU-free Python protocol
suites all pass. A live end-to-end dictation pass with the feature enabled
still needs a human at the machine. Phase C (disambiguation UX, overlay
indication, other tools) remains future work.

## 1. What we're building

**User story.** Claude Code is running in a focused terminal. You hold the
hotkey and say:

> "look at the daemon file and the polish lib, then update the settings"

and the daemon inserts:

> look at `@apps/daemon/src/daemon.rs` and `@crates/sunoto-polish/src/lib.rs`,
> then update `@apps/daemon/src/settings.rs`

`@relative/path` is Claude Code's own file-mention syntax, resolved relative to
the session's working directory. The same context plumbing later unlocks
spoken CLI/identifier formatting in terminals and file references for plain
shells and editors.

## 2. Why it fits the existing architecture

We are extending two seams the daemon already has, not inventing a subsystem:

1. **Focused-window context at release.** `daemon.rs` already sends
   `UiCommand::CaptureFocus` on hotkey release and gets back
   `DaemonEvent::FocusClass(Option<(instance, class)>)`, used today only to
   pick a `StyleProfile`. We enrich that capture into a `DictationContext`.
2. **The deterministic polish pipeline.** `sunoto-polish::polish()` is a staged,
   microsecond-budget, app-aware text transform (normalize → corrections →
   fillers → dictionary → snippets → style). File-reference resolution is one
   more **gated** stage, active only when the context says a supported tool is
   focused.

No change to the latency contract: no LLM on the insertion path, all string
work, index pre-built off the hot path.

## 3. Feasibility — validated on this machine (X11, June 15 2026)

The load-bearing question was *"can the daemon reliably know Claude Code is
focused and find its working directory?"* Verified end to end against a live
session:

```
# Active window properties (what the daemon reads at release):
_NET_WM_PID(CARDINAL)   = 2129
WM_CLASS(STRING)        = "gnome-terminal-server", "Gnome-terminal"
_NET_WM_NAME(UTF8_STRING) = "⠂ Plan Claude Code CLI file integration features"

# Process tree from that window PID downward:
claude (635142) -> bash (619799) -> gnome-terminal- (2129)

# The prize — Claude Code's working directory, directly readable:
readlink /proc/635142/cwd  ->  /home/antash/workspace/voice-dictation
```

Findings:

- **Claude Code runs as a process literally named `claude`** (`/proc/<pid>/comm
  == "claude"`); its `/proc/<pid>/cwd` symlink gives the working directory.
- The window's `_NET_WM_PID` is the **terminal emulator**, not `claude`, so we
  walk *down* the `/proc` PPid tree to find the `claude` descendant.
- Claude Code also sets `_NET_WM_NAME` to the current task (with a leading
  braille spinner) — a useful *secondary* hint, but `/proc` is the robust
  detector.
- Everything is **local**: two extra X11 property reads plus `/proc` reads.
  Nothing leaves the machine.
- Cross-backend: on Wayland, `hyprctl activewindow -j` already returns the
  window `pid` (the daemon currently ignores it), so the identical `/proc`
  walk works there.

## 4. Design

### 4.1 Context detection — `DictationContext`

Generalize the focus capture. In `sunoto-linux`, alongside `read_wm_class`,
add reads during the **same parent-climb** that already finds WM_CLASS (the
top-level window typically carries all three properties):

- `_NET_WM_PID` (intern atom, read as `XA_CARDINAL`/32-bit) → window PID.
- `_NET_WM_NAME` (intern atom, read as `UTF8_STRING`) → window title.

The connection already exposes `atom(name)` for interning, so this mirrors the
existing `read_wm_class` exactly.

New module/crate `sunoto-context` (dependency-light, reads `/proc` directly):

```
struct DictationContext {
    class: Option<(String, String)>,   // existing WM_CLASS
    title: Option<String>,             // _NET_WM_NAME
    tool: Option<DevTool>,             // resolved below
}
enum DevTool { ClaudeCode { cwd: PathBuf }, /* VsCode, Shell, ... later */ }
```

Resolution: given the window PID, BFS the `/proc/*/status` `PPid:` links to
enumerate descendants; the first whose `comm == "claude"` yields
`ClaudeCode { cwd: readlink /proc/<pid>/cwd }`. Bounded depth/scan count so a
huge process table can never stall the capture thread.

`daemon.rs` change: `DaemonEvent::FocusClass(...)` becomes
`DaemonEvent::FocusContext(DictationContext)`; `resolve_style` keeps using
`.class`. Log one line per session (`claude-code context: cwd=…`) — same
diagnostic pattern as the existing `insertion target at release:` line.

### 4.2 File index — the matcher (in `sunoto-context`)

Given a `cwd`:

- Enumerate candidates with `git ls-files` (fast, respects `.gitignore`); fall
  back to a bounded recursive walk for non-git dirs. Cap at `max_index_files`
  and skip obvious noise (`.git`, `target`, `node_modules`, `.venv*`).
- Build a match index keyed by **basename, stem, and path components**, so
  "daemon" → `apps/daemon/src/daemon.rs` and "polish lib" → the lib under
  `sunoto-polish`.
- **Cache per `cwd`** with an mtime/TTL check and warm it in the **background**
  when a context is first seen (not on the release path). Optional inotify
  refresh later. Re-dictations into the same repo hit the cache.

### 4.3 Reference resolution — a gated polish stage

Add `resolve_file_references(text, &index, &cfg)` to `sunoto-polish`, run
**only** when `context.tool` matches a configured tool. It is pure string work
inside the deterministic budget:

1. Normalize spoken punctuation in candidate spans ("dot" → ".", "slash" → "/",
   "dash"/"underscore"), since ASR spells code-y names oddly.
2. Identify reference spans, cued by trigger words ("the X file", "open X",
   "look at X", "reference X") to avoid over-triggering. (A permissive,
   no-trigger mode can come later behind a flag.)
3. Fuzzy-match the span against the index; rewrite to `@relpath` **only on a
   high-confidence, unambiguous match**. On ambiguity or low confidence, leave
   the text untouched — same "never change meaning, safe fallback" principle
   the correction/snippet stages already follow.

Reuse the **existing personal dictionary** for project jargon the ASR mangles
(e.g. spoken "sunoto IPC" → token the matcher can hit).

### 4.4 Config (`PolishConfig` / `Settings`, all `#[serde(default)]`)

```jsonc
"file_references": {
  "enabled": false,                       // default off; opt-in
  "tools": ["claude"],                    // focused-tool gate
  "trigger_words": ["file", "open", "look at", "reference"],
  "min_confidence": 0.8,
  "max_index_files": 5000
}
```

Backward-compatible exactly like every other settings addition (the
`serde(default)` round-trip test already guards this).

## 5. Injection safety & privacy

- `@paths` are plain text with no control characters → they pass the existing
  `sanitize_for_insertion` unchanged. The real risk is a *wrong* rewrite, not
  an unsafe one; mitigated by high-confidence-only + default-off + a global
  switch.
- Use **repo-relative** paths; never emit absolute paths that leak `$HOME`.
- Fully local. The file list never leaves the machine; the feature only inserts
  text the user dictated, with file names substituted.
- Optional overlay affordance: show "↳ 2 file refs" so rewrites are visible.

## 6. Phasing

- **Phase A — context plumbing (no behavior change).** Add `_NET_WM_PID` /
  `_NET_WM_NAME` reads + `/proc` `claude` detection + cwd; widen the focus
  event to `DictationContext`; log the resolved context per session. Zero risk,
  immediately verifiable in `journalctl`, and reusable by every later feature.
- **Phase B — file index + resolver (the feature).** Background-warmed index +
  the gated polish stage + config, shipped **default-off**. Corpus-driven tests
  (below).
- **Phase C — polish & generalization.** Disambiguation UX, overlay indication,
  spoken CLI/identifier formatting in terminals, and extending `DevTool` to VS
  Code (workspace cwd) and plain shells (relative paths, no `@`).

## 7. Testing (matches repo conventions)

- Unit tests in `sunoto-context`: the `/proc` PPid walk (against a synthetic
  process-table fixture) and the matcher (basename/stem/component hits,
  ambiguity → no rewrite).
- A spoken→expected corpus under `tests/corpus/` (e.g.
  `file-reference-cases.json`) mirroring `phase2-text-cases.json`, plus an eval
  akin to `sunoto-daemon eval` reporting correct-resolution / false-rewrite
  rates.
- `cargo test --workspace --offline` and `cargo clippy --workspace --offline
  --all-targets -- -D warnings` stay green; `make test` runs it all.

## 8. Risks & mitigations

| Risk | Mitigation |
| --- | --- |
| Over-triggering (rewriting non-file phrases) | Trigger words + confidence gate + default-off + visible indication |
| ASR mangles project file names | Match on stems + fuzzy + reuse personal dictionary for jargon |
| Multiple `claude` under one terminal (tmux/splits) | Pick most-recent/deepest descendant; v1 takes the sole match and documents the limit |
| Title-based detection unreliable across terminals | Rely on `/proc`; title is only a secondary hint |
| Latency | Index cached + warmed off the release path; resolver is pure string work |
| Stale cwd after `cd` | cwd re-read at each capture (cheap); index keyed by cwd |

## 9. Adjacent features unlocked by the same context plumbing

Phase A's `DictationContext` is the foundation for several product-plan items:

- **Spoken CLI/code formatting in terminals** — "ls dash l a" → `ls -la`,
  identifier casing, `npm run dev`; extends the existing `Terminal` style.
- **On-demand local-LLM polish** (product plan §7 step 8) — a second
  llama.cpp sidecar reached by a "polish that" command or second hotkey, run
  *after* fast insertion, never on the latency path.
- **Nearby-text / screen context** — read the focused field (AT-SPI) or visible
  terminal buffer to bias spelling and to prefer files Claude already mentioned.
- **Dictation history & scratchpad** (Phase 3) — SQLite-backed re-insert / undo.
- **Voice commands** — "press enter" / "new line" (gated by injection safety),
  transform-selected-text, generate-at-cursor.
