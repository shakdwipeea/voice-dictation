#!/usr/bin/env python3
"""Benchmark local GGUF models on voice-dictation disfluency cleanup.

Loads one model at a time (VRAM-safe), runs every example, scores with
exact + fuzzy matching, and emits per-model JSON plus an HTML report.
"""
from __future__ import annotations
import json, os, sys, time, re, statistics, argparse, html

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
MODEL_DIR = os.path.join(ROOT, "models")
DATASET = os.path.join(HERE, "dataset.jsonl")
OUT_DIR = os.path.join(HERE, "out")

# Constrained prompt: the model's ONLY job is to remove disfluency and
# preserve everything else. Refined after v1: v1 was too vague about
# restarts, so models kept abandoned prefixes. This version explicitly
# draws the line between a RESTART (speaker abandons the start and
# restates -> keep only the final version) vs. a genuine utterance that
# happens to contain 'sorry'/'actually'/'no' (keep it).
SYSTEM_PROMPT = (
    "You are a transcript cleaner for a voice-dictation system. "
    "Your ONLY job is to remove speech disfluencies. Output the cleaned "
    "transcript and nothing else: no labels, no quotes, no preamble, no "
    "explanation.\n\n"
    "REMOVE these when they are disfluencies:\n"
    "- Stutters and immediate word repeats: 'I I think' -> 'I think'.\n"
    "- Fillers / hesitation: 'um, uh, er, hmm, well, you know' when they "
    "add no meaning.\n"
    "- False starts and abandoned prefixes the speaker discards and then "
    "RESTATES: keep only the restatement. Example: 'I went to the store, "
    "no wait, I went to the park.' -> 'I went to the park.'\n\n"
    "PRESERVE these exactly:\n"
    "- All facts, names, numbers, emails, URLs, code, and acronyms.\n"
    "- The intended meaning and information content.\n"
    "- 'sorry', 'actually', 'no', 'wait' etc. when they ARE the message "
    "('I am sorry for the delay.' 'Actually, I agree.').\n"
    "- Punctuation intent; fix obvious spacing only.\n\n"
    "Do NOT add information, do NOT answer or respond to the text, do NOT "
    "translate. If the text is already clean, return it unchanged."
)

# Few-shot examples. Different surface forms from the test set but same
# transformation. LFM docs explicitly call out few-shot as effective for
# instruction-tuned LFM2.5 models; helps the small models most.
FEW_SHOT = [
    ("The the report is, no wait, the report is due today.",
     "The report is due today."),
    ("Um, so, my name is, I mean, my name is Svetlana.",
     "My name is Svetlana."),
    ("Call him at five, actually, call him at five thirty.",
     "Call him at five thirty."),
    ("I I am not sure, you know what, never mind.",
     "Never mind."),
]

MODELS = {
    "ministral-3-3b": {
        "file": "ministral-3-3b.gguf",
        "label": "Ministral-3-3B-Instruct-2512 (Q4_K_M)",
        "family": "Mistral",
        "size_mib": 2048,
    },
    "phi-4-mini": {
        "file": "phi-4-mini.gguf",
        "label": "Phi-4-mini-instruct (Q4_K_M)",
        "family": "Microsoft",
        "size_mib": 2376,
    },
    "phi-4-mini-q5": {
        "file": "phi-4-mini-q5.gguf",
        "label": "Phi-4-mini-instruct (Q5_K_M)",
        "family": "Microsoft",
        "size_mib": 2714,
    },
    "phi-4-mini-q6": {
        "file": "phi-4-mini-q6.gguf",
        "label": "Phi-4-mini-instruct (Q6_K)",
        "family": "Microsoft",
        "size_mib": 3011,
    },
    "phi-4-mini-q8": {
        "file": "phi-4-mini-q8.gguf",
        "label": "Phi-4-mini-instruct (Q8_0)",
        "family": "Microsoft",
        "size_mib": 3891,
    },
    "qwen3-4b-2507": {
        "file": "qwen3-4b-2507.gguf",
        "label": "Qwen3-4B-Instruct-2507 (Q4_K_M)",
        "family": "Qwen",
        "size_mib": 2386,
    },
    "qwen3.5-4b": {
        "file": "qwen3.5-4b.gguf",
        "label": "Qwen3.5-4B (Q4_K_M)",
        "family": "Qwen",
        "size_mib": 2611,
    },
    "lfm2.5-230m": {
        "file": "lfm2.5-230m.gguf",
        "label": "LiquidAI LFM2.5-230M (Q4_K_M)",
        "family": "Liquid",
        "size_mib": 150,
    },
    "lfm2.5-1.2b": {
        "file": "lfm2.5-1.2b.gguf",
        "label": "LiquidAI LFM2.5-1.2B-Instruct (Q4_K_M)",
        "family": "Liquid",
        "size_mib": 697,
    },
    "lfm2.5-8b-a1b": {
        "file": "lfm2.5-8b-a1b.gguf",
        "label": "LiquidAI LFM2.5-8B-A1B (Q4_K_M, MoE, thinking)",
        "family": "Liquid",
        "size_mib": 4915,
        "thinking": True,
    },
}


