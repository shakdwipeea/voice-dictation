# The pill as part of the notch ("Dynamic Island" on the Mac): plan

**Created:** 2026-10-16.
**Status:** implemented on branch `notch-island` (2026-10-16), steps 0–4 and
the docs part of step 5. Not released yet. The owner's look-and-feel check
(§6) is still to do. See §9 for what was built and checked.
**Scope:** the macOS overlay (`services/macos/sunoto-overlay.swift`), plus
one setting in the daemon (`apps/daemon/src/settings.rs`). Linux has no
notch, so its GTK pill stays as it is. System mode's command palette is
unchanged.
**Reading of the request:** "Dynamic Island, at least in iOS" is read as
**macOS on a MacBook with a notch**. The notch is the Mac's version of the
island, and Sunoto has no iPhone app. A real iPhone Dynamic Island (a Live
Activity) would need an iOS app and is out of scope.

## 1. Goal

On a Mac with a notch, the pill should look like it grows out of the notch,
the way the iPhone's Dynamic Island grows out of the camera cutout:

1. While idle, nothing is drawn. The notch itself is the island at rest.
2. When a dictation starts, the black notch widens into "shoulders" on both
   sides: the record dot and timer on the left, the live sound ribbon on the
   right.
3. When there are words to show (the start hint, a slow "Transcribing…", a
   health message, an error), the island grows down under the notch to make
   a row for them, then shrinks back.
4. After a paste, the dot turns into the check mark, and the island pulls
   back into the notch until it is gone.

On a screen without a notch (external displays, a closed lid, Macs without
a notch), Sunoto keeps today's floating pill.

## 2. What exists today

- The pill is one borderless, non-activating `NSPanel` that ignores the
  mouse (`makePillPanel`, `services/macos/sunoto-overlay.swift:1679`). Its
  level is `.statusBar` (`:1693`).
- It sits 14 pt below the menu bar, centred on `NSScreen.main`'s
  `visibleFrame` (`targetFrame`, `:1852`). The window resizes with the
  pill's content (`fitToContent`, `:1870`). That resize path has caused
  bugs before (the 2026-10-16 "pill stayed too tall" fix).
- `PillView` (`:101`, about 760 lines) mixes three things: the state
  machine (phases, audio levels, captions, timings), the open and close
  motion (`PillMotion`, the dot "unrolling" to the right), and the drawing.
- The drawing pieces can be reused: the record dot that breathes with the
  voice (`drawCaptureMark`, `:668`), the check (`:705`), the ribbon and its
  glint (`:745`), the timer (`:804`), the spinner and warning (`:827`,
  `:843`). They draw at fixed positions (`markX`, `center`), so they need
  their positions passed in.
- The fold after a paste carries the check to the **middle** of the pill
  (`515caa4a`). Under the notch, the middle is the camera, so the island
  needs a different ending.
- The minimum macOS is 13.0 (`apps/daemon/src/setup.rs:862`). The notch
  APIs (`NSScreen.safeAreaInsets`, `auxiliaryTopLeftArea`,
  `auxiliaryTopRightArea`) arrived in macOS 12, so they need no version
  checks.
- The daemon does not need to know about any of this. The overlay protocol
  (`show`, `recording`, `status`, `pasted`, `state`, `hide`) already carries
  everything the island needs. `sunoto-ipc` does not change.

### Measured on this Mac (2026-10-16)

| Fact | Value |
| --- | --- |
| Built-in screen | 1496 × 967 pt, scale 2 |
| Notch | 160 pt wide (x 668–828), 28 pt tall (`safeAreaInsets.top`) |
| Menu bar | 29 pt |
| Menu bar window level | 24 (`.mainMenu`) |
| Status items (Control Centre) | 25 (`.statusBar`), the same level as today's pill |
| Borderless panel placed flush with the top edge | AppKit kept the frame exactly; it was not pushed under the menu bar |

Other MacBooks have other notch sizes, and scaled display modes change them
in points, so the code must always read the notch from `NSScreen`, never
from constants.

## 3. Decisions for the owner

