# Sunoto System Mode Plan

**Created:** July 15, 2026
**Revised:** July 22, 2026
**Status:** Opening installed applications works end-to-end on the current
macOS product path, and Phase 3's capability contract, dispatcher, typed
observations, cross-step target binding, hard limits, fake planner, and bounded
runner are complete. Native URL/browser navigation is next; unified target
search, bounded AI planning, and integrations come afterward.
**Scope:** Build an explicitly activated, cross-platform, voice-first extension
of Spotlight. It first finds and acts on real local/web targets, then allows an
AI planner to combine those same safe capabilities into bounded multi-step
goals. The model never receives a shell or generic process-execution tool.

Status labels used in this plan:

- **[DONE]** implemented in the current worktree and covered by tests;
- **[PARTIAL]** implemented for part of the flow or awaiting live platform
  validation;
- **[NEXT]** the current implementation focus; and
- **[LATER]** intentionally sequenced after the native foundation.

### Current implementation snapshot

- **[DONE]** Separate, immutable Dictation and System sessions with distinct
  shortcuts and overlay states.
- **[DONE]** System transcripts bypass dictation insertion and polishing.
- **[DONE]** Pure typed intents, deterministic routing, suggestion ranking,
  risk classification, one-time selection tokens, and replay/stale rejection.
- **[DONE]** Native application discovery and launch implementations for macOS
  (`NSWorkspace`) and Linux (`GAppInfo`), isolated from the daemon event loop.
- **[DONE]** Native macOS and GTK suggestion palettes with explicit selection
  and cancellation.
- **[DONE]** Live macOS System flow resolves, presents, selects, revalidates,
  and opens installed applications.
- **[PARTIAL]** The equivalent Linux implementation exists; supported
  X11/Wayland host validation remains.
- **[PARTIAL]** Parser types exist for files, folders, URLs, and web search,
  but the live worker resolves and executes applications only.
- **[PARTIAL]** `system_llm_fallback_enabled` and the planner boundary exist,
  but no command-planning model is connected to the live flow.
- **[DONE]** The working application path now uses typed capability contracts,
  a native dispatcher, bounded observations, cross-step target binding,
  system-owned limits, and a bounded runner.
- **[PARTIAL]** Native URL/browser navigation has a typed HTTP(S)-only
  contract, strict web-search construction, palette-confirmed live default
  browser dispatch, and resolved-browser executor binding. The deterministic
  two-step spoken browser route and live-host proof remain before this phase
  can be marked done.

## 1. Product Definition

System mode is a voice-first extension of Spotlight with an additional bounded
action layer. It begins as universal voice search and open; once that is
reliable, an AI planner composes the same capabilities into multi-step goals.
It is not merely an application finder.

Examples:

- “Open Chrome.”
- “Open Visual Studio Code.”
- “Find the quarterly report.”
- “Search for the quarterly report and open it.”
- “Show the Downloads folder.”
- “Search the web for Rust async traits.”
- “Open who-else-is-free.”
- “Open who-else-is-free in VS Code.”
- “Open Chrome and go to google.com.”
- “Open Spotify and play Despacito.”

For a single-action request, native providers search applications, files,
folders, projects, websites, and web search, then rank real results in one
Spotlight-style palette. For a multi-step goal, an AI planner translates the
request into a short plan made only from the same capabilities advertised by
Sunoto. Platform executors perform each action and report observations back to
the runner.

System mode is not a shell and is not an automatic interpretation layer over
normal dictation. The planner cannot create executable paths, shell commands,
AppleScript, arbitrary D-Bus calls, or unregistered integration operations.
It chooses typed capabilities; trusted Sunoto code validates and executes
them.

System mode is risk-aware rather than universally suggestion-first. Safe,
reversible native actions may become auto-executable after the native MVP is
measured and the user opts in. Ambiguous targets, state-changing actions, and
external side effects require selection or confirmation. Destructive,
privileged, credential, payment, and other denied actions remain unavailable.

### Product layers

Build the product in this order:

1. **Voice Spotlight:** find and rank applications, files, folders, projects,
   websites, and web-search actions in one palette.
2. **Actions on results:** open, reveal, search, or open-with using opaque
   resolved target IDs and native platform APIs.
3. **Multi-step execution:** let the AI planner compose those proven
   capabilities, execute one step at a time, observe results, and stop or ask
   when a target is ambiguous.
4. **Application integrations:** add app-specific capabilities through the
   public integration contract.
5. **Computer-use fallback:** use semantic accessibility and, last, visual
   interaction for applications without a direct capability or integration.

Each layer must remain useful on its own. Multi-step planning must reuse the
Voice Spotlight index, targets, actions, ranking, policy, and executors rather
than creating a parallel agent-only path.

## 2. Core Product Decision: Explicit Activation

Normal dictation must never be reinterpreted as an operating-system command.
System mode should therefore use a separate configurable hold shortcut.

- Dictation shortcut: `Ctrl+F1` by default.
- System shortcut: `Ctrl+F2` by default, subject to onboarding conflict
  detection and user configuration.
- Hold the System shortcut, speak one command, and release to plan and perform
  a search for matching actions.
- The mode exists only for that session. The initial release must not have a
  persistent toggle that a user can forget is enabled.
- Wayland compositor bindings send mode-specific edges, such as
  `trigger system press` and `trigger system release`.

The recording overlay must look unmistakably different in System mode. For
example, dictation can retain its current red recording treatment while System
mode uses blue or purple, an action icon, and the label `System`.

Voice prefixes such as “Computer, open Chrome” may be explored later, but must
not replace the explicit shortcut until false-activation testing shows that
they are equally safe.

## 3. Intended User Flows

### 3.1 Open an application

1. The user holds the System shortcut.
2. The overlay shows `System — listening`.
3. The user says “Open Chrome” and releases.
4. Sunoto transcribes the utterance.
5. The command parser returns `OpenApplication { query: "Chrome" }`.
6. The application resolver builds suggestions from installed applications,
   led by `Google Chrome` with a stable platform identifier.
7. The command palette opens with `Open Google Chrome` highlighted and any
   plausible alternatives underneath it.
8. The user presses Enter or clicks the highlighted suggestion.
9. Policy revalidates the selected suggestion.
10. The platform executor opens or activates Google Chrome.
11. The overlay briefly shows `Opened Google Chrome` and hides.

If Chrome is not installed, Sunoto does nothing and reports `Chrome was not
found`, optionally offering a safe `Search the web for Chrome` suggestion. A
high-confidence first result may be highlighted, but it is never executed only
because it ranked first.

### 3.2 Find and open a file

1. The user says “Search for the quarterly report and open it.”
2. The parser returns `OpenFileByQuery { query: "quarterly report" }`.
3. A local file-search provider returns ranked, canonical candidates.
4. The command palette shows the top safe candidates with filename, parent
   folder, type, and modification date.
5. The requested action, `Open`, is shown explicitly beside each candidate.
6. The user chooses a candidate before it is opened.
7. Executables, scripts, application bundles, installers, and shortcuts are
   excluded from document-opening suggestions.

The existing recording overlay must remain non-activating. A separate command
result/confirmation window may take focus only after recording has ended and
only when user selection is required.