def load_dataset(path: str) -> list[dict]:
    rows = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def norm(s: str) -> str:
    """Normalize for fuzzy comparison: lowercase, collapse whitespace/punct."""
    s = s.lower().strip()
    # strip surrounding quotes the models sometimes add
    s = s.strip('"`\'')
    s = re.sub(r"\s+", " ", s)
    s = re.sub(r"[.,!?;:]+", " ", s)
    s = re.sub(r"\s+", " ", s).strip()
    return s


def tokenize(s: str) -> list[str]:
    return [t for t in re.split(r"\s+", s) if t]


def lev(a: list[str], b: list[str]) -> int:
    """Word-level Levenshtein distance."""
    if a == b:
        return 0
    if not a:
        return len(b)
    if not b:
        return len(a)
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def score(output: str, expected: str) -> dict:
    n_out, n_exp = norm(output), norm(expected)
    exact = n_out == n_exp
    to_out, to_exp = tokenize(n_out), tokenize(n_exp)
    # word error rate (0 = perfect, 1 = totally wrong)
    d = lev(to_out, to_exp)
    denom = max(len(to_exp), 1)
    wer = min(d / denom, 1.0)
    # token similarity 0..1
    sim = 1.0 - wer
    return {
        "exact": exact,
        "wer": round(wer, 4),
        "similarity": round(sim, 4),
        "edits": d,
    }


def is_refusal_or_chatty(output: str) -> bool:
    """Did the model chat/answer instead of returning cleaned text?
    Conservative: only flags unambiguous chatty preambles, NOT transcripts
    that legitimately start with 'I'/'The' (the refined prompt produces many)."""
    o = output.strip().lower()
    if not o:
        return True
    bad_starts = (
        "here is the cleaned", "here's the cleaned", "sure,", "certainly,",
        "of course,", "the cleaned transcript is", "cleaned text:",
        "output:", "result:", "after removing", "the disfluen",
        "as an ai", "i cannot", "i'm sorry",
    )
    return o.startswith(bad_starts)


THINK_OPEN  = "╖"   # the literal token our 8B thinking model emits
THINK_CLOSE = "╖╖"  # confirmed empirically


def clean_output(raw: str, system_prompt: str) -> str:
    """Best-effort strip of model chatter, thinking tags, surrounding quotes.
    The LFM2.5-8B-A1B thinking model emits a thinking block of plain ASCII
    text bookended by two literal marker chars; the answer is on the line
    AFTER the closing marker. Take the tail past the LAST closing marker."""
    s = raw.strip()
    # Take the text after the last occurrence of the thinking-close marker
    # followed by a newline. This is robust to the model emitting multiple
    # thinking blocks or putting a one-line reasoning dump before the answer.
    parts = s.split(THINK_CLOSE)
    if len(parts) > 1:
        # everything after the final closing marker is the answer
        s = parts[-1].lstrip()
    s = re.sub(r"<\|.*?\|>", "", s).strip()
    lines = [ln.strip() for ln in s.splitlines() if ln.strip()]
    if lines:
        def is_preamble(ln: str) -> bool:
            ll = ln.lower()
            return ll.startswith(("here is the", "here's the", "sure",
                                  "the cleaned", "cleaned text", "output:",
                                  "result:", "after removing", "the disfluen",
                                  "of course", "as an ai", "i cannot"))
        cand = [ln for ln in lines if not is_preamble(ln)] or lines
        s = cand[-1] if len(cand) > 1 else cand[0]
    s = s.strip("`'\"")
    s = re.sub(r"^[#*>\-]+\s*", "", s).strip()
    s = s.strip("*`'\"")
    return s.strip()


