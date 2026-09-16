# Independent Session Prompt: Complete Native Voice Spotlight

Copy everything between the horizontal rules into a new Codex session opened
on the existing `/Users/antash/workspace/voice-dictation` workspace.

---

Continue implementing Sunoto System Mode through the native Voice Spotlight
foundation, its end-to-end test skill, bounded AI multi-step planning, and
native beta hardening. Work through Phase 7 of
`docs/system-mode-plan-2026-07-15.md`; do not implement Phases 8–10.

Do not stop after reviewing or writing another plan. Implement, test, perform
safe available live checks, update the plan with objective evidence, and
continue while safe in-scope work remains.

## Workspace and context

1. Work in the existing `/Users/antash/workspace/voice-dictation` checkout.
   Do not create another worktree: the System Mode implementation and plan
   include important uncommitted and untracked work.
2. Before editing, read every applicable `AGENTS.md` instruction completely,
   then read these files completely:
   - `docs/system-mode-plan-2026-07-15.md`
   - `docs/desktop-configuration.md`
   - `docs/macos-recurring-issues.md` before macOS daemon or live work
3. Inspect `git status`, the implementation, and existing tests. All existing
   changes belong to the user. Preserve unrelated work. Do not reset, discard,
   stage, commit, or broadly reformat it unless explicitly requested.
4. Treat the plan's safety boundaries, phase goals, proof requirements, and
   exit criteria as authoritative. Status must describe evidence, not intent.
5. Phases 0, 1, and 3 are complete. Phase 2 is complete on macOS; Linux
   post-launch evidence and live validation remain partial. Phase 4 is only
   partially implemented. Begin there.

## Mandatory execution order

1. Repair, complete, and prove Phase 4.
2. Complete and prove Phase 5, including the actual unified Voice Spotlight
   experience.
3. Only after Phase 5 is objectively complete, use the available
   `skill-creator` skill and create the repository-local `system-mode-e2e`
   skill described below. Validate the skill before proceeding.
4. Complete and prove Phase 6 using only the capabilities proven by Voice
   Spotlight.
5. Complete Phase 7 where the available hosts permit it. Keep unavailable
   Linux/macOS live checks explicitly partial rather than inventing evidence.

Do not create the E2E skill before Phase 5 passes. Ordinary implementation
tests must evolve with Phases 4 and 5; the later skill should codify a complete
working product path, not institutionalize incomplete behavior.

## Phase 4 starting audit

The previous review established the following facts. Recheck them against the
current files, but treat them as the initial defect queue unless newer code
already fixes them:

- `PlannedCapabilityInput::ApplicationTargetFromStep` always resolves a prior
  application result into `application.open`. It cannot bind that result into
  the `browser` argument of `url.open`.
- `system plan "open Chrome and go to google.com"` currently returns a matched
  `OpenTarget` with a URL hint but zero executable steps.
- `system plan "open google.com"` also returns zero steps because URL-hinted
  generic `open` is not routed into the live URL path.
- the hand-written URL parser rejects a valid URL such as
  `https://example.com?x=1`, while accepting malformed authorities such as
  `example..com`, `-example.com`, and a non-numeric port;
- the selected-browser dispatcher test proves only that a manually authorized
  application reaches the native URL executor. It does not prove that a spoken
  compound request resolves, presents, selects, binds, executes, and observes
  the chosen browser; and
- no harmless live macOS proof was recorded for default-browser and selected
  Chrome navigation. The current macOS Rust target does not compile or prove
  the real Linux implementation.

### Required Phase 4 implementation

- Replace ad-hoc URL authority parsing with standards-compliant URL parsing.
  Accept only absolute HTTP/HTTPS URLs; reject credentials, denied schemes,
  control characters, malformed hosts/ports, and oversized input. Define and
  test the Unicode/IDNA policy. Preserve valid paths, queries, and fragments.
- Keep bounded spoken normalization such as `google dot com`, without turning
  arbitrary prose into a host.
- Make “go to google.com” and “open google.com” use the same validated typed
  capability path.
- Add a dedicated deterministic compound route for “open Chrome and go to
  google.com”. Do not treat the whole phrase as one application or URL query.
- Generalize planned target references sustainably so a discovery result can
  fill the correct typed argument of a later capability. A browser result must
  populate `OpenUrl.browser`, not become `OpenApplication`. Keep target
  references opaque and step-relative outside the native target store.
- Validate that a target used as a browser can handle HTTP(S); do not assume
  every installed application is a browser.
- Preserve explicit selection. Present a combined suggestion such as “Open
  google.com in Google Chrome”, consume authorization once, and reject stale,
  invented, replayed, expired, missing, and wrong-kind targets.