### 3.3 Unknown or unsupported command

If the fast router and planner cannot produce a valid bounded plan, Sunoto
performs nothing and reports `I couldn't match that to a supported System
action`.

The raw transcript must not be inserted into the focused application and must
never be forwarded to a shell as a fallback.

### 3.4 Open an unresolved target

For “Open who-else-is-free”:

1. ASR produces the transcript.
2. The fast router (or later planner) emits
   `FindTarget { query: "who-else-is-free", kinds: Auto }`; it does not assume
   the phrase names an application.
3. Native providers search installed applications, configured project roots,
   recent editor projects, allowed files/folders, and enabled browser sources.
4. Providers return opaque candidate IDs plus evidence. They do not expose an
   executable command.
5. If one strong project match exists and its preferred editor is known,
   Sunoto offers or performs `OpenProject` with the resolved project and editor
   IDs.
6. If several plausible candidates exist, the palette asks the user to choose.
7. If nothing exists locally, Sunoto reports the no-match and may offer a
   separate web search.

The planner never invents a local path. Only a native provider can turn spoken
text into a canonical target ID.

### 3.5 Native multi-step navigation

For “Open Chrome and go to google.com”:

1. The fast router first produces a bounded two-step plan: resolve Chrome,
   then open a validated HTTPS URL in that resolved application. The later AI
   planner produces the same contract for natural variations.
2. Sunoto resolves the real Chrome installation and validates the URL.
3. The native executor opens the URL in Chrome. Launching Chrome separately is
   unnecessary when the platform URL-open operation can do both reliably.
4. Sunoto verifies the platform operation and reports completion.

The user experiences one goal even when the planner represents it as multiple
typed steps. The runner may optimize adjacent steps, but it may not change the
goal or add side effects.

### 3.6 Integration-backed application action

For “Open Spotify and play Despacito”:

1. The planner resolves Spotify and the requested media action.
2. If a trusted Spotify integration is installed, Sunoto calls its declared
   `media.search_and_play` capability and observes the typed result.
3. If no compatible integration exists, the later computer-use fallback may
   inspect Spotify through accessibility APIs and interact with labeled UI
   elements.
4. If several tracks are equally plausible, Sunoto asks the user to choose.
5. Sunoto verifies the now-playing state before reporting success.

This flow is **[LATER]**. The current implementation focus is native targets
and navigation; the integration SDK and generic UI fallback are intentionally
sequenced after that foundation.

## 4. Delivery Scope

### 4.1 Native foundation — current focus

| Capability | Example | Status |
| --- | --- | --- |
| `application.find/open` | “Open Chrome” | **[DONE]** current macOS flow; Linux host validation remains |
| `target.find` | “Open who-else-is-free” | **[NEXT]** provider fan-out and evidence ranking |
| `file.find/open/reveal` | “Open the quarterly report” | **[NEXT]** |
| `folder.find/open` | “Open Downloads” | **[NEXT]** |
| `project.find/open` | “Open who-else-is-free in VS Code” | **[NEXT]** |
| `web.search` | “Search the web for Tokio channels” | **[NEXT]** |
| `url.open` | “Go to google.com” | **[NEXT]** |
| unified voice search | “Open who-else-is-free” | **[NEXT]** Spotlight-style cross-provider palette |
| bounded native plans | “Open Chrome and go to google.com” | **[NEXT]** |

The native phase uses operating-system facilities and stable local metadata.
It does not depend on application-specific integrations or screen clicking.

### 4.2 Integration and computer-use expansion

After the native foundation is complete:

| Capability family | Example | Status |
| --- | --- | --- |
| application integrations | “Play Despacito on Spotify” | **[LATER]** |
| semantic UI inspection/action | “Click New Project” | **[LATER]** |
| visual observe/click fallback | inaccessible or canvas UI | **[LATER]** |
| state-changing workflows | calendar, notes, messaging | **[LATER]**, separate policy gates |

### 4.3 Explicitly denied or separately gated

- Arbitrary shell or terminal commands
- Model-generated command lines, AppleScript, PowerShell, or shell scripts
- Installing or uninstalling software
- Moving, renaming, overwriting, or deleting files
- Emptying Trash
- Changing security, privacy, account, network, or accessibility settings
- Entering passwords, authentication codes, payment data, or credentials
- Sending messages, email, or form submissions without explicit integration
  policy and final confirmation
- Purchases or financial actions
- Unbounded UI control or coordinate clicking without observation and policy
- Plans that exceed configured step, time, or side-effect limits
- Background autonomous actions
- Remote-computer control
- Actions based on untrusted webpage, document, or clipboard instructions

These exclusions are part of the security boundary. Later integration work
must not weaken them incidentally.

## 5. Architecture

System mode should preserve the repository's existing invariants:

- one Rust daemon event loop owns capture, session state, planning decisions,
  and dispatch;
- ASR continues to run in the existing managed sidecar;
- pure parsing, resolution, ranking, and policy code remains testable without
  platform UI or a live model;
- platform operations live in `sunoto-macos` and `sunoto-linux` behind the
  `sunoto-desktop` facade;
- typed protocols are used at every sidecar boundary; and
- UI writes and file searches never block the capture/transcription latency
  path.

### Proposed components

```text
System shortcut
      |
      v
sunoto-daemon session(mode=System)
      |
      +--> existing audio capture + ASR sidecar
      |                |
      |                v
      |          final transcript
      |                |
      v                v
request router
      |
      +--> single action -> Voice Spotlight target search + ranking
      |
      +--> multi-step goal -> AI planner [after Voice Spotlight]
      |                         ^
      |                         | typed observations and tool results
      v                         |
typed action or bounded typed plan
      |
      v
capability registry + policy
      |
      +--> native target providers (apps/files/folders/projects/URLs)
      +--> native platform executors (macOS/Linux)
      +--> installed application integrations [LATER]
      +--> accessibility/visual computer use [LATER]
      |
      v
observe -> validate -> execute one step -> observe
      |
      +--> ambiguity/risk -> palette or confirmation
      |
      v
typed result -> overlay / menu-bar UI
```

The planner is the brain, but it has no direct filesystem, shell, network, or
desktop access. It can only request capabilities published by the registry.
The daemon and platform implementations are the eyes and hands.

Voice Spotlight is the product's shared resolution front door. Both a direct
single action and an AI-planned step use the same providers, candidates,
ranking, target store, policy, and executor.

### 5.1 `sunoto-core`: mode-aware sessions

**[DONE]** The core contains a small session mode type:

```rust
pub enum SessionMode {
    Dictation,
    System,
}
```

The mode is captured when the shortcut press starts a session and remains
immutable through recording, transcription, and finalization. Stale partials
or finals continue to be rejected by session ID.

The hotkey/control event should become mode-aware, for example:

```rust
pub struct ModeHotkeyEvent {
    pub mode: SessionMode,
    pub edge: HotkeyEvent,
}
```

Dictation finals keep the existing deterministic and LLM polish/insertion
path. System finals bypass dictation polish and insertion and enter the System
planner. Dictation LLM polish must not rewrite application or file names in a
System command.

### 5.2 `sunoto-system`: pure planning and capability domain

