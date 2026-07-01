# LLM Polish Two-Step Benchmark Results - 2026-06-29

## Summary

The two-step decision architecture works functionally, but it is not ready to
replace the current one-pass minimal mode on the synthetic seed set.

Best measured two-step variant: internal `OK`/`EDIT` decision labels normalized
back to `UNCHANGED`/`EDIT` in daemon diagnostics.

It improves quality and completion-token count versus one-pass, but misses the
latency acceptance bar:

- Final exact improves from `84.4%` to `87.5%`.
- Mean WER improves from `6.83%` to `3.09%`.
- Completion-token p50 improves from `3` to `1`.
- LLM p50 regresses from `324ms` to `496ms`.
- LLM p95 regresses from `613ms` to `1136ms`.

Conclusion: keep `SUNOTO_LLM_POLISH_MODE=two_step` experimental/flagged. Do
not make it the default yet.

## Environment

- Machine: Apple M1 Pro.
- ASR backend: `parakeet_mlx_streaming`.
- LLM backend: llama.cpp/Metal GGUF
  `google_gemma-4-E2B-it-Q4_K_M.gguf`.
- Dataset: `llm-polish-bench/synthetic-minimal-v1.jsonl`.
- Benchmark path:
  `benchmark -> daemon control socket -> deterministic polish -> warm LLM sidecar`.

## Artifacts

| Mode | Artifact |
| --- | --- |
| Baseline one-pass minimal | `llm-polish-bench/out/daemon-architecture/one-pass-current.json` |
| Initial two-step (`UNCHANGED`/`EDIT`) | `llm-polish-bench/out/daemon-architecture/two-step-current.json` |
| Two-step with one-token `OK` label | `llm-polish-bench/out/daemon-architecture/two-step-ok-label-current.json` |
| Trimmed classifier prompt, rejected | `llm-polish-bench/out/daemon-architecture/two-step-trimmed-current.json` |

## Results

| Metric | One-pass minimal | Two-step initial | Two-step `OK` label | Trimmed prompt |
| --- | ---: | ---: | ---: | ---: |
| Final exact | `84.4%` | `81.2%` | `87.5%` | `90.6%` |
| Mean WER | `6.83%` | `6.50%` | `3.09%` | `4.94%` |
| Aggregate edit/word ratio | `6.96%` | `7.33%` | `4.40%` | `5.49%` |
| Contract / decision accuracy | `84.4%` | `84.4%` | `93.8%` | `81.2%` |
| Clean/no-op precision | n/a | `85.7%` | `90.9%` | `88.9%` |
| Edit recall | n/a | `75.0%` | `83.3%` | `83.3%` |
| LLM p50 | `324ms` | `470ms` | `496ms` | `519ms` |
| LLM p95 | `613ms` | `990ms` | `1136ms` | `1144ms` |
| LLM max | `691ms` | `1420ms` | `1446ms` | `1425ms` |
| Completion tokens p50 / p95 | `3 / 20` | `3 / 15` | `1 / 9` | `1 / 13` |
| Rewrite calls | `0` | `11` | `10` | `14` |
| Validation rejections | `0` | `0` | `0` | `0` |

## Observations

The original decision label `UNCHANGED` was not cheap for this model. It
decoded as three completion tokens. Switching the internal no-op label to `OK`
reduced decision completion-token p50 to `1` while preserving external daemon
semantics by normalizing `OK` to `decision_label="UNCHANGED"`.

Clean/no-op cases did get cheaper in the best two-step run. Expected no-op
cases had an approximate p50 around `194ms`, which is near the target shape.
The overall dataset p50 still regressed because 12 of 32 cases are edit cases,
and edit cases pay both the decision call and a rewrite call.

The rewrite path is the core latency problem. Rewrite calls commonly add about
`500ms`, and long live-ASR-like repairs can approach `900ms` for rewrite alone.
That pushes edit-case total latency near or above one second.

There is also a first-request outlier after warmup. The first benchmark request
in the two-step runs landed around `1.1-1.4s` despite the daemon's post-ASR LLM
warmup. This did not dominate the median, but it is a visible residual warmup
or cache-shape issue.

The trimmed classifier prompt was rejected. It produced higher final exact on
this seed set, but it over-routed clean text like `Actually, I agree with you.`
and `No, that is not what I said.` to rewrite. That increased rewrite calls to
`14` and worsened contract accuracy to `81.2%`.

## Resolution Attempted

Implemented and benchmarked a one-token internal decision label:

```text
OK -> normalized to UNCHANGED
EDIT -> rewrite
malformed -> rewrite
```

Also added focused decision examples for false starts and correction cues, and
kept rewrite validation fallback behavior unchanged.

This resolved the biggest mechanical issue, unnecessary three-token no-op
labels, but did not resolve total latency on the mixed clean/edit seed set.

## Remaining Issue

The two-step architecture only wins when most inputs are truly clean/no-op.
On this seed set, enough cases need rewrite that the median falls into the
slower two-call path. The current GGUF model is not fast enough for a decision
plus rewrite flow to beat the one-pass prompt at this edit mix.

## Recommendation

Keep the current one-pass minimal mode as the default.

Keep two-step behind `SUNOTO_LLM_POLISH_MODE=two_step` for further trials. The
best measured variant is useful for clean-heavy real dictation experiments, but
it does not meet the acceptance criteria from the plan:

- LLM p50 is not below `250ms`.
- LLM p95 is not below the one-pass p95.
- Clean/no-op precision is below the requested `95%`.

Next attempts should focus on one of:

1. A much smaller decision model or grammar/logit-biased classifier.
2. A shorter rewrite-only prompt with measured quality guardrails.
3. A real-dictation clean/edit mix benchmark; two-step may still be useful if
   real traffic is overwhelmingly clean.
4. A deterministic safety router after the required LLM decision call for
   obvious repeated spoken-digit prefixes and ASR word-order repairs.
