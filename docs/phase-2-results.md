# Phase 2 Results: Deterministic Polish Pipeline

**Date:** June 12, 2026 (updated later the same day: active-application style
detection wired; zero-edit-rate evaluation harness, scripted corpus,
punctuation restoration, and corpus recording/transcription tools added)
**Status:** Deterministic cleanup pipeline implemented, unit-tested, and wired
into the daemon and the latency bench, including per-application style
selection from the focused window's WM_CLASS. The zero-edit-rate harness runs
today against a scripted text corpus (97% polished vs 19% raw); the recorded
corpus needs a human to speak it (`make phase2-record`), after which the same
harness produces the exit-gate measurement. The optional local-LLM polish
mode remains open.

## Scope Delivered

`crates/sunoto-polish` implements the staged pipeline from plan section 7,
stages 1–6, as pure deterministic string processing (no model, no allocation
churn, well under the 20 ms budget — the whole pipeline is microseconds on
normal dictation lengths):

1. **Normalize** — whitespace collapse, punctuation spacing, duplicate-comma
   cleanup; numbers ("3.14", "1,000") and ellipses are preserved.
2. **Resolve corrections** —
   - *Swap markers* `actually`, `i mean`, `no wait`, `wait no`: the pattern
     `L, <marker> R` replaces L with R ("Tuesday, actually Wednesday." →
     "Wednesday."). Conservative gates keep false positives out: a comma must
     precede the marker, the replacement clause is capped at four words, and
     L and R must be shape-compatible word-for-word (case/digit classes), so
     "meet on Tuesday, actually Wednesday works better for me too." is left
     alone.
   - *Restart markers* `scratch that`, `strike that`: cancel the sentence
     being corrected ("Send it to Bob. Scratch that, send it to Alice." →
     "Send it to Alice."), recapitalizing the surviving sentence start.
3. **Remove fillers** — configurable list (default um/uh/uhm/umm/er/erm/hmm/
   mhm, multi-word entries supported), whole-word only ("umbrella" is safe),
   with parenthetical commas removed ("I, uh, think so." → "I think so.") and
   sentence-start capitalization repaired ("Um, send it." → "Send it.").
4. **Personal dictionary** — ordered case-insensitive whole-phrase
   replacements ("git hub" → "GitHub").
5. **Snippets** — whole-utterance triggers expand verbatim (multi-line
   expansions are preserved and skip further normalization).
6. **Style profiles** — `default` (no change), `terminal` (strip the dictated
   trailing period), `prose` (capitalize sentence starts and add a missing
   final period only when the utterance ends in plain text). Nemotron's
   existing punctuation is always preserved; the deterministic path does not
   invent question marks, internal commas, or sentence boundaries. Prose
   is the base profile; terminal rules override it for common terminals. The profile is
   selected per dictation from the focused window: the UI thread reads the
   window's WM_CLASS at shortcut release (climbing to ancestors, since input
   focus usually sits on a class-less child widget) and the daemon resolves
   it through configurable `app_styles` rules — case-insensitive substring
   against the WM_CLASS instance/class names, first match wins, base `style`
   otherwise. Default rules map common terminals (gnome-terminal, xterm,
   konsole, alacritty, kitty, tilix, terminator, urxvt) to `terminal`. The
   lookup is validated live by `sunoto-daemon selftest` (probe window with a
   class-less child) and `sunoto-daemon check` prints the class of the
   currently focused window.

Every stage records a before/after trace when it changes the text; the daemon
logs these per session, which provides the plan's raw-versus-polished diff
logging for local evaluation.

## Integration

- The daemon applies `polish` to every final transcript when
  `polish_enabled` is true (the default), followed by the injection-safety
  sanitizer (control characters dropped; Enter/Tab neutralized unless
  allowed). Setting `polish_enabled: false` gives raw transcription —
  the raw/deterministic mode choice required by the plan.
- All pipeline configuration lives in the settings file under `polish`
  (fillers, dictionary, snippets, style, stage toggles) and round-trips
  through serde; missing fields fall back to defaults so old settings files
  keep working.
- The latency bench runs the same pipeline, so the published latency numbers
  include cleanup cost.

## Zero-Edit-Rate Evaluation Harness

`sunoto-daemon eval` measures the pipeline's zero-edit rate over a corpus
manifest: raw ASR transcripts paired with the gold final text a user would
accept without edits. It always runs the default pipeline configuration plus
the corpus's own dictionary/snippets, so results are machine-independent. The
report includes per-tag rates, **regressions** (cases where raw already
matched and polish broke it) and **digit violations** (polished output whose
digit content differs from the gold text); either fails the run.

The corpus workflow has three steps, only the first needing a human:

1. `make phase2-record` — speak each case's `raw` text naturally
   (Enter-to-start/Enter-to-stop push-to-talk simulation, monitor-rejecting
   source selection, RMS validation, accept/retry/skip per case).
2. `make phase2-transcribe` — stream each recording through the real
   Nemotron sidecar exactly as the daemon would, filling the manifest's
   `raw` fields.
3. `make phase2-eval-recorded` — the exit-gate measurement.

### Scripted baseline (measured today)

`make phase2-eval` runs the 32-case scripted text corpus
(`tests/corpus/phase2-text-cases.json`: fillers, swap/restart corrections,
punctuation restoration, negatives, names, numbers, code terms, dictionary,
snippets):

| Metric | Result |
| --- | --- |
| Raw zero-edit rate | 6/32 (19%) |
| Polished zero-edit rate | 31/32 (97%) |
| Regressions | 0 |
| Digit violations | 0 |

The one failing case is tagged `known-gap`: a filler between the comma and
the correction marker ("Friday, uh, actually Monday") blocks the swap,
because fillers are removed after corrections. Reordering the stages has
false-positive risks of its own; this is exactly the kind of gate tuning the
recorded corpus should drive. Report: `build/phase2/eval-scripted.json`.

## Verification

- 21 unit tests in `crates/sunoto-polish`, including the plan's canonical
  example, every stage in isolation, conservative-gate counterexamples,
  idempotence over varied inputs, multi-line snippet expansion, unicode/fuzz
  no-panic coverage, style-rule resolution, and serde round-trips.
- 3 `sunoto-daemon` eval-module tests (match counting, regression and
  digit-violation detection, manifest parsing defaults).
- `tests/phase2/` — transcriber end-to-end against the mock sidecar, WAV
  format validation, backend rejection.
- `make phase2-test` runs the crate tests plus the Python phase 2 suite;
  `sunoto-daemon selftest` covers the live window-class lookup.

## Remaining Phase 2 Work

- Record the evaluation corpus (needs a human voice: accents, fillers,
  corrections, names, code terms, background noise) via `make phase2-record`,
  then `make phase2-transcribe` and `make phase2-eval-recorded` for the
  exit-gate measurement; tune the correction gates from that data (starting
  with the documented `known-gap` case).
- Optional local text-LLM polish behind the same provider interface, with
  the plan's safety validation (never blocking first insertion).
