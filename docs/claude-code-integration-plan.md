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

- **`sunoto-context`** (new crate): config-driven `/proc` agent detection + cwd
  (`detect_agent` over an `AgentProcess` registry), and a `git ls-files` /
  bounded-walk file index.
- **`sunoto-linux`**: focused-window `_NET_WM_PID` and `_NET_WM_NAME` reads via
  a new `window_context` (the WM_CLASS climb now reads all three at once).
- **`sunoto-polish`**: `resolve_file_references` (conservative,
  unique-match-only, per-agent `ref_template`) plus `FileReferenceConfig` and its
  `AgentConfig` registry.
- **`sunoto-daemon`**: detects any configured agent at finalize (terminal windows
  only, so the `/proc` scan is skipped otherwise), caches the index per cwd, and
  rewrites references after the polish pass.

Verified: `cargo test --workspace` (including new resolver and `/proc` tests),
`cargo clippy --workspace -- -D warnings`, and the GPU-free Python protocol
suites all pass. Phase C (overlay indication, eval corpus) remains future work.

**Update (later June 15) — field-tested live, two fixes from the logs:**

1. **Wrong session picked.** gnome-terminal runs every tab/window under one
   `gnome-terminal-server` PID, so PID-only detection found *all* `claude`
   sessions and indexed the first `/proc` listed (the wrong repo). The daemon
   now gathers every matching agent session under the focused terminal and, when
   there is more than one, picks the session whose **controlling terminal was
   most recently active** (`/dev/pts/N` atime/mtime via `/proc/<pid>/fd`) — an
   agent-agnostic, hook-free tie-break (`select_candidate`, unit-tested). A sole
   session needs no tie-break; on a tie, or with no readable pts activity, the
   daemon refuses to guess. *(An earlier iteration used a Claude-specific cwd
   hint file written by a Claude Code hook; the terminal-activity tie-break
   replaced it so the mechanism generalizes to every agent.)*
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

`sunoto-context` (std-only crate) turns the PID + a config-driven agent registry
into a detected agent + working dir:

```
struct AgentProcess  { name: String, comm: String }      // one registry row
struct DetectedAgent { name: String, cwd: PathBuf }
fn detect_agent(window_pid: u32, agents: &[AgentProcess]) -> Option<DetectedAgent>
```

- One `/proc` pass: keep processes whose `comm` matches any registry row and
  that descend from `window_pid` (walk each one's `/proc/<pid>/status` `PPid`
  chain, bounded), reading `/proc/<pid>/cwd` for each.
- **Disambiguate** (`select_candidate`): gnome-terminal shares one server PID
  across tabs, so a window PID can match several agent sessions. One candidate
  is used directly; with several, the daemon picks the session whose
  **controlling terminal was most recently active** — the newer of its
  `/dev/pts/N` atime (input) / mtime (output), read via `/proc/<pid>/fd`. This
  is agent-agnostic (it reads the terminal device, not the agent), needs **no
  per-agent hook**, and the daemon still **refuses to guess** on a tie or when
  no pts activity is readable.

The daemon logs `insertion target at release:` (class + title) and, on a cache
miss, `indexing <cwd> …`. *(The original design walked to "the first `claude`
descendant"; field testing showed that picks an arbitrary session under a
shared terminal. The first fix was a Claude-specific cwd hint file written by a
Claude Code hook; this was then replaced by the hook-free, agent-agnostic
terminal-activity tie-break above.)*

### 4.2 File index (`sunoto-context::FileIndex`)

`FileIndex::build(root, max_files)` lists repo-relative paths via
`git ls-files --cached --others --exclude-standard` (fast, honors `.gitignore`),
with a bounded, symlink-free recursive walk fallback for non-git dirs (skips
`.git`, `target`, `node_modules`, `.venv*`, …), truncated to `max_files`. It is
a flat `Vec<String>` of paths. From this the daemon builds a
`sunoto-polish::FileMatchIndex` — the lowercased `(path, basename, stem)` match
keys precomputed once — and caches `(cwd, FileMatchIndex)`, rebuilding when the
cwd changes. Precomputing the keys keeps `resolve_file_references` free of
per-utterance lowercasing/allocation (~4× faster matching; it previously rebuilt
the keys on every spoken reference). Built lazily at finalize, off the
partial-streaming path. *(Background warming and mtime/inotify refresh are
deferred to Phase C; today the first dictation in a new repo pays the
`git ls-files` + key build.)*