| # | Question | Recommendation | Why |
| --- | --- | --- | --- |
| D1 | Default | **On.** New setting `overlay_style: "auto"`: the island on a screen with a notch, the floating pill elsewhere. `"pill"` turns the island off. | This is the request. The opt-out covers people who run another notch app (NotchNook, Boring Notch, Alcove) or dislike the island. |
| D2 | Screens without a notch | **Keep the floating pill.** | A fake black island on a screen with a visible menu bar reads as a black block over the menu items. |
| D3 | Compact layout | **Dot and timer on the left, sound ribbon on the right.** | This is how the iPhone shows a phone call (time on the left, live audio on the right). It balances the two shoulders. Step 0's mockups also show the alternative (dot left; ribbon and timer right) before we commit. |
| D4 | Start hint ("Tap again to paste, Esc to cancel") | **Keep it** in a row under the notch. It shows for at most 3 s and goes as soon as you talk, as today. | First-time users need it. The shoulders are too narrow for it. |
| D5 | Check mark after a paste | **In the left shoulder, where the dot was**, then the island pulls back into the notch. | The middle of the island is under the camera. |

## 4. Design

### 4.1 Shapes and sizes

Numbers below are for this Mac. Shoulders are a constant, about 76 pt each;
the notch comes from the screen.

```
 compact (recording)
 ┄┄ menus ┄┄┄┄╭━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╮┄┄┄ status items ┄┄
              ┃ ● 0:04     ▓▓▓▓▓▓▓ camera ▓▓▓▓▓▓▓     ∿∿∿∿∿∿  ┃   28 pt tall
              ╰━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╯
               └─ 76 pt ─┘└────── notch 160 pt ──────┘└─ 76 pt ─┘
                                 312 pt in all

 expanded (a row of words under the notch)
              ╭━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╮
              ┃ ● 0:01     ▓▓▓▓▓▓▓ camera ▓▓▓▓▓▓▓     ∿∿∿∿∿∿  ┃
              ┃        Tap again to paste, Esc to cancel       ┃   + 28 pt
              ╰━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╯
```

- **Outline.** The top edge is flush with the top of the screen. The top
  corners curve outward (about 6 pt), like the notch's own corners, so the
  island looks cut from the same black. The bottom corners match the
  notch's radius (about 8–10 pt) when compact and grow to about 18 pt when
  expanded.
- **Height.** Compact is exactly the notch height. Expanded adds one 28 pt
  row (the current `captionHeight` 24 plus padding).
- **Width.** Compact is notch + 2 × shoulder. Expanded is the larger of
  that and the text width plus padding, capped at today's `maxWidth`
  (440 pt), with the text truncated after that.
- **Colour.** Always pure black (`#000`, opaque), in light and dark mode,
  so it merges with the hardware. Foreground colours come from the dark
  palette (white text, red voice, ink-blue line, amber warning). There is no
  shadow when compact. A soft shadow appears under the expanded row only.
- **Nothing is drawn in the notch's own rectangle.** The camera hides it.

### 4.2 Every state

| Pill phase | Left shoulder | Right shoulder | Row under the notch |
| --- | --- | --- | --- |
| `recording` | Red dot breathing with the voice, then the timer | Live ribbon, newest sound at the outer edge | The start hint, for up to 3 s |
| `working` | The dot crossfades to ink; the timer dims | The ribbon settles into the ink line; the glint runs along it | "Transcribing…"/"Polishing…" only after 1.5 s, as today |
| pasted | The dot grows into the check disc; the check is drawn on | The line fades | None. Then the island closes |
| `busy` (loading, warming, mic starting) | Spinner | Empty | The health message |
| `attention`, `error` | Amber warning | Empty | The message |
| `notice` | Ink dot | Empty | The message |
| System mode | Accent-coloured dot and timer, as today | Accent ribbon | "System" label in the row |

### 4.3 Motion

The island has its own small motion model, separate from the floating
pill's `PillMotion`:

- `spread`, 0 to 1: 0 is exactly the notch, 1 the full shoulders. It may
  overshoot a little on open.
- `drop`, 0 to 1: how far the row under the notch is out.
- `contentAlpha`.