- If execution pauses for user selection, model the pause/resume explicitly.
  Do not bypass the runner or execute the first ranked candidate implicitly.
- Keep all native browser work off the daemon event loop and use NSWorkspace /
  supported Linux native APIs. Never use raw transcript shell text,
  AppleScript, arbitrary executable arguments, or generic process execution.
- Preserve data-only UTF-8 web-search encoding.

### Phase 4 exit proof

Do not mark Phase 4 done until all of the following are demonstrated:

- “Go to google.com” opens the validated URL in the default browser after the
  required confirmation.
- “Open google.com” follows the same safe path.
- “Search the web for Rust async traits” opens the correctly encoded search.
- “Open Chrome and go to google.com” resolves the real Chrome installation and
  opens the URL in that selected browser without UI automation or shell text.
- table-driven URL tests cover spoken forms, scheme/host case, valid ports,
  paths, queries with and without `/`, fragments, UTF-8 query encoding,
  Unicode/IDNA behavior, malformed labels/ports, credentials, control
  characters, oversized input, and denied schemes;
- route tests cover the four exit phrases and natural variations while
  ambiguous prose and unsupported multi-action input fail closed;
- runner/dispatcher tests cover default and selected browsers, correct
  cross-step argument binding, missing candidates, wrong-kind targets,
  one-time selection, stale/replay rejection, cancellation before side
  effects, native failure, and step/whole-plan timeouts;
- daemon-level tests cover transcript -> route -> palette -> selection ->
  worker -> typed observation without launching a real browser;
- read-only `system plan` and `system resolve` output has non-empty typed steps,
  shows only validated/step-relative data, exposes no native path/ID or command,
  and keeps `execution_allowed=false`; and
- harmless live macOS checks cover the default browser and installed Chrome.
  Add real Linux fixture/build coverage where possible and leave unavailable
  X11/Wayland live proof explicitly partial.

Passing compilation or a direct native-executor unit test is not sufficient.
The spoken route, selection, binding, executor, observation, and safe live
evidence must form one demonstrable product path.

## Phase 5: actual Voice Spotlight

After Phase 4 is proven, implement Phase 5 using the plan's full goal and
proof requirements. At minimum:

- introduce reusable typed target kinds and opaque target references for
  applications, files, folders, projects, URLs, and web-search actions;
- add approved-root and known-folder providers, macOS Spotlight search, and
  Linux indexed or cancellable bounded fallback search;
- canonicalize and revalidate targets, enforce expiry/result caps/safe file
  types, and handle symlinks, disappearing targets, and hidden/system paths;
- detect projects and editor evidence without hard-coding one user's paths;
- add native open/reveal/open-with operations with explicit selection;
- fan out one `target.find` across relevant providers and rank candidates in
  one palette with target kind, location, evidence, ambiguity, and no-match
  behavior; and
- make single-action and later planned actions share the same target store,
  ranking, policy, dispatcher, executor, observations, and verification.

Phase 5 is not done until “open who-else-is-free” finds the real local project,
folder, or application instead of performing application-only search, and
“open who-else-is-free in VS Code” opens the selected project in the resolved
editor. Prove applications, files, folders, projects, URLs, and web searches in
the unified palette; safe-root behavior; selection-before-action; ambiguity;
no-match; cancellation; deadlines; target changes; ranking quality; and
available macOS/Linux live behavior. Update the plan to `[DONE]` only with
recorded evidence.

## After Phase 5: create the E2E skill

Once Phase 5 is genuinely `[DONE]`, explicitly announce that the skill is now
being used, read the available `skill-creator` `SKILL.md` completely, and follow
it. Create a repository-local skill at:

`/Users/antash/workspace/voice-dictation/.agents/skills/system-mode-e2e/SKILL.md`

The `system-mode-e2e` skill must safely run and report the complete System Mode
verification workflow. Keep its main instructions focused and use only the
scripts, references, and disposable fixtures it genuinely needs.

### Required skill modes

- `static`: formatting checks, Rust tests, clippy, and relevant Python tests;
- `contract`: routing, planning, resolving, policy, target-store, ranking,
  capability, adversarial, and fixture checks without opening applications;
- `daemon`: mock-ASR/control-socket/palette/worker flows, including normal
  dictation regression, without loading the real ASR model or GPU; and
- explicit opt-in `live-macos` and `live-linux`: harmless real application,
  browser, file, folder, and project actions on the current host.

The skill must cover application opening, default/specified-browser URL open,
web search, unified target search, file/folder reveal/open, project open,
ambiguity selection, cancellation, no-match, stale/replay rejection, timeout,
target mutation, and normal dictation regression.