The existing pure Rust crate contains the first form of:

- **[DONE]** typed unresolved intents;
- **[DONE]** deterministic parsing;
- **[DONE]** application and target candidate types;
- **[DONE]** normalization, alias matching, and ranking;
- **[DONE]** typed application actions and suggestion-selection contracts;
- **[DONE]** initial risk classification and selection policy; and
- **[DONE]** mockable resolver/executor fixtures.

It must evolve from a single unresolved intent into a bounded plan and reusable
capability vocabulary:

```rust
pub struct SystemPlan {
    pub goal: String,
    pub steps: Vec<PlannedCapabilityCall>,
    pub limits: PlanLimits,
}

pub struct PlannedCapabilityCall {
    pub capability: CapabilityId,
    pub arguments: TypedArguments,
    pub depends_on: Vec<StepId>,
}
```

The first native capability families are:

```rust
pub enum NativeCapability {
    FindTarget { query: String, kinds: TargetKinds },
    OpenApplication { target: ResolvedTargetId },
    OpenFile { target: ResolvedTargetId, application: Option<ResolvedTargetId> },
    RevealFile { target: ResolvedTargetId },
    OpenFolder { target: ResolvedTargetId, application: Option<ResolvedTargetId> },
    OpenProject { project: ResolvedTargetId, editor: Option<ResolvedTargetId> },
    WebSearch { query: String, browser: Option<ResolvedTargetId> },
    OpenUrl { url: ValidatedHttpUrl, browser: Option<ResolvedTargetId> },
}
```

There must never be a `ShellCommand(String)` or generic executable-and-args
variant. Only the resolver can create stable application and file identifiers,
and only the policy engine can authorize a resolved action.

### 5.3 Desktop platform capability

Extend the desktop facade with native providers and executors implemented by
each platform:

```rust
pub trait SystemPlatform {
    fn resolve_targets(
        &self,
        query: &TargetQuery,
        cancel: &CancellationToken,
    ) -> Result<Vec<TargetCandidate>, SystemError>;
    fn execute_native(
        &self,
        action: &ResolvedNativeAction,
    ) -> Result<ActionObservation, SystemError>;
}
```

All operations must use structured arguments or native APIs. User/model text
must never be interpolated into a shell command.

### 5.4 Capability registry and dispatcher

The daemon needs one registry that describes every currently available tool:

- stable capability ID and version;
- JSON-schema-like input and output contract;
- provider (`native`, `integration`, or later `computer_use`);
- platform availability and required permissions;
- risk class and confirmation rule;
- timeout, cancellation, and retry behavior; and
- whether observations may contain untrusted external content.

The planner receives only this catalog and limited goal context. Unknown
capabilities or arguments fail schema validation. The dispatcher resolves all
opaque IDs again before execution, applies policy per step, and reports a typed
observation.

### 5.5 Bounded plan runner

The runner executes one step at a time and supports an
`observe -> validate -> act -> observe` loop. Initial limits:

- no more than five planned steps for the native phase;
- one active System goal at a time;
- per-step and whole-plan deadlines;
- cancellation from Escape or a new System session;
- no automatic retries for state-changing or externally visible actions;
- no execution after a stale session, target, permission, or integration
  version; and
- completion only after the executor returns evidence of success.

The runner may combine equivalent native steps. For example, opening a URL in
Chrome can launch Chrome and navigate in one platform call.

## 6. Planning Strategy

### 6.1 Fast deterministic routes

Keep deterministic routes for obvious, latency-sensitive requests. They are
an optimization and safety baseline, not the product's language boundary.

Every supported intent must have deterministic patterns. Routing follows one
fixed order:

1. Normalize harmless ASR variation without changing target names.
2. Try the deterministic command catalog.
3. If exactly one deterministic intent matches, use it.
4. If deterministic patterns conflict, show clarification suggestions rather
   than invoking the LLM to break the tie.
5. If no deterministic intent matches, call the constrained local-LLM router.
6. Accept only a schema-valid bounded plan made from advertised capabilities.
7. Resolve every target and execute every step through the same deterministic
   registry, policy, and dispatcher used by fast routes.

Example patterns:

- `open <target>`
- `launch <application>`
- `find <file query>`
- `search for <file query>`
- `find <file query> and open it`
- `show <file query> in finder/files`
- `open <known folder>`
- `search the web for <query>`
- `go to <validated URL>`
- `open <browser> and go to <validated URL>`

The parser extracts an unresolved user query. It does not convert the query
into an executable path.

Application aliases should be data, not hard-coded parser branches:

```json
{
  "chrome": "com.google.Chrome",
  "visual studio code": "com.microsoft.VSCode",
  "vs code": "com.microsoft.VSCode"
}
```

The resolver must still verify that the referenced application is currently
installed.

### 6.2 Tool-calling AI planner

After the native capability dispatcher, plan runner, and complete single-action
Voice Spotlight flow are testable without a model, connect a tool-calling AI
planner. The planner handles natural, ambiguous, and multi-step goals. It may
request discovery tools, inspect their typed results, and then propose the next
capability call.

Requirements:

- Keep the planner behind a model-agnostic adapter. A local schema-constrained
  model is preferred for privacy; an explicitly configured remote model may be
  evaluated separately.
- Reuse a suitable already loaded local runtime where quality is sufficient;
  do not load a second multi-gigabyte model without measuring memory and
  dictation latency.
- Keep polish and System planning as logically separate request types,
  prompts, grammars, tests, and diagnostics.
- Produce schema-constrained capability calls or a terminal status of
  `Completed`, `NeedsClarification`, or `Unsupported`.
- Give the model the transcript, capability catalog, and minimum typed
  observations needed for the active goal. Do not provide file contents,
  clipboard contents, credentials, or unrelated screen/web content.
- Treat any model-supplied confidence value as untrusted. Execution confidence
  comes from deterministic resolution evidence and policy.
- Reject unknown tools or fields, malformed output, oversized arguments,
  control characters, unsupported URL schemes, cyclic dependencies, and
  plans over configured limits.
- On timeout or failure, perform no action.
- The LLM chooses capabilities; it cannot create executable commands or claim
  that an action succeeded. Every target and success result comes from a
  provider or executor.
- Treat text returned by files, webpages, applications, accessibility trees,
  and integrations as untrusted data, never as planner instructions.

This can be implemented by evolving the current warm LLM sidecar into a typed
local-LLM service with separate `polish` and `plan_system` requests, or by a
separate model adapter with measured resource isolation. The design must not
weaken the existing llama.cpp serialization lock or dictation latency path.

### 6.3 Example planner loop

For “Open who-else-is-free”:

```json
{"capability":"target.find","arguments":{"query":"who-else-is-free","kinds":["project","folder","file","application"]}}
```

The native provider returns real candidates. The planner may then request:

```json
{"capability":"project.open","arguments":{"project_id":"target:42","editor_id":"app:vscode"}}
```

Both IDs must belong to the active session's candidate store. Invented IDs are
rejected before policy or execution.

## 7. Resolution

Planning answers “what goal and capability does the user want?” Resolution
answers “which real local target did they mean?” The two stages must remain
separate. An unresolved noun after “open” is never assumed to be an
application.