def build_messages(model_key: str, user_text: str) -> list[dict]:
    """Chat messages: system + few-shot examples + the live transcript.
    Works for Mistral/Microsoft/Qwen/Liquid (all ChatML-ish via llama.cpp)."""
    msgs: list[dict] = [{"role": "system", "content": SYSTEM_PROMPT}]
    for inp, out in FEW_SHOT:
        msgs.append({"role": "user", "content": f"Clean this transcript:\n{inp}"})
        msgs.append({"role": "assistant", "content": out})
    msgs.append({"role": "user", "content": f"Clean this transcript:\n{user_text}"})
    return msgs


# Per-model inference config. LFM settings come from the official Liquid
# prompting guide (temp=0.1, top_k=50, repeat_penalty=1.05). We apply the
# same knobs to all chat models here to keep the comparison fair; LFM in
# v1 ran at greedy temperature 0 with no repeat penalty and that hurt it.
INFER = {"temperature": 0.1, "top_k": 50, "top_p": 0.95, "repeat_penalty": 1.05}


def run_model(model_key: str, rows: list[dict], gpu_layers: int,
              n_ctx: int, max_tokens: int) -> dict:
    from llama_cpp import Llama
    info = MODELS[model_key]
    path = os.path.join(MODEL_DIR, info["file"])
    if not os.path.exists(path):
        return {"model": model_key, "label": info["label"], "error": f"missing file {path}"}
    # Thinking models (LFM2.5-8B-A1B) emit a reasoning trace before the
    # answer, so they need a much larger token budget, else the answer
    # never emits and we score empty output.
    if info.get("thinking"):
        max_tokens = max(max_tokens, 800)
        n_ctx = max(n_ctx, 4096)
    print(f"\n=== loading {info['label']} ({path}) ===", flush=True)
    t0 = time.time()
    llm = Llama(
        model_path=path,
        n_gpu_layers=gpu_layers,
        n_ctx=n_ctx,
        verbose=False,
        logits_all=False,
        n_threads=8,
    )
    load_ms = round((time.time() - t0) * 1000)
    print(f"  loaded in {load_ms} ms", flush=True)

    results = []
    exact_n = 0
    sims, wers, lat_ms = [], [], []
    chat_fails = 0
    for row in rows:
        msgs = build_messages(model_key, row["input"])
        t1 = time.time()
        try:
            r = llm.create_chat_completion(messages=msgs, max_tokens=max_tokens,
                                          temperature=INFER["temperature"],
                                          top_k=INFER["top_k"],
                                          top_p=INFER["top_p"],
                                          repeat_penalty=INFER["repeat_penalty"])
            raw = r["choices"][0]["message"]["content"] or ""
        except Exception as e:
            raw = ""
            print(f"    [{row['id']}] ERROR {e}", flush=True)
        ms = round((time.time() - t1) * 1000)
        out = clean_output(raw, SYSTEM_PROMPT)
        sc = score(out, row["expected"])
        if is_refusal_or_chatty(out):
            chat_fails += 1
        if sc["exact"]:
            exact_n += 1
        sims.append(sc["similarity"])
        wers.append(sc["wer"])
        lat_ms.append(ms)
        results.append({
            "id": row["id"],
            "category": row["category"],
            "input": row["input"],
            "expected": row["expected"],
            "output": out,
            "raw_output": raw.strip(),
            "exact": sc["exact"],
            "similarity": sc["similarity"],
            "wer": sc["wer"],
            "latency_ms": ms,
        })
        marker = "OK " if sc["exact"] else ("~ " if sc["similarity"] >= 0.9 else "X  ")
        print(f"    {marker}{row['id']:14s} sim={sc['similarity']:.2f} {ms:4d}ms", flush=True)

    # free model before next
    del llm

    return {
        "model": model_key,
        "label": info["label"],
        "family": info["family"],
        "size_mib": info["size_mib"],
        "load_ms": load_ms,
        "n": len(rows),
        "exact_matches": exact_n,
        "exact_pct": round(100 * exact_n / len(rows), 1),
        "mean_similarity": round(statistics.mean(sims), 4),
        "mean_wer": round(statistics.mean(wers), 4),
        "mean_latency_ms": round(statistics.mean(lat_ms)),
        "p50_latency_ms": round(statistics.median(lat_ms)),
        "p95_latency_ms": round(sorted(lat_ms)[int(0.95 * (len(lat_ms) - 1))]) if len(lat_ms) > 1 else round(lat_ms[0]),
        "chat_fails": chat_fails,
        "results": results,
    }