### Skill safety and reporting

- Read repository guidance, desktop configuration, and macOS recurring issues
  before daemon or platform operations.
- Use disposable fixture roots, harmless local documents/projects, and safe
  test URLs.
- Require explicit user confirmation immediately before any mode that opens a
  real application, browser, file, folder, or project.
- Never type into an uncontrolled focused window, start a second GPU-heavy ASR
  sidecar, delete user data, send messages, purchase anything, install
  software, alter security settings, or execute shell text derived from speech.
- Detect the host. Report unavailable platform checks as `SKIPPED` with a
  reason; never claim Linux evidence from macOS stubs or vice versa.
- Emit a concise machine-readable artifact and human report with pass/fail/skip
  counts, covered phase criteria, commands and fixtures, failures, useful logs,
  host information, live actions actually observed, and cleanup status. Redact
  sensitive transcripts, paths, credentials, and opaque IDs.
- Clean up temporary configuration, fixtures, palettes, mock processes, and
  test daemons. Never leave a second daemon running.

### Skill completion proof

- Validate the skill using the skill-creator workflow.
- Prove a clean `static`/`contract` run causes no live application opening or
  external state change.
- Prove the mock-daemon mode covers transcript-to-observation plus normal
  dictation regression without real ASR/GPU work.
- Inject a negative fixture and prove the skill reports failure rather than a
  false pass.
- Run safe live modes only after explicit approval on each available host;
  unavailable platforms remain skipped.
- Run it twice and prove cleanup/idempotence: no conflicting daemon, palette,
  fixture, focus target, or temporary configuration remains.

Do not begin Phase 6 until the skill exists, validates successfully, and its
non-live modes pass.

## Phases 6 and 7

For Phase 6, add a model-agnostic schema-constrained planner over only the
capabilities proven in Phase 5. Deterministic obvious commands stay fast. The
model never receives a shell, native IDs, unrestricted filesystem access, or
authority to bypass policy or report success. Complete the plan's evaluation,
adversarial-observation, clarification, hard-limit, stale-ID, cancellation,
success-evidence, latency, and ASR/GPU coexistence proof.

For Phase 7, complete native beta hardening where the available machines allow:
onboarding, permissions, settings, recovery, sleep/wake, daemon/sidecar and
overlay loss, redacted logs, cancellation, compatibility reporting, and
release metrics. Do not claim unavailable platform evidence.

Do not implement Phases 8–10 in this session. Specifically, do not build the
integration SDK, Spotify/VS Code-specific integrations, general accessibility
control, visual clicking, arbitrary shell execution, messaging, purchases,
credentials, deletion, installation, or security-setting changes.

## Architecture and safety invariants

- System Mode is a voice-first extension of Spotlight, not an application
  finder.
- Single-action and AI-planned actions share one capability registry, target
  model, provider set, ranking, opaque target store, policy, dispatcher,
  executor, observation model, and verification path.
- Only native providers mint opaque IDs. Revalidate immediately before side
  effects and reject invented, stale, replayed, expired, consumed, or
  wrong-kind references.
- Treat application, filename, file content, webpage, integration,
  accessibility, and clipboard text as untrusted data, never instructions.
- Keep blocking discovery and execution off the daemon event loop.
- Preserve explicit selection/confirmation unless measured policy and tests
  explicitly authorize something narrower.
- Preserve ordinary dictation and the working macOS application-open flow after
  every phase.
- Never introduce a generic command string, executable-and-arguments escape
  hatch, AppleScript fallback, arbitrary D-Bus operation, or shell fallback.

## Verification discipline

Run after relevant changes:

- `cargo test --workspace --offline`
- `cargo clippy --workspace --offline --all-targets -- -D warnings`
- `cargo fmt --all -- --check`, without rewriting unrelated dirty files
- relevant Python `unittest` suites for overlay/sidecar changes
- focused provider, routing, runner, dispatcher, policy, and adversarial tests
- read-only `system plan` / `system resolve` checks
- harmless live checks required by the current phase, only with the safety
  precautions and user approval described above

When a required host is unavailable, finish portable contract/fixture tests,
record the live item as partial or skipped, and continue with other safe work.
Never turn unavailable evidence into `[DONE]`.

At the end, report:

1. Phase-by-phase status with objective evidence.
2. Exact tests, dry runs, and live checks performed.
3. Remaining partial/skipped items, especially Linux/macOS host coverage.
4. The final supported Voice Spotlight commands and multi-step behavior.
5. The created E2E skill path, supported modes, validation result, and most
   recent non-live report.
6. Confirmation that normal dictation and existing application opening were
   not regressed.
7. All files changed, without staging or committing them.

---