| Moment | Motion |
| --- | --- |
| Open | `spread` 0 → 1 with today's gentle spring (`Ease.outBack`, 0.42 s). Content fades in from 25 %. Because it starts as the notch, it seems to come out of the camera. |
| Words arrive or go | `drop` springs between 0 and 1 over about 0.3 s. The width follows the text. |
| Paste | The check grows in the left shoulder with today's timings (`checkDelay`, `checkGrow`, `checkStroke`), then it closes after `confirmDuration`. |
| Close | Content fades out in the first third. `spread` and `drop` go back to 0 (`Ease.inCubic`). At 0 the island is the notch, so it disappears into the hardware. Then the window is ordered out. |
| Reduce Motion | The island appears and leaves at full size with a plain fade. The check appears in the dot without growing, as today. |

A new dictation that starts while the check is still showing opens afresh,
as the floating pill does today.

### 4.4 The window

- A second panel, `islandPanel`, built like `makePillPanel` (borderless,
  non-activating, ignores the mouse, joins all Spaces, full-screen
  auxiliary). It reuses the "moved to a fresh window" fix for Spaces
  (`moveToFreshPanel`, `:1703`).
- **Level `.mainMenu + 3` (27).** This is above the menu bar (24) and the
  status items (25), and below pop-up menus (101), so an open menu still
  covers the island. Today's `.statusBar` (25) would tie with the status
  items.
- **Fixed size, no resizing.** The window is as big as the largest island:
  `maxWidth` + margin wide, notch + row + margin tall. Its top edge is
  flush with `screen.frame.maxY`, centred on the notch. The island's shape
  animates inside it. Because the window ignores the mouse, the transparent
  part blocks nothing. This avoids the window-resize bugs of
  `fitToContent` in this path.
- As a guard, the panel subclass overrides `constrainFrameRect(_:to:)` to
  return the frame unchanged. The probe above shows AppKit leaves it alone
  today.

### 4.5 Which screen, and when to choose

- Keep today's screen choice (`NSScreen.main`). Use the island when that
  screen has a notch (`safeAreaInsets.top > 0` and both auxiliary areas
  exist). Otherwise use the floating pill.
- Choose on each `show()` that opens the pill from hidden. **Never switch
  during a session.** A session that started on the island ends on it, even
  if the screens change, so the pill does not jump mid-dictation.
- Recompute the notch on every open (it is cheap) and on
  `NSApplication.didChangeScreenParametersNotification`. If the notch screen
  disappears mid-session, move the island's window to the new main screen
  as a floating pill at the next open.

### 4.6 Code structure

Inside the one Swift file (it is compiled with `swiftc` in
`install-macos.sh`, `packaging/macos/build-app.sh` and the Homebrew
formula, so no packaging changes):

1. **`PillModel`.** Move the state machine out of `PillView` as it is:
   phases, levels, captions, `pendingCaption`, timings, `transition`,
   `presence`, `tick`, `reset`, `closeDelay`, `isAnimating`. This is a move,
   not a rewrite.
2. **Shared drawing.** The dot, check, ribbon with glint, timer, spinner,
   warning, notice dot and caption string become functions that take their
   position and the palette.
3. **`PillView`** keeps today's floating drawing and `PillMotion`, reading
   from the model. It looks exactly as it does now.
4. **`IslandView`** draws the island from the same model with its own
   motion (§4.3).
5. **`OverlayApp`** owns one model and both panels and routes `show`/`hide`
   to the one chosen in §4.5. The onboarding panel's still pill
   (`OnboardingController`, `:1397`) keeps using `PillView`.

### 4.7 The setting

- `overlay_style: "auto" | "pill"` in `Settings`, next to
  `overlay_backend` (`apps/daemon/src/settings.rs:180`). The default is
  `"auto"`. Validation rejects other values.
- `overlay_command()` (`:645`) passes it to the Swift overlay as
  `SUNOTO_OVERLAY_STYLE`. Linux and the mock overlay ignore it.
- Changing it needs only a daemon restart, like other overlay settings.

## 5. Steps

Each step ends with a check that must pass before the next one starts.

