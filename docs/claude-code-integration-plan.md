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
suites all pass. Phase C (disambiguation UX, overlay indication, other tools)
remains future work.

**Update (later June 15) — field-tested live, two fixes from the logs:**

1. **Wrong session picked.** gnome-terminal runs every tab/window under one
   `gnome-terminal-server` PID, so PID-only detection found *all* `claude`
   sessions and indexed the first `/proc` listed (the wrong repo). The daemon
   now gathers every `claude` working directory under the focused terminal and,
   when there is more than one, picks the active one from a hint file written by
   a Claude Code hook — `bin/sunoto-claude-cwd-hook.sh`, wired to
   `UserPromptSubmit`/`SessionStart` in `~/.claude/settings.json`, writing
   `$XDG_RUNTIME_DIR/sunoto/claude-active-cwd`. A sole session needs no hook;
   with several and no usable hint the daemon refuses to guess
   (`select_cwd`, unit-tested).
2. **Matcher too strict.** Now four tiers (exact separator-aware join →
   path-qualified → all-tokens-in-basename → best-overlap), tolerating spoken
   `hyphen`/`dot`, compound names (`make file` → `Makefile`), and a one-word ASR
   slip (`cloud` → `claude`). Still rewrites only a unique result.

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
   `UiCommand::CaptureFocus` on hotkey release and used the focused WM_CLASS
   only to pick a `StyleProfile`. We widened that capture — the event is now
   `DaemonEvent::Focus(FocusInfo { class, pid, title })` — so the same path
   also yields the window PID the tool detector needs.
2. **The deterministic polish pipeline.** `sunoto-polish::polish()` is a staged,
   microsecond-budget, app-aware text transform (normalize → corrections →
   fillers → dictionary → snippets → style). File-reference resolution is a
   sibling pure transform (`sunoto-polish::resolve_file_references`) the daemon
   runs **after** polish and **only** when the focused tool is recognized — kept
   out of `polish()` itself because it needs the daemon-supplied file list.

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

### 4.1 Focused-window context and tool detection

`sunoto-linux` reads three properties from the focused window's client window in
the **same parent-climb** that already located WM_CLASS — `WM_CLASS`,
`_NET_WM_PID` (`XA_CARDINAL`) and `_NET_WM_NAME` (`UTF8_STRING`) — and returns
them as `WindowContext { class, pid, title }` from `window_context()`;
`window_class()` is now a thin wrapper over it. On Wayland the same three come
from `hyprctl activewindow -j`. The daemon carries them as
`DaemonEvent::Focus(FocusInfo { class, pid, title })`, captured at hotkey
release. `class` still drives `resolve_style`; `pid` feeds tool detection;
`title` is logged.

`sunoto-context` (new std-only crate) turns the PID into a tool + working dir:

```
enum DevTool { ClaudeCode { cwd: PathBuf } }   // VsCode, Shell, ... later
fn detect_tool(window_pid: u32) -> Option<DevTool>
```