### 4.3 Reference resolution (`sunoto-polish::resolve_file_references`)

`resolve_file_references(text, &FileMatchIndex, ref_template, &FileReferenceConfig)
-> String` — pure, no I/O, well inside the deterministic budget. The daemon runs
it **after** `polish()` (not as a stage inside it, since it needs the file list),
gated by `resolve_agent_file_references`: only when `file_references.enabled`,
the focused class is a terminal, and `detect_agent` returns an agent + cwd. The
`ref_template` of the matched registry row is applied to each resolved path
(`{path}` substituted), so the output syntax is per-agent config, not code.

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
  "agents": [                                        // registry: one row per agent
    { "name": "claude", "process": "claude", "ref_template": "@{path}" },
    { "name": "gemini", "process": "gemini", "ref_template": "@{path}" }
    // add e.g. { "name": "aider", "process": "aider", "ref_template": "/add {path}" }
  ],
  "trigger_nouns": ["file", "module", "script"],
  "trigger_verbs": ["open", "edit", "check", "update", "read", "see"],
  "max_index_files": 5000
}
```

`agents` replaces the original `tools` string list: detection, cwd resolution,
and rendering are one generic engine, so a new agent is a row (process name +
`{path}` template), never new code. A stale `tools` key in an old config is
simply ignored (no `deny_unknown_fields`), so the change is backward-compatible.

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
  `resolve_file_references` + `FileReferenceConfig`. Index is lazily built and
  cwd-cached (background warming deferred to C). Unit tests cover the matcher,
  `select_candidate`, and the `/proc` helpers.
- **Phase B′ — agent generalization. ✅ Done (default-off).** `DevTool` →
  config-driven registry (`AgentConfig` rows: `process` + `ref_template`);
  `detect_agent` matches any configured agent in one `/proc` pass; the
  Claude-specific cwd hook replaced by hook-free terminal-activity disambiguation
  (`select_candidate`). Claude Code + Gemini CLI ship as default rows; Codex /
  Aider are config rows (syntax pending verification).
- **Phase C — polish & remaining generalization (next).** Overlay indication
  ("↳ 2 file refs"), background index warming + inotify refresh, spoken
  CLI/identifier formatting in terminals, the spoken→expected corpus + eval,
  dictionary reuse for jargon, and non-terminal targets — VS Code (workspace
  cwd) and plain shells (relative paths, no `@`).

## 7. Testing (matches repo conventions)

- `sunoto-context`: `select_candidate` activity tie-breaking, the `/proc`
  descendant/`comm` helpers, and the file-index walk (temp-dir fixture).
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
| Multiple agent sessions under one terminal (gnome-terminal tabs) | **Resolved:** `select_candidate` picks the most-recently-active controlling terminal (pts atime/mtime) — hook-free, agent-agnostic — and refuses to guess on a tie |
| Title-based detection unreliable across terminals | Rely on `/proc`; title is only a secondary hint |
| Latency | Index cached + warmed off the release path; resolver is pure string work |
| Stale cwd after `cd` | cwd re-read at each capture (cheap); index keyed by cwd |

## 9. Adjacent features unlocked by the same context plumbing

Phase A's focus-context plumbing (`FocusInfo` + `sunoto-context`) is the
foundation for several product-plan items:

- **Spoken CLI/code formatting in terminals** — "ls dash l a" → `ls -la`,
  identifier casing, `npm run dev`; extends the existing `Terminal` style.
- **On-demand local-LLM polish** (the product plan's optional polish) — a second
  llama.cpp sidecar reached by a "polish that" command or second hotkey, run
  *after* fast insertion, never on the latency path.
- **Nearby-text / screen context** — read the focused field (AT-SPI) or visible
  terminal buffer to bias spelling and to prefer files Claude already mentioned.
- **Dictation history & scratchpad** (Phase 3) — SQLite-backed re-insert / undo.
- **Voice commands** — "press enter" / "new line" (gated by injection safety),
  transform-selected-text, generate-at-cursor.