| Step | Work | Check |
| --- | --- | --- |
| 0 | **Render harness in the repo, and mockups.** Today the harness is ad hoc: cut the app launch off the Swift file, append a `main` that drives the view with synthetic times, and save PNGs. Make it repeatable: add a `// MARK: App launch` marker before the last lines, and add `tools/ui/render-overlay-frames.sh`, which cuts at the marker, appends `tools/ui/overlay_render_main.swift`, compiles to a temp dir, and writes frames. Render today's pill as the **baseline**. Then render island mockups of every state in §4.2 over a drawn menu bar and notch, on a light and a dark wallpaper, with both D3 layouts. | The owner picks D3 from the contact sheet and agrees the look. |
| 1 | **Refactor with no visible change.** Extract `PillModel` and the shared drawing (§4.6, items 1–3). | Frames are byte-identical to the step 0 baseline. A live, instrumented NDJSON run logs the same phases and frames. `make test` passes. |
| 2 | **Notch geometry and the empty island.** `NotchGeometry` from `NSScreen`. `islandPanel`, its level and fixed frame. The island outline with `spread` and `drop`, no content. | Harness checks for a notched screen, a screen without a notch, and an external display. On this Mac, a live run logs the panel frame flush with the top and centred on x 668–828. The owner watches an empty island open and close (a `make ui-demo`-style script). |
| 3 | **Content and motion** for every row of §4.2 and §4.3, including Reduce Motion and System mode. | New frames for every state. The owner checks on the real notch (§6). |
| 4 | **Setting and fallback.** `overlay_style` in settings, validation, env var and tests. Older config files without the key get `"auto"`. | `cargo test --workspace --offline` and `cargo clippy --workspace --offline --all-targets -- -D warnings` pass. With `"pill"`, the floating pill is back. |
| 5 | **Docs and release.** Update the pill bullets in `AGENTS.md`, `docs/desktop-configuration.md` (the setting), `docs/architecture.md` and `docs/how-sunoto-works.html`. Ship in the next version. | The release smoke test passes. An auto-updated install shows the island. |

Rough size: step 0 half a day, step 1 half a day to a day, step 2 half a
day, step 3 one to two days, steps 4–5 half a day. **About 3–4 days.**

## 6. The owner's live check (step 3)

Claude's terminal (Ghostty) now has Screen Recording, so Claude can
screenshot the real island (see §9). No screenshot shows the hardware notch,
though, so how the island blends into it is still checked by eye. Focus a
scratch text window first, as for any dictation test.

1. A short dictation (2–3 s): the island opens from the notch, the hint row
   shows and goes when you talk, the check shows, and the island closes into
   the notch.
2. A long dictation (over 20 s, so it is transcribed in pieces): the timer
   keeps going and nothing jumps.
3. A slow wait (more than 1.5 s): "Transcribing…" drops down under the
   notch.
4. Esc to cancel, and a paste into a password field (the error row).
5. Startup while the speech model loads: the spinner and "Loading…" row,
   then the island closes by itself.
6. A full-screen app (the menu bar is hidden and the band is black).
7. "Automatically hide and show the menu bar" turned on.
8. An external display as the main screen: the floating pill, as today.
9. Switching Spaces, then dictating: the island is on the current Space.
10. Light and dark mode: the island stays black.

## 7. Risks

| Risk | What we do |
| --- | --- |
| The shoulders cover the menus and status items beside the notch while you dictate. | The shoulders are narrow (about 76 pt) and only show during a dictation. Clicks pass through, because the window ignores the mouse. |
| Another notch app draws in the same place. | `overlay_style: "pill"`. Mention it in the docs. |
| Notch sizes differ between models and scaled display modes. | Always read the notch from `NSScreen`. Harness checks use several sizes. |
| A future macOS changes how windows over the menu bar are placed. | `constrainFrameRect` guard. Step 2's live check logs the real frame. |
| Text in the row is longer than 440 pt. | Truncate, as the pill does today. |

## 8. Not in this version, and what to keep room for

- **The Fix and Improve buttons** planned for after a paste
  (`docs/local-history-plan-2026-10-16.md` §6). On the island they belong in
  the row under the notch: "✓ Pasted   [Improve] [Fix]". That needs the
  island to take clicks while the buttons show. The fixed-size window must
  then stop ignoring the mouse **only over the island's shape**, or it
  would block clicks on the menu bar around it. Whichever plan lands second
  handles this. Shrinking the window to the island's frame while the
  buttons show is the simplest way.