- Enumerate processes named `claude`; keep those that descend from `window_pid`
  (walk each one's `/proc/<pid>/status` `PPid` chain, bounded) and read its
  `/proc/<pid>/cwd`.
- **Disambiguate** (`select_cwd`): gnome-terminal shares one server PID across
  tabs, so a window PID can match several `claude` sessions. One candidate is
  used directly; with several, the active-cwd hint
  (`$XDG_RUNTIME_DIR/sunoto/claude-active-cwd`, written by the Claude Code hook
  `bin/sunoto-claude-cwd-hook.sh`) breaks the tie, and the daemon **refuses to
  guess** when the hint matches no candidate.

The daemon logs `insertion target at release:` (class + title) and, on a cache
miss, `indexing <cwd> …`. *(The original design walked to "the first `claude`
descendant"; field testing showed that picks an arbitrary session under a
shared terminal — hence the hint-based `select_cwd`.)*

### 4.2 File index (`sunoto-context::FileIndex`)

`FileIndex::build(root, max_files)` lists repo-relative paths via
`git ls-files --cached --others --exclude-standard` (fast, honors `.gitignore`),
with a bounded, symlink-free recursive walk fallback for non-git dirs (skips
`.git`, `target`, `node_modules`, `.venv*`, …), truncated to `max_files`. It is
a flat `Vec<String>` of paths; the matcher derives basename/stem keys on demand
rather than pre-indexing them. The daemon caches one index keyed by cwd and
rebuilds it when the cwd changes — built lazily at finalize, off the
partial-streaming path. *(Background warming and mtime/inotify refresh are
deferred to Phase C; today the first dictation in a new repo pays the
`git ls-files`.)*

### 4.3 Reference resolution (`sunoto-polish::resolve_file_references`)

`resolve_file_references(text, files, &FileReferenceConfig) -> String` — pure,
no I/O, well inside the deterministic budget. The daemon runs it **after**
`polish()` (not as a stage inside it, since it needs the file list), gated by
`resolve_claude_file_references`: only when `file_references.enabled`, the
focused class is a terminal, and `detect_tool` returns a cwd.

- **Spans.** A run of name words adjacent to a trigger: a trigger **noun** after
  the name ("the daemon **file**") or a trigger **verb** before it ("**open**
  daemon"), absorbing a leading determiner. Trigger nouns stay minimal
  (`file`/`module`/`script`) so filename words like "plan" stay part of the
  name. Spoken separators (`hyphen`, `dot`, …) are dropped from the candidate.
- **Match — four tiers, first unique wins.** (1) basename/stem equals a
  separator-aware join (`daemon` `rs` → `daemon.rs`); (2) the last token names
  the file and earlier tokens are path substrings (`polish` `lib` → the lib.rs
  under sunoto-polish); (3) every token is a substring of the basename; (4) the
  basename with the most token hits, given a strict winner with ≥2 hits
  (tolerates a one-word ASR slip, e.g. `cloud`→`claude`). Ambiguous or unmatched
  → text unchanged, the same safe-fallback the correction/snippet stages follow.

Reusing the personal dictionary for project jargon is left to Phase C.

### 4.4 Config — `FileReferenceConfig` (in `PolishConfig`, all `#[serde(default)]`)

```jsonc
"file_references": {
  "enabled": false,                                 // opt-in
  "tools": ["claude"],                              // focused-tool gate
  "trigger_nouns": ["file", "module", "script"],
  "trigger_verbs": ["open", "edit", "check", "update", "read", "see"],
  "max_index_files": 5000
}
```

Backward-compatible exactly like every other settings addition (the
`serde(default)` round-trip test guards it). *(The original sketch had a single
`trigger_words` list plus a `min_confidence`; the build instead splits noun vs
verb triggers and gates on a unique match, not a score.)*

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

- **Phase A — context plumbing. ✅ Done.** `_NET_WM_PID` / `_NET_WM_NAME` reads +
  `/proc` `claude` detection + cwd; the focus event widened to
  `FocusInfo { class, pid, title }`. Verifiable in `journalctl`, reusable by
  every later feature.
- **Phase B — file index + resolver. ✅ Done (default-off).** `FileIndex` +
  `resolve_file_references` + `FileReferenceConfig` + the Claude Code cwd hook.
  Index is lazily built and cwd-cached (background warming deferred to C). Unit
  tests cover the matcher, `select_cwd`, and the `/proc` helpers.
- **Phase C — polish & generalization (next).** Disambiguation UX, overlay
  indication ("↳ 2 file refs"), background index warming + inotify refresh,
  spoken CLI/identifier formatting in terminals, the spoken→expected corpus +
  eval, dictionary reuse for jargon, and extending `DevTool` to VS Code
  (workspace cwd) and plain shells (relative paths, no `@`).

## 7. Testing (matches repo conventions)

- `sunoto-context`: `select_cwd` tie-breaking, the `/proc` descendant/`comm`
  helpers, and the file-index walk (temp-dir fixture).
- `sunoto-polish`: `resolve_file_references` — unique matches, ASR-noise
  tolerance (spoken hyphens, `make file`→`Makefile`, `cloud`→`claude`), and the
  conservative cases (ambiguous/unknown → unchanged, idempotent).
- A spoken→expected corpus under `tests/corpus/` plus an eval subcommand
  reporting correct-resolution / false-rewrite rates are **not yet built**
  (Phase C); current coverage is inline unit tests.
- `cargo test --workspace --offline` and `cargo clippy --workspace --offline
  --all-targets -- -D warnings` stay green.

## 8. Risks & mitigations

| Risk | Mitigation |
| --- | --- |
| Over-triggering (rewriting non-file phrases) | Trigger words + confidence gate + default-off + visible indication |
| ASR mangles project file names | Four-tier matcher tolerant of spoken separators and a one-word slip; unique-match-only (dictionary reuse deferred to C) |
| Multiple `claude` under one terminal (gnome-terminal tabs) | **Resolved:** `select_cwd` uses the Claude Code cwd hook (most-recently-active session) and refuses to guess without it |
| Title-based detection unreliable across terminals | Rely on `/proc`; title is only a secondary hint |
| Latency | Index cached + warmed off the release path; resolver is pure string work |
| Stale cwd after `cd` | cwd re-read at each capture (cheap); index keyed by cwd |

## 9. Adjacent features unlocked by the same context plumbing

Phase A's focus-context plumbing (`FocusInfo` + `sunoto-context`) is the
foundation for several product-plan items:

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