### 7.1 Application inventory and matching

Each candidate should expose:

- stable platform ID;
- display name;
- normalized aliases;
- application URL or desktop-entry identity held only by the platform layer;
- icon reference for the chooser; and
- whether the candidate is visible/launchable for the current desktop.

Ranking should prioritize:

1. exact configured alias;
2. exact normalized display name;
3. exact token or common short-name match;
4. prefix match;
5. conservative fuzzy match.

Fuzzy matching must never silently suppress another plausible candidate.
Recently used applications may influence ranking only after the user has
explicitly launched them through System mode before; recency must not override
an exact name.

### 7.2 File search and ranking

The native foundation searches filenames and metadata, not file contents. Default search
scope should be user-controlled and limited to locations such as Desktop,
Documents, Downloads, and explicitly added folders.

Ranking should consider:

1. exact filename including extension;
2. exact normalized filename stem;
3. all query tokens present in the filename;
4. preferred search root;
5. modification recency; and
6. shallower path depth.

The search must be cancellable, time-bounded, result-capped, and executed away
from the daemon event loop. Hidden paths, package contents, system directories,
Trash, model directories, and application-support caches should be excluded by
default.

An Open suggestion is allowed only when:

- the user explicitly requested opening;
- the file is a regular safe document or folder;
- the canonical path is still inside an allowed search root;
- the item still exists at execution time; and
- the extension/MIME classification is not executable or otherwise denied.

Ranking may choose the initially highlighted suggestion, but ranking never
authorizes execution.

### 7.3 Unified target and project resolution

`target.find` fans out to enabled providers away from the daemon event loop:

- installed applications;
- known folders;
- configured file and project roots;
- recent projects from supported editors when available through native
  metadata;
- validated URLs and enabled browser sources; and
- application integrations only after the integration phase.

Candidates carry a kind, opaque ID, display metadata, match evidence, provider
ID, and expiry. Ranking is deterministic. The planner may choose among strong
unambiguous evidence but cannot manufacture a candidate.

A project is initially a folder under an approved project root with evidence
such as a VCS directory, language/build manifest, or recent-editor metadata.
Opening a project is a native file/folder-open operation with an explicitly
resolved editor. It does not require a VS Code-specific integration for the
first implementation.

## 8. Platform Implementation

### 8.1 macOS

- Discover application bundles in the user and system application domains and
  resolve them to stable bundle identifiers/application URLs.
- Launch or activate resolved applications through `NSWorkspace`, which
  provides asynchronous application-launch status.
- Open resolved file URLs through `NSWorkspace` using the system's registered
  application association.
- Use `NSMetadataQuery`/Spotlight metadata for local file discovery.
- Reveal ambiguous or user-selected results through Finder using a native
  workspace operation.
- Keep all AppKit/Launch Services details inside `sunoto-macos` or a small
  native helper owned by that crate.

Primary API references:

- [NSWorkspace application launching](https://developer.apple.com/documentation/appkit/nsworkspace/openapplication%28at%3Aconfiguration%3Acompletionhandler%3A%29)
- [NSMetadataQuery Spotlight search](https://developer.apple.com/documentation/foundation/nsmetadataquery)
- [NSWorkspace application association lookup](https://developer.apple.com/documentation/appkit/nsworkspace/urlforapplication%28toopen%3A%29-7qkzf)

Do not use AppleScript or construct `open -a <user text>` shell commands in the
production executor.

### 8.2 Linux

- Enumerate installed, visible applications through GIO `GAppInfo` or an
  equivalent binding that respects registered desktop applications.
- Launch a resolved application through its `GAppInfo`, including D-Bus
  activation where the desktop entry requests it.
- Never independently execute an unparsed `.desktop` `Exec` string with a
  shell.
- Open files/URIs through GIO for a host package and the XDG Desktop Portal
  `OpenFile`/`OpenURI` APIs where sandboxing or desktop integration requires
  them.
- Use a provider interface for file search: prefer an available desktop index
  service; otherwise perform a cancellable, bounded Rust filename walk inside
  user-approved roots. Do not spawn an unbounded `find /` command.
- Preserve desktop activation context when launching applications.

Primary API references:

- [GIO AppInfo](https://docs.gtk.org/gio/iface.AppInfo.html)
- [GIO AppInfo launch](https://docs.gtk.org/gio/method.AppInfo.launch.html)
- [Desktop Entry Specification](https://specifications.freedesktop.org/desktop-entry/latest/)
- [XDG Desktop Portal OpenURI/OpenFile](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.OpenURI.html)

### 8.3 Native implementation order

Implement and validate native capabilities in this order:

1. **[DONE]** macOS application inventory, launch, activation, selection, and
   cancellation; **[PARTIAL]** equivalent Linux live validation;
2. **[DONE]** capability registry, dispatcher, bounded observations,
   cross-step output binding, system-owned limits, and bounded runner using the
   existing application flow plus a fake planner;
3. **[NEXT]** URL normalization plus default/specified-browser open;
4. **[NEXT]** known folders and bounded file/project discovery;
5. **[NEXT]** native open/reveal, unified `target.find`, cross-provider
   ranking, and the complete Voice Spotlight palette;
6. **[NEXT]** real AI planner over the completed native single-action
   capability set; and
7. **[LATER]** integrations, semantic accessibility, and visual computer-use
   backends.

The dispatcher and runner are proven with applications and URLs first. The
real planner is connected only after Voice Spotlight can reliably find and act
on applications, URLs, files, folders, projects, and web searches.

## 9. Sustainable Application Integrations

Application-specific behavior comes after the native capability foundation.
An integration adds capabilities such as playing media or creating a note; it
must not add new parsing branches to the daemon.

### 9.1 Design goals

- Adding an integration must not require modifying or recompiling Sunoto core.
- Simple integrations should be declarative and editable by a technically
  comfortable user.
- Complex integrations run as isolated sidecars over typed NDJSON, following
  the repository's existing sidecar pattern.
- The planner discovers integrations from the same capability registry used
  by native tools.
- Integrations declare permissions and risk; installing one does not silently
  grant every capability.
- Capability contracts are versioned and testable without launching the real
  application.

### 9.2 Package format

An integration package contains:

```text
spotify/
  integration.json
  workflows/                 # optional declarative actions
  sidecar/                    # optional advanced implementation
  tests/
  README.md
```

`integration.json` declares:

```json
{
  "schema_version": 1,
  "id": "community.spotify",
  "name": "Spotify",
  "version": "0.1.0",
  "platforms": ["macos", "linux"],
  "applications": ["com.spotify.client", "spotify.desktop"],
  "capabilities": [
    {
      "id": "media.search_and_play",
      "description": "Search Spotify and play a matching track",
      "input_schema": {
        "type": "object",
        "required": ["query"],
        "properties": {"query": {"type": "string", "maxLength": 200}}
      },
      "risk": "reversible",
      "confirmation": "never"
    }
  ]
}
```

The manifest never contains a free-form command template. Declarative
workflows may use only capability building blocks approved by Sunoto. Advanced
sidecars receive validated inputs and return typed observations; they never
receive a general daemon command channel.

### 9.3 Author experience

Provide:

- `sunoto integration init` to scaffold a manifest, handler, tests, and docs;
- `sunoto integration validate` for schema, permission, and risk checks;
- `sunoto integration test` with a fake planner and recording executor;
- a small SDK for Rust and Python handlers;
- reusable standard contracts such as `media.search`, `media.play`,
  `editor.open_project`, and `notes.create`; and
- an installation flow that displays requested permissions and supports
  disable/uninstall without editing the main config manually.

For non-programmers, a later integration builder can map a spoken capability
to an existing OS Shortcut, desktop action, URL scheme, or approved sequence
of Sunoto capabilities. It must produce the same manifest and pass the same
validator.

### 9.4 Bundled integrations

After the SDK is stable, add a small representative set:

1. browser integration for tab/search operations beyond native URL opening;
2. VS Code integration for recent workspaces and editor-specific actions; and
3. Spotify integration for search, play, pause, and now-playing observation.

These are reference implementations for third-party authors, not special
cases in the planner.

### 9.5 Integration trust and lifecycle

- Integrations are disabled until explicitly installed and enabled.
- Each capability has declared permissions, data access, network access, risk,
  timeout, and confirmation behavior.
- The daemon pins the integration ID/version for an active plan and rejects a
  changed or missing handler.
- Results are untrusted data and cannot introduce new planner instructions.
- Credentials remain in OS credential storage or provider authorization flows;
  they are never exposed to the planner transcript.
- Third-party sidecars should be sandboxed where practical and receive only
  the minimum capability-specific input.

## 10. Safety Policy

### Risk classes

| Class | Examples | Native-phase behavior |
| --- | --- | --- |
| **A: reversible launch/navigation** | Installed app, known folder, web search | May appear as a selectable suggestion |
| **B: local content open** | Open or reveal an existing safe document | May appear after path/type validation; user selects it |
| **C: state-changing** | Close app, move/rename file, change setting | Unsupported natively; later requires capability-specific confirmation and undo design |
| **D: destructive/privileged/external** | Delete, install, shell, admin, send, purchase | Denied; no executor variant |

### Required invariants

- Mode activation is explicit and stored per session.
- One session owns at most one bounded plan; a plan may contain multiple typed
  steps within configured limits.
- No action is a shell string.
- The planner cannot name an executable path.
- The resolver cannot bypass the allowlisted action enum.
- The executor accepts only a resolved typed action.
- Every step receives a policy decision. Ambiguous and confirmation-required
  steps cannot run without a current user decision tied to the active plan.
- Auto-execution, if introduced for reversible unambiguous actions, is an
  explicit user setting and is never available for state-changing/external
  actions.
- Canonical paths are revalidated immediately before execution.
- Symlink resolution cannot escape allowed roots unnoticed.
- Unsafe document types are denied before open.
- Ambiguity produces UI, not a guess.
- Unsupported, malformed, timed-out, or stale plans perform no action.
- A stale result from a prior session cannot execute.
- A stale, replayed, or already-consumed suggestion ID cannot execute.
- Normal dictation can never reach the System executor.
- System transcripts and file queries are redacted from normal logs.
- Every attempted action produces a local metadata-only audit event containing
  action type, outcome, policy decision, latency, and redacted target ID.
- Planner and integration observations are untrusted data; their text cannot
  request capabilities or override policy.
- A plan stops on failed verification unless an allowed retry/recovery rule is
  explicit.

## 11. User Interface

### Recording overlay

- Distinct System color and icon
- `System — listening`
- Microphone level
- Partial command transcript, if enabled
- `Understanding command…`
- `Searching files…`
- `Opening Google Chrome…`
- Success or concise error

### Voice command palette

Use a native plan/palette window whenever the goal is ambiguous, requires
confirmation, or is configured to show previews. It should show:

- the recognized command;
- the planned action or short sequence in plain language;
- a ranked list of resolved application, file, folder, URL, or web-search
  suggestions;
- application icons or file names, types, and parent folders;
- the action each row will perform, such as Open, Reveal, or Search Web;
- why the suggestion matched and any relevant safety warning;
- one highlighted result;
- Up/Down navigation, Enter to select, pointer selection, and Escape to
  cancel; and
- no selected result after its underlying target becomes invalid.

The first version should show at most five primary results plus a small number
of clearly separated alternative actions. Suggestions must be real, resolved
targets or validated actions; the LLM must not invent application names,
files, or URLs for display.

The palette should support:

- Open/Reveal/Search/Cancel controls as appropriate; and
- Escape as a reliable cancel action.

The window must not appear or take focus while the user is still recording.

### Settings

Initial/development settings:

```json
{
  "system_mode_enabled": false,
  "system_shortcut": "Ctrl+F2",
  "system_require_selection": true,
  "system_max_plan_steps": 5,
  "system_auto_execute_reversible": false,
  "system_allowed_actions": [
    "open_application",
    "find_file",
    "open_file",
    "reveal_file",
    "open_folder",
    "web_search",
    "open_url"
  ],
  "system_search_roots": ["Desktop", "Documents", "Downloads"],
  "system_llm_fallback_enabled": true
}
```

System mode should be disabled behind a feature flag during development and
enabled only after onboarding explains its scope and verifies the second
shortcut. The AI planner remains gated until the native dispatcher, bounded
runner, and single-action Voice Spotlight flows pass their safety and quality
tests. In the finished product, natural, ambiguous, and multi-step goals route
through the planner while obvious commands retain the deterministic fast path.

Users should be able to manage application aliases, search roots, allowed
action categories, integrations, permissions, and suggestion ranking
preferences. Auto-execution remains off throughout native development and may
be introduced only after risk-specific metrics are available.

## 12. Control Protocol and Testability

The control socket already has mode-specific triggers and dry-run planning.
Evolve their response from a single route into the bounded plan contract:

```json
{"type":"trigger","mode":"system","edge":"press"}
{"type":"trigger","mode":"system","edge":"release"}
{"type":"plan_system","text":"open chrome","dry_run":true}
```

`dry_run` must return the fast route or planner calls, plan steps, candidates,
resolution evidence, risk decisions, and typed observations without executing
anything.

Example response:

```json
{
  "type": "system_plan",
  "ok": true,
  "goal": "Open Chrome and go to google.com",
  "steps": [
    {
      "id": "step-1",
      "capability": "application.find",
      "arguments": {"query": "Chrome"},
      "policy": "read_only"
    },
    {
      "id": "step-2",
      "capability": "url.open",
      "arguments": {
        "url": "https://google.com",
        "application_from": "step-1"
      },
      "depends_on": ["step-1"],
      "policy": "present_suggestion"
    }
  ],
  "would_execute":false
}
```

The existing Wayland `press` and `release` forms may remain temporarily for
backward compatibility, but new compositor templates should use typed mode
commands.

## 13. Verification Strategy

### Unit tests

- Session mode remains immutable across press/release/final.
- Stale System finals and plans cannot execute.
- Deterministic parser accepts every documented phrase.
- Safe documented multi-action utterances produce bounded dependency-ordered
  plans; unsafe or oversized plans are rejected.
- Application alias and ranking behavior is deterministic.
- Planner output never bypasses resolution, policy, or dispatcher validation.
- Every confirmation-required execution consumes one current decision token.
- File ranking and allowed-root checks are deterministic.
- Canonicalization and symlink-escape cases are denied.
- Executable/script/installable file types are denied.
- Policy covers every action enum variant with a fail-closed default.
- Serialized planner output cannot contain unknown action types.
- The plan runner enforces step, time, cancellation, dependency, and retry
  limits.
- Integration manifests and sidecar messages pass conformance tests.

### Adversarial corpus

Include transcripts and candidate names containing:

- quotes, backticks, semicolons, pipes, redirects, `$()`, and newlines;
- shell-looking text such as `open Chrome; rm -rf ...`;
- path traversal and symlink escapes;
- application names similar to system utilities;
- misleading filenames and double extensions;
- unsupported URL schemes;
- safe, unsafe, ambiguous, and oversized commands joined with “and”;
- ASR confusions such as Chrome/Chromium and Finder/finder; and
- stale or replayed session IDs.

No adversarial case may reach a platform executor unless its final typed
action is independently expected and allowlisted.

### Mock end-to-end tests

Use the mock ASR and a recording executor that captures typed actions without
touching the real desktop:

```text
system press -> mock audio -> system release -> mock ASR final
-> parser -> resolver fixture -> policy -> recording executor
```

Test exact, ambiguous, denied, no-match, timeout, cancellation, and stale-event
flows. Add fixture planner conversations that request discovery, receive typed
candidate observations, select a resolved ID, and complete only after an
executor observation.

### Live platform tests

Live tests must use harmless fixtures and never execute arbitrary text:

- launch a known test application;
- activate an already running application;
- find a uniquely named temporary text file in an approved root;
- display an ambiguous file chooser;
- open and reveal the temporary text file;
- open a harmless HTTP test URL;
- revoke permissions or remove a target between resolution and execution;
- confirm cancellation performs no action; and
- confirm normal dictation still inserts the same text as before.

### Quality metrics

- intent accuracy;
- slot/query exactness;
- application resolution accuracy;
- file top-1 and top-5 recall;
- false-action rate;
- suggestion top-1 and top-5 usefulness;
- selection-to-action success rate;
- command release-to-action latency p50/p95;
- no-match and cancellation latency;
- planner timeout rate;
- planner tool-call/schema error rate;
- average and maximum plan steps;
- observation-to-action verification rate;
- integration capability success rate; and
- per-platform execution success rate.

**Safety gate:** no plan, match, model response, or integration result can
bypass resolution, policy, and dispatcher validation. False execution must be
zero across the maintained negative, ambiguous, multi-action, and adversarial
corpora. Any uncertainty must result in clarification or no action. During the
native development phases, `system_auto_execute_reversible` remains false and
all executions continue to require explicit selection.

## 14. Delivery Plan

### Phase 0 — Safety contracts and mode boundary **[DONE]**

- Typed unresolved intents, candidates, suggestions, risk classes, and the
  initial resolved application action.
- No shell-command or generic executable action.
- Immutable session mode and fail-closed routing.
- Natural-language, routing, replay, and adversarial unit coverage.

**Exit achieved:** System and Dictation sessions cannot cross execution paths,
and arbitrary model/user strings cannot become executables.

### Phase 1 — Mode-aware fixture vertical slice **[DONE]**

- Separate configurable shortcut and mode-aware control events.
- Shared ASR with System-specific finalization.
- Distinct overlay and native command-palette protocol.
- Fixture resolver/executor with one-time selection and cancellation.
- Dry-run `system plan` and `system resolve` commands.

**Exit achieved:** fixture “Open Chrome” reaches one recorded typed action only
after a valid current selection.

### Phase 2 — Native application launching **[DONE macOS / PARTIAL Linux]**

Implemented:

- macOS `.app` discovery, opaque IDs, revalidation, and `NSWorkspace` launch;
- Linux visible Desktop Entry discovery and `GAppInfo` launch;
- dedicated blocking System worker;
- macOS and GTK palettes with keyboard/pointer selection and cancel; and
- fail-closed no-match, stale, invalid, overlay-loss, and launch-failure paths.

Current product result:

- the live macOS Ctrl+F2 flow opens a selected installed application; and
- cancellation and invalid/stale selection paths remain fail-closed.

Remaining cross-platform validation:

- add Linux post-launch evidence (the current GIO executor reports
  `LaunchRequestedOnly`, which the dispatcher correctly refuses to call
  success); and
- exercise the Linux provider and GTK palette on supported X11/Wayland hosts.

**Exit achieved on macOS:** “Open Chrome” resolves and opens the installed
application through native APIs. Linux reaches the same exit after post-launch
evidence and host validation are complete.

### Phase 3 — Capability contract and bounded runner **[DONE]**

Implemented and verified:

- stable `application.find` and `application.open` capability IDs, compact
  schemas, risk/confirmation metadata, typed calls, and typed observations;
- native capability dispatcher with opaque short-lived target IDs, explicit
  selection authorization, revalidation, one-time consumption, and stale
  discovery invalidation;
- the live macOS application flow migrated from `ResolveApplications` to the
  dispatcher while retaining worker isolation and explicit selection;
- bounded sequential runner with step-count checks, dependencies,
  cancellation, terminal states, failure-stop behavior, and observations;
- serializable typed bindings that resolve an opaque target from a prior
  successful discovery observation without exposing or inventing native IDs;
- system-owned five-step, ten-second-per-step, and thirty-second-whole-plan
  ceilings, with the capability contract able to impose a smaller deadline;
- fake planner, recording dispatcher/executor, and deterministic tests;
- capability-specific launch evidence: a launcher request alone is not treated
  as completed; and
- environment-derived application observations classified as untrusted; and
- read-only resolve output exposes ranked, query-bounded target observations,
  capability steps, dependencies, and policy without enabling execution.

Verification on July 22, 2026:

- `cargo test --workspace --offline` passes;
- `cargo clippy --workspace --offline --all-targets -- -D warnings` passes;
- focused `sunoto-system` tests pass (39 unit tests plus 3 routing-corpus
  tests); and
- all Rust files changed for Phase 3 are formatted. The workspace-wide format
  check still reports unrelated differences in other dirty files.

**Exit achieved:** the existing “Open Chrome” flow works through the general
dispatcher, and a fixture multi-step plan resolves data in one step, binds it
into the next step, observes, cancels, times out, and fails safely without a
model.

### Phase 4 — Native URL and browser navigation **[DONE macOS / PARTIAL Linux live]**

**Implementation evidence (July 22, 2026):** URL inputs now use the Rust
`url` parser with a narrow HTTP(S)/DNS/IDNA policy, credential and control
character rejection, and data-only UTF-8 search construction. Deterministic
routes now include `open <url>` and `open <browser> and go to <url>`; the
latter presents browser discovery, preserves an opaque selected target, and
binds it only into `url.open`. Native dispatch revalidates the selected
application and its declared HTTP(S) handling before opening the URL. Focused
`sunoto-system` and daemon tests plus clippy pass, and read-only plan output
shows validated URL steps without native paths or executable commands.

**Exit evidence (July 22, 2026):** the daemon-level mock integration test
drives transcript → deterministic compound route → one-time palette selection
→ selected-browser target authorization → bounded runner → typed `UrlOpened`
observation without opening a browser. The opt-in native macOS test exercised
both default-browser and selected installed Google Chrome navigation through
`NSWorkspace` using harmless `https://example.com/?sunoto=…` URLs. Linux has
the same fixture contract and remains explicitly partial for live desktop
validation/post-launch evidence.

**Goal:** make websites and web searches first-class resolved targets that can
open in the default browser or a specifically resolved browser through native
APIs.

- Add HTTP/HTTPS normalization and validation.
- Add default-browser and explicitly resolved-browser URL open on macOS/Linux.
- Add web-search URL construction with strict encoding.
- Support a deterministic bounded plan for “Open Chrome and go to google.com.”

**Exit:** “Go to google.com” opens the default browser and “Open Chrome and go
to google.com” opens it in resolved Chrome without UI clicking or shell text.

**Proof required before `[DONE]`:**

- table-driven normalization tests for spoken domains, explicit URLs, query
  encoding, Unicode/IDN handling, malformed input, and denied schemes;
- fixture plans prove default-browser and specified-browser paths, cross-step
  binding, cancellation, stale-target rejection, and timeout behavior;
- read-only dry-run shows the validated URL and resolved browser without an
  executable command;
- harmless live macOS checks cover default browser and installed Chrome; and
- Linux fixture coverage passes, with live X11/Wayland validation tracked
  separately when a Linux host is available.

### Phase 5 — Voice Spotlight target breadth **[DONE macOS / PARTIAL Linux live]**

**Implementation evidence (July 22, 2026):** `target.find` now fans a typed
intent into installed-application discovery and approved local roots instead
of treating generic `open` as application-only. Relative roots resolve below
the user's home and default to Desktop, Documents, Downloads, and the
conventional `workspace` folder. macOS Spotlight (`mdfind`, fixed arguments,
one-second kill deadline) feeds the same canonical approved-root classifier as
the cancellable bounded filesystem fallback. Linux uses that bounded fallback.
Both platforms retain canonical paths only in private opaque stores, cap
results, skip hidden/system-like subtrees, reject symlink escapes, executables,
unsafe/double extensions, expired selections, disappeared or changed targets,
and revalidate immediately before native execution.

Files, folders, and projects now share the application/URL capability store,
ranking, selection, runner, dispatcher, observations, and one-time replay
protection. Project evidence comes from common repository markers, while
editor candidates come from installed application metadata and aliases. “Open
who-else-is-free in VS Code” creates combined project/editor rows (including
ambiguous editor alternatives) and NSWorkspace/GIO receives only the selected
canonical project plus resolved application identity. There is no shell,
AppleScript, transcript-derived executable, or arbitrary argument escape hatch.

**Exit evidence (July 22, 2026):** read-only `system plan` emits a non-empty
`target.find` step for the project/editor phrase with
`execution_allowed=false` and no native ID/path. `system resolve "open
who-else-is-free"` returned the real project as top-1 plus four useful related
projects in the top five, with kind/location evidence and opaque session
tokens. The daemon fixture test covers transcript → deterministic route →
unified target observation → palette → one-time selection → bounded runner →
typed `LocalTargetOpened(Project)` without desktop effects. Focused Rust tests
cover exact/alias/cross-kind/recency ranking, ambiguity, no-match, cancellation,
deadlines, result caps, expiry, replay, safe roots, symlink escape, hidden and
unsafe files, target disappearance, local planned-target binding, and all six
palette kinds. The disposable quality gate records 100% top-1/top-5 recall,
zero false actions, sub-500ms fixture latency, and sub-50ms cancellation.

The opt-in native macOS tests passed for disposable safe-file open, Finder
reveal, folder open, and opening the real `~/workspace/who-else-is-free`
project in the resolved installed Visual Studio Code through NSWorkspace. The
portable Linux implementation was independently compiled from its real source
module on the macOS host; a live Linux desktop was unavailable, so GIO and GTK
observation remains explicitly partial rather than being inferred from stubs.

The repository-local `system-mode-e2e` skill now makes these distinctions
repeatable. Its negative fixture produced 8 passes and the single intentional
failure with a nonzero exit. Two subsequent independent non-live runs
(`run-20260722-d` and `run-20260722-e`) each produced 21 passes, zero failures,
zero skips, a clean mock-daemon shutdown, redacted logs, and matching JSON,
Markdown, HTML, and image evidence. The daemon proof uses mock ASR plus a
headless cancel-only overlay to exercise the real control socket, System
capture, `target.find`, worker discovery, and palette presentation without
selecting a native target or typing into a focused application. The ignored
Linux GIO live tests cover an installed HTTP handler, harmless URLs, and
disposable file/folder/project open and reveal, but remain unclaimed until an
explicitly confirmed run on a Linux desktop.

**Goal:** provide one useful voice-first palette that searches and ranks real
applications, files, folders, projects, websites, and web-search actions
without assuming every “open” target is an application.

- Add user-approved search roots and known-folder providers.
- Add macOS Spotlight and Linux indexed/bounded search providers.
- Add canonical IDs, expiry, safe-type checks, and symlink/root revalidation.
- Add project detection and preferred/recent editor evidence.
- Implement native open/reveal for files, folders, and projects.
- Fan out `target.find` across application, project, folder, file, and URL
  providers and rank their evidence together.
- Extend the palette with target kind, location, match evidence, and
  clarification.
- Ensure every single-action request uses the same target store, policy,
  selection, execution, and verification path that planned steps will use.

**Exit:** “Open who-else-is-free” finds a real project/folder/application
without assuming it is an app; “Open who-else-is-free in VS Code” opens the
resolved project in the resolved editor. Applications, files, folders,
projects, URLs, and web searches appear in one useful voice-first palette.

**Proof required before `[DONE]`:**

- provider contracts share opaque IDs, expiry, evidence, cancellation, result
  caps, and untrusted-data classification;
- deterministic ranking tests cover exact names, aliases, competing target
  kinds, ambiguous duplicates, recency, and no-match behavior;
- file/project tests cover approved roots, symlinks, hidden/system paths,
  unsafe file types, disappearing targets, and bounded search deadlines;
- “Open who-else-is-free” returns the actual local project in a fixture and on
  the current macOS host, while an unknown phrase does not become an app-only
  error;
- opening/revealing a harmless fixture works after selection and never before;
  and
- provider latency, top-1/top-5 recall, cancellation, and false-action metrics
  meet documented thresholds on macOS and supported Linux environments.

### Phase 6 — AI multi-step planner over Voice Spotlight capabilities **[NEXT]**

**Goal:** let a model compose only the already-proven Voice Spotlight
capabilities into short observe/act/verify plans while deterministic routes
remain fast and all policy stays outside the model.

- Add a model-agnostic planner adapter and schema-constrained local planner.
- Advertise only capabilities already proven through the single-action Voice
  Spotlight flow.
- Keep deterministic fast routes for obvious commands.
- Add fixture and real-model conversations for natural phrasing, ambiguity,
  short multi-step navigation, failure, and clarification.
- Reject invented target IDs, unknown capabilities, malformed calls, and plans
  over configured limits.
- Measure planner accuracy, latency, malformed-call rate, and interaction with
  ASR/LLM GPU residency.

**Exit:** natural variations and short multi-step goals compose the existing
Voice Spotlight capabilities through the same dispatcher, with zero policy
bypasses in the adversarial corpus.

**Proof required before `[DONE]`:**

- a versioned planner request/response schema supports capability calls,
  clarification, completion, and unsupported outcomes;
- a fixed evaluation corpus measures intent/tool accuracy, slot exactness,
  invented-ID rate, malformed-call rate, clarification quality, and latency;
- adversarial observations prove webpage/file/application text cannot add
  tools, change policy, or claim success;
- plans respect hard limits, explicit selection/confirmation, cancellation,
  stale IDs, and capability-specific success evidence;
- live harmless goals include “Open Chrome and go to google.com” and “Find the
  test project and open it in the selected editor”; and
- ASR latency and GPU/memory residency remain within measured budgets with the
  planner enabled and disabled.

### Phase 7 — Native beta hardening **[LATER]**

**Goal:** make the complete native Voice Spotlight and multi-step experience
predictable for non-technical users across restarts, permissions, failures,
and supported desktops.

- Finish onboarding, settings, permissions, cancellation, sleep/wake, and
  process-restart behavior.
- Complete the macOS and Linux desktop support matrix.
- Keep auto-execution disabled until reversible-action metrics justify a
  separate opt-in experiment.
- Redact sensitive goal/target content from normal logs.

**Exit:** non-technical beta users can predict, inspect, cancel, and recover
from every supported native action.

**Proof required before `[DONE]`:**

- onboarding validates shortcuts, permissions, search roots, and planner
  availability;
- sleep/wake, daemon/sidecar restart, overlay loss, permission revocation, and
  stale palette recovery tests pass;
- the macOS and Linux compatibility matrix states exactly which native
  capabilities are available and verified;
- logs contain useful metadata while redacting transcripts, paths, queries,
  target IDs, and credentials; and
- beta sessions meet release thresholds for success, cancellation, latency,
  no-match quality, and zero false destructive/external actions.

### Phase 8 — Integration package contract and SDK **[LATER]**

**Goal:** allow an application integration to be added, validated, installed,
disabled, and tested without editing or recompiling Sunoto core.

- Implement integration discovery, manifest validation, permissions, version
  pinning, enable/disable, and typed sidecar transport.
- Add `integration init`, `validate`, and `test` tooling.
- Publish standard capability contracts and fixture/conformance suites.
- Add a declarative workflow format limited to approved Sunoto capabilities.

**Exit:** a new integration can be created, tested, installed, and disabled
without changing or rebuilding the daemon.

**Proof required before `[DONE]`:**

- manifest, capability schema, permission, version, lifecycle, and typed NDJSON
  contracts are documented and machine-validated;
- `integration init`, `validate`, and `test` generate and verify a minimal
  example from a clean directory;
- invalid schemas, undeclared permissions, unknown tools, stale versions,
  crashes, timeouts, oversized output, and prompt-like result text fail closed;
- enable/disable/uninstall takes effect without modifying core code or leaving
  a running sidecar; and
- a third-party-style fixture integration passes the public conformance suite
  using only published APIs.

### Phase 9 — Reference application integrations **[LATER]**

**Goal:** prove the public SDK with useful browser, VS Code, and Spotify
integrations that contain no hidden core special cases.

- Add browser, VS Code, and Spotify reference integrations.
- Exercise authorization, permission review, unavailable application, stale
  integration, and ambiguous-result paths.
- Document how community authors add capabilities without adding parser cases.

**Exit:** “Play Despacito on Spotify” uses the same public integration contract
available to community authors.

**Proof required before `[DONE]`:**

- each reference integration installs through the public package mechanism and
  passes the same conformance suite as community packages;
- browser tests cover tab/search operations, VS Code tests cover recent and
  explicit workspaces, and Spotify tests cover search, ambiguity, play/pause,
  authorization loss, and now-playing evidence;
- missing applications, logged-out accounts, network failure, API changes,
  stale results, and revoked permissions produce honest recoverable outcomes;
  and
- removing every bundled integration still leaves native Voice Spotlight and
  the planner functional.

### Phase 10 — General accessibility and visual computer use **[LATER]**

**Goal:** provide a permissioned, observable fallback for applications without
native capabilities or integrations, preferring semantic accessibility over
visual coordinates.

- Add macOS Accessibility and Linux AT-SPI semantic element providers.
- Add Wayland portal/X11 screen observation and input backends with explicit
  permissions.
- Prefer stable element IDs/roles/labels; use visual coordinate interaction
  only when semantic access is unavailable.
- Treat all visible application/web text as untrusted data.
- Require step verification and stricter confirmation for generic UI actions.

**Exit:** unsupported applications can perform bounded visible workflows while
preserving cancellation, observation, and policy boundaries.

**Proof required before `[DONE]`:**

- macOS Accessibility and Linux AT-SPI expose stable role/label/value element
  observations with explicit permission diagnostics;
- Wayland portal and X11 screen/input backends have separate capability and
  permission tests;
- semantic targets are preferred, coordinates are tied to a fresh observation,
  and every action is followed by visible-state verification;
- prompt injection, moving windows, stale screenshots/elements, inaccessible
  controls, focus changes, and emergency cancellation fail closed; and
- a maintained harmless workflow suite passes on each claimed desktop without
  passwords, purchases, sending, deletion, installation, or security-setting
  changes.

## 15. Expansion Rules

A new native capability or integration capability requires:

- a documented user benefit and stable typed input/output schema;
- an explicit risk class, permissions, and confirmation rule;
- platform or integration semantics and availability reporting;
- timeout, cancellation, verification, and recovery behavior;
- negative, adversarial, and conformance tests; and
- no arbitrary command string, executable-and-args escape hatch, or hidden
  planner access.

Native capabilities belong in reviewed platform code. Application-specific
capabilities belong in packages loaded through the public integration
contract. File mutation, messaging, installation, credentials, purchases, and
privileged actions require separate threat models and cannot arrive as
incidental integration features.

## 16. Recommended Next Implementation Slice

Work only on the native foundation now:

```text
preserve the working macOS application flow
-> validated HTTP/HTTPS targets
-> default/specified-browser native open
-> deterministic “open Chrome and go to google.com” plan
-> native files, folders, projects, and web search
-> unified target.find and Voice Spotlight palette
-> macOS/Linux single-action fixture and live verification
-> real AI planner over the proven Voice Spotlight capabilities
```

Do not implement Spotify, VS Code-specific behavior, generic accessibility
clicking, or third-party sidecars before Voice Spotlight is complete and the
small real planner loop is working. Build the integration SDK only after native
targets and the AI tool-calling runner share one stable capability contract.

## 17. Independent Session Handoff

Use
[`docs/system-mode-independent-session-prompt.md`](system-mode-independent-session-prompt.md)
to start a fresh session that implements and verifies Phases 4–7 in order. The
handoff deliberately stops before the integration SDK and general
accessibility/visual computer-use phases.