def build_html(all_models: list[dict], rows: list[dict], out_html: str) -> None:
    """Render a user-friendly HTML report: summary table + per-example grid."""
    # index results by id for cross-model columns
    by_id = {row["id"]: row for row in rows}
    mkeys = [m["model"] for m in all_models]
    # sort models by mean_similarity desc
    ranked = sorted(all_models, key=lambda m: m.get("mean_similarity", 0), reverse=True)

    def esc(s: str) -> str:
        return html.escape(str(s))

    parts = []
    parts.append("<!DOCTYPE html><html><head><meta charset='utf-8'>")
    parts.append("<title>LLM Disfluency Cleanup Benchmark</title>")
    parts.append("<style>")
    parts.append("""
    body { font-family: -apple-system, Segoe UI, Roboto, Helvetica, Arial, sans-serif;
           margin: 0; background: #0d1117; color: #c9d1d9; }
    h1 { background: #161b22; margin: 0; padding: 18px 24px; border-bottom: 1px solid #30363d;
         font-size: 20px; }
    h1 small { color: #8b949e; font-weight: normal; margin-left: 12px; }
    .wrap { max-width: 1400px; margin: 0 auto; padding: 20px; }
    h2 { margin-top: 28px; font-size: 17px; color: #58a6ff; border-bottom: 1px solid #21262d; padding-bottom: 6px; }
    .summary { overflow-x: auto; }
    table { border-collapse: collapse; width: 100%; font-size: 13px; background: #161b22; }
    th, td { border: 1px solid #30363d; padding: 7px 10px; text-align: left; vertical-align: top; }
    th { background: #21262d; position: sticky; top: 0; cursor: pointer; }
    tbody tr:nth-child(even) { background: #11161d; }
    td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; }
    .best { color: #3fb950; font-weight: bold; }
    .ok { color: #3fb950; }
    .warn { color: #d29922; }
    .bad { color: #f85149; }
    .pill { display:inline-block; padding:1px 7px; border-radius:10px; font-size:11px; background:#21262d; color:#8b949e; }
    .miss .out { background: #2a1213; }
    .grid td.needed { width: 28%; }
    .lozenge { display:inline-block; width:10px; height:10px; border-radius:2px; vertical-align:middle; margin-right:3px; }
    .legend { font-size:12px; color:#8b949e; margin: 6px 0 10px; }
    details { margin-top: 4px; }
    summary { cursor: pointer; color:#8b949e; font-size:12px; }
    .raw { white-space: pre-wrap; font-family: ui-monospace, monospace; font-size:11px; color:#8b949e; }
    .filters button { background:#21262d; color:#c9d1d9; border:1px solid #30363d; padding:4px 10px; border-radius:14px; cursor:pointer; font-size:12px; margin: 2px; }
    .filters button.active { background:#1f6feb; color:#fff; border-color:#1f6feb; }
    .hide { display:none !important; }
    """)
    parts.append("</style></head><body>")
    parts.append("<h1>LLM Disfluency-Cleanup Benchmark <small>local GGUF models on a 60-example voice-dictation cleanup set</small></h1>")
    parts.append("<div class='wrap'>")

    # ---- Summary table ----
    parts.append("<h2>Summary</h2>")
    parts.append("<div class='summary'><table id='summary'><thead><tr>")
    parts.append("<th>Rank</th><th>Model</th><th>Family</th><th class='num'>Size</th>")
    parts.append("<th class='num'>Exact %</th><th class='num'>Mean sim</th>")
    parts.append("<th class='num'>Mean WER</th><th class='num'>Chat fails</th>")
    parts.append("<th class='num'>Load</th><th class='num'>p50 lat</th><th class='num'>p95 lat</th>")
    parts.append("</tr></thead><tbody>")
    best_sim = max((m.get("mean_similarity", 0) for m in all_models), default=0)
    best_exact = max((m.get("exact_pct", 0) for m in all_models), default=0)
    best_p50 = min((m.get("p50_latency_ms", 9e9) for m in all_models), default=9e9)
    for i, m in enumerate(ranked, 1):
        parts.append("<tr>")
        parts.append(f"<td>{i}</td><td>{esc(m['label'])}</td><td>{esc(m.get('family',''))}</td>")
        parts.append(f"<td class='num'>{m.get('size_mib','?')} MiB</td>")
        se = "best" if m.get("exact_pct") == best_exact else ""
        ss = "best" if m.get("mean_similarity") == best_sim else ""
        sl = "best" if m.get("p50_latency_ms") == best_p50 else ""
        parts.append(f"<td class='num {se}'>{m.get('exact_pct','?')}</td>")
        parts.append(f"<td class='num {ss}'>{m.get('mean_similarity','?')}</td>")
        parts.append(f"<td class='num'>{m.get('mean_wer','?')}</td>")
        cf = m.get("chat_fails", 0)
        cfc = "ok" if cf == 0 else ("warn" if cf <= 3 else "bad")
        parts.append(f"<td class='num {cfc}'>{cf}</td>")
        parts.append(f"<td class='num'>{m.get('load_ms','?')}</td>")
        parts.append(f"<td class='num {sl}'>{m.get('p50_latency_ms','?')}</td>")
        parts.append(f"<td class='num'>{m.get('p95_latency_ms','?')}</td>")
        parts.append("</tr>")
    parts.append("</tbody></table></div>")
    parts.append("<div class='legend'>Legend: <span class='lozenge' style='background:#3fb950'></span>exact "
                 "<span class='lozenge' style='background:#d29922'></span>close (sim≥0.9) "
                 "<span class='lozenge' style='background:#f85149'></span>off. "
                 "&#9889; = chat/refusal failure. Green = best in column.</div>")
    parts.append("<div class='legend'>Scoring: exact = normalized char/word match; "
                 "similarity = 1 − word-error-rate vs expected. WER lower is better.</div>")

    # ---- Category breakdown ----
    cats = {}
    for m in all_models:
        for r in m.get("results", []):
            cats.setdefault(r["category"], {k: [] for k in mkeys})
            cats[r["category"]][m["model"]].append(r["similarity"])
    parts.append("<h2>By category (mean similarity)</h2>")
    parts.append("<div class='summary'><table><thead><tr><th>Category</th>")
    for m in ranked:
        parts.append(f"<th class='num'>{esc(m['model'])}</th>")
    parts.append("</tr></thead><tbody>")
    for cat in sorted(cats):
        parts.append(f"<tr><td>{esc(cat)}</td>")
        for m in ranked:
            vals = cats[cat].get(m["model"], [])
            v = statistics.mean(vals) if vals else 0
            cls = "best" if v == max(statistics.mean(cats[cat].get(kk, [0]) or [0]) for kk in mkeys) else ""
            parts.append(f"<td class='num {cls}'>{v:.2f}</td>")
        parts.append("</tr>")
    parts.append("</tbody></table></div>")

    # ---- Per-example grid ----
    parts.append("<h2>Per-example results</h2>")
    cats_list = sorted(set(r["category"] for r in rows))
    parts.append("<div class='filters' id='filters'>")
    parts.append("<button class='active' data-cat='all'>all</button>")
    for c in cats_list:
        parts.append(f"<button data-cat='{esc(c)}'>{esc(c)}</button>")
    parts.append("</div>")
    parts.append("<table class='grid'><thead><tr><th>ID ✱ type</th><th>Input</th>"
                 "<th>Expected</th>")
    for m in ranked:
        parts.append(f"<th>{esc(m['model'])}</th>")
    parts.append("</tr></thead><tbody>")
    for row in rows:
        parts.append(f"<tr data-cat='{esc(row['category'])}'>")
        parts.append(f"<td><b>{esc(row['id'])}</b><br><span class='pill'>{esc(row['category'])}</span></td>")
        parts.append(f"<td class='needed'>{esc(row['input'])}</td>")
        parts.append(f"<td class='needed'><b>{esc(row['expected'])}</b></td>")
        for m in ranked:
            r = next((x for x in m.get("results", []) if x["id"] == row["id"]), None)
            if not r:
                parts.append("<td>—</td>")
                continue
            cls = "ok" if r["exact"] else ("warn" if r["similarity"] >= 0.9 else "bad")
            rowcls = "" if (r["exact"] or r["similarity"] >= 0.9) else "miss"
            lat = r["latency_ms"]
            bits = [f"<div class='out {cls}'>{esc(r['output'] or '(empty)')}</div>"]
            bits.append(f"<div class='legend' style='margin:2px 0'>sim {r['similarity']:.2f} · {lat}ms"
                        + (" &#9889;" if not r['output'] else "") + "</div>")
            if r.get("raw_output") and r["raw_output"] != (r["output"] or ""):
                bits.append("<details><summary>raw</summary><div class='raw'>"
                            + esc(r["raw_output"]) + "</div></details>")
            parts.append(f"<td class='{rowcls}'>{''.join(bits)}</td>")
            del rowcls
        parts.append("</tr>")
    parts.append("</tbody></table>")

    parts.append("""
    <script>
    const btns = document.querySelectorAll('#filters button');
    btns.forEach(b => b.onclick = () => {
      btns.forEach(x => x.classList.remove('active'));
      b.classList.add('active');
      const cat = b.dataset.cat;
      document.querySelectorAll('table.grid tbody tr').forEach(tr => {
        tr.classList.toggle('hide', !(cat === 'all' || tr.dataset.cat === cat));
      });
    });
    document.querySelectorAll('th').forEach(th => {
      const table = th.closest('table');
      if (!table || table.classList.contains('grid')) return;
      th.onclick = () => {
        const idx = Array.from(th.parentNode.children).indexOf(th);
        const tbody = table.querySelector('tbody');
        const rows = Array.from(tbody.querySelectorAll('tr'));
        const asc = th.dataset.asc === '1' ? 0 : 1;
        th.dataset.asc = asc;
        rows.sort((a, b) => {
          let av = a.children[idx].textContent.replace(/[^0-9.\\-]/g,'');
          let bv = b.children[idx].textContent.replace(/[^0-9.\\-]/g,'');
          if (av === '' && bv === '') { return 0; }
          if (av === '') return 1;
          if (bv === '') return -1;
          return asc ? (parseFloat(av)-parseFloat(bv)) : (parseFloat(bv)-parseFloat(av));
        });
        rows.forEach(r => tbody.appendChild(r));
      };
    });
    </script>
    """)
    parts.append("</div></body></html>")
    with open(out_html, "w") as f:
        f.write("".join(parts))
    print(f"  HTML report -> {out_html}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="*", default=list(MODELS),
                    help="subset of model keys to run")
    ap.add_argument("--gpu-layers", type=int, default=-1)
    ap.add_argument("--ctx", type=int, default=2048)
    ap.add_argument("--max-tokens", type=int, default=128)
    ap.add_argument("--out-dir", default=OUT_DIR)
    args = ap.parse_args()

    os.makedirs(args.out_dir, exist_ok=True)
    rows = load_dataset(DATASET)
    print(f"loaded {len(rows)} examples")

    all_models = []
    for key in args.models:
        if key not in MODELS:
            print(f"  skipping unknown model {key}")
            continue
        res = run_model(key, rows, args.gpu_layers, args.ctx, args.max_tokens)
        all_models.append(res)
        # write per-model json as we go
        with open(os.path.join(args.out_dir, f"{key}.json"), "w") as f:
            json.dump(res, f, indent=2)
        print(f"  -> {key}: exact {res.get('exact_pct')}%, sim {res.get('mean_similarity')}, "
              f"p50 {res.get('p50_latency_ms')}ms", flush=True)

    # combined json + html
    with open(os.path.join(args.out_dir, "all.json"), "w") as f:
        json.dump(all_models, f, indent=2)
    build_html(all_models, rows, os.path.join(args.out_dir, "report.html"))
    print("\nDONE")


if __name__ == "__main__":
    main()