- Hover and click on the island when idle (NotchNook-style). Sunoto's pill
  is passive, and staying that way is simpler.
- A fake island on screens without a notch (D2).
- The onboarding panel's still pill shown as an island.
- Linux.

## 9. What was built (2026-10-16)

The decisions in §3 were taken as recommended: `overlay_style: "auto"` by
default, the floating pill on screens without a notch, the dot and timer on
the left with the ribbon on the right, the hint kept in the row, and the
check in the left shoulder.

| Step | Result |
| --- | --- |
| 0 | `tools/ui/render-overlay-frames.sh OUT [GROUP...]` cuts the Swift file at `// MARK: App launch`, appends `tools/ui/overlay_render_main.swift`, and writes PNG frames plus a contact sheet per group. It replays the daemon's messages on a synthetic clock: quick and slow dictations, Esc, health, System mode, Reduce Motion, light and dark. Groups `pill-*` are today's pill; `island-*` draw the island over a mock menu bar and notch, including notches of 160×28, 185×32 and 210×38 pt. |
| 1 | `PillModel` (state), `PillArt` (shared drawing, positions passed in) and `PillView` (floating pill and its motion). All 75 floating-pill frames were byte-identical to the baseline rendered before the change. |
| 2 | `NotchGeometry` reads the notch from `NSScreen` (the side areas are measured inwards from the screen's edges, so their coordinate space does not matter). `IslandPanel` overrides `constrainFrameRect`; the window is level `.mainMenu + 3`, fixed size, flush with the top, centred on the notch. The harness group `geometry` checks a notched screen, no notch, an external display, a notched screen away from the origin, and both coordinate spaces. Live, the overlay logged `island window x 500.0–996.0, top 967.0; notch x 668.0–828.0, top 967.0`. |
| 3 | `IslandView` draws every row of §4.2 with the motion of §4.3: `spread` from the open/close transition, `drop` and the width as tweens that follow the row's words. Content is clipped out of the notch's rectangle. |
| 4 | `overlay_style` in `Settings` (default `"auto"`, validated, passed as `SUNOTO_OVERLAY_STYLE`), with tests for old config files and the environment. With `"pill"` the live run showed the floating pill as before. |
| 5 | `AGENTS.md`, `docs/desktop-configuration.md`, `docs/architecture.md`/`.html`, `docs/how-sunoto-works.html`. Release still to do. |

Live checks Claude ran on this Mac with `tools/ui/macos_overlay_demo.py`
(drives a real overlay with the daemon's NDJSON and screenshots the top of
the screen): hint row, live ribbon, settled line with glint, "Transcribing…"
row, check in the left shoulder, closed, loading row with spinner, ready
(closes), error row. A screen recording showed the open (shoulders spring
out, the row drops, content fades in) and the close (content fades, the
island pulls back into the notch). Screenshots fail or come back black while
the display is asleep.

**Design changed after the owner's review (2026-10-16).** The shoulders
layout (dot and timer left of the camera, voice right of it) looked
lopsided: the camera split the timer from the voice, and the check landed
at the far left. Four directions were mocked up in
`docs/notch-island-designs.html`; the owner chose **A, the hanging pill**:
the notch grows a pill under the camera with the dot, voice and timer in one
row, words in a second row, and the check in the middle under the camera.
Loading was redrawn to match (the ink dot and a glinting line, the words
below, no spinner). Every size change uses one spring (0.45 s; a little
overshoot on open, none on close) and parts only fade in place. D3 and D5
above are superseded.

Differences from the plan:

- **A new dictation during the check.** The island is already open with its
  shoulders, so it swaps the check back to the dot instead of collapsing and
  opening afresh (that would be a jump). Caught mid-close, it reopens from
  where it is, as the floating pill does.
- **After a paste the row folds away.** If "Transcribing…" was showing, the
  row goes as the check grows; the floating pill still says "Pasted".
- **D3's alternative layout** (ribbon and timer on the right) was not
  rendered. Only the recommended layout exists.
- **Not checked yet:** §6 items 6–9 (full screen, auto-hiding menu bar, an
  external display as the main screen, switching Spaces), and the island in a
  real dictation from the installed app.

