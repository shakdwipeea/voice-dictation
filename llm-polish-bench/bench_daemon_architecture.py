#!/usr/bin/env python3
"""Benchmark LLM polish through the running Sunoto daemon.

This exercises the realistic runtime path for text polish:

  benchmark -> daemon control socket -> deterministic polish -> warm LLM sidecar

It intentionally does not instantiate llama.cpp itself. The ASR sidecar remains
resident in the daemon process tree, the LLM prompt cache is the daemon's live
cache, and timings come from the same LLM client used after real ASR finals.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import socket
import statistics
import tempfile
import time
from datetime import datetime
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
DEFAULT_DATASET = HERE / "synthetic-minimal-v1.jsonl"
DEFAULT_OUT_DIR = HERE / "out" / "daemon-architecture"

_bench_spec = importlib.util.spec_from_file_location("bench_prompts", HERE / "bench.py")
bench = importlib.util.module_from_spec(_bench_spec)
assert _bench_spec.loader is not None
_bench_spec.loader.exec_module(bench)


def control_socket_path() -> Path:
    if os.environ.get("SUNOTO_CONTROL_SOCKET"):
        return Path(os.environ["SUNOTO_CONTROL_SOCKET"])
    if os.environ.get("XDG_RUNTIME_DIR"):
        return Path(os.environ["XDG_RUNTIME_DIR"]) / "sunoto" / "daemon.sock"
    user = os.environ.get("USER", "user")
    return Path(tempfile.gettempdir()) / f"sunoto-{user}-daemon.sock"


def load_dataset(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with path.open() as handle:
        for line in handle:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def request_polish(path: Path, text: str, timeout_s: float) -> tuple[dict[str, Any], int]:
    started = time.time()
    payload = json.dumps({"type": "polish", "text": text}, separators=(",", ":")) + "\n"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(timeout_s)
        sock.connect(str(path))
        sock.sendall(payload.encode("utf-8"))
        chunks: list[bytes] = []
        while True:
            chunk = sock.recv(65536)
            if not chunk:
                break
            chunks.append(chunk)
            if b"\n" in chunk:
                break
    response = b"".join(chunks).decode("utf-8").strip()
    return json.loads(response), round((time.time() - started) * 1000)


def raw_mode(raw_output: str | None) -> str:
    text = (raw_output or "").strip().strip('"`')
    first_line = text.splitlines()[0].strip() if text.splitlines() else ""
    if re.fullmatch(r"(?i)unchanged[\s.!:]*", first_line) and len(bench.tokenize(first_line)) <= 3:
        return "UNCHANGED"
    if re.match(r"(?is)^\s*edited\s*:", text):
        return "EDITED"
    return "FULL_OR_OTHER"


def actual_llm_mode(llm: dict[str, Any]) -> str:
    diagnostics = llm.get("diagnostics") or {}
    if diagnostics.get("decision_malformed"):
        return "MALFORMED_DECISION"
    decision = diagnostics.get("decision_label")
    if decision == "UNCHANGED":
        return "UNCHANGED"
    if decision == "EDIT":
        return "EDITED"
    return raw_mode(llm.get("raw_output"))


def percentile(values: list[int], pct: float) -> int | None:
    if not values:
        return None
    ordered = sorted(values)
    index = int((pct / 100.0) * (len(ordered) - 1))
    return ordered[index]


def summarize(results: list[dict[str, Any]]) -> dict[str, Any]:
    ok = [row for row in results if row.get("ok")]
    final_exact = sum(1 for row in ok if row["final_score"]["exact"])
    deterministic_exact = sum(1 for row in ok if row["deterministic_score"]["exact"])
    contract_ok = sum(1 for row in ok if row["contract_ok"])
    expected_words = [
        max(len(bench.tokenize(bench.norm(row["expected"]))), 1)
        for row in ok
    ]
    accepted = [row for row in ok if row.get("llm", {}).get("accepted")]
    total_lat = [row["daemon_total_latency_ms"] for row in ok]
    client_lat = [row["client_wall_latency_ms"] for row in ok]
    llm_lat = [row["llm"]["latency_ms"] for row in accepted if row["llm"].get("latency_ms") is not None]
    completion_tokens = [
        row["llm"]["diagnostics"].get("completion_tokens")
        for row in accepted
        if row["llm"].get("diagnostics", {}).get("completion_tokens") is not None
    ]
    decision_rows = [row for row in accepted if row.get("decision_label") or row.get("decision_malformed")]
    predicted_unchanged = [row for row in decision_rows if row.get("decision_label") == "UNCHANGED"]
    expected_edit = [row for row in decision_rows if row["expected_llm_mode"] == "EDITED"]
    decision_correct = [
        row
        for row in decision_rows
        if (
            row.get("decision_label") == "UNCHANGED"
            and row["expected_llm_mode"] == "UNCHANGED"
        )
        or (
            row.get("decision_label") == "EDIT"
            and row["expected_llm_mode"] == "EDITED"
        )
    ]
    decision_lat = [
        row["decision_diagnostics"].get("latency_ms")
        for row in decision_rows
        if isinstance(row.get("decision_diagnostics"), dict)
        and row["decision_diagnostics"].get("latency_ms") is not None
    ]
    decision_tokens = [
        row["decision_diagnostics"].get("completion_tokens")
        for row in decision_rows
        if isinstance(row.get("decision_diagnostics"), dict)
        and row["decision_diagnostics"].get("completion_tokens") is not None
    ]
    rewrite_rows = [row for row in accepted if row.get("rewrite_called")]
    rewrite_lat = [
        row["rewrite_diagnostics"].get("latency_ms")
        for row in rewrite_rows
        if isinstance(row.get("rewrite_diagnostics"), dict)
        and row["rewrite_diagnostics"].get("latency_ms") is not None
    ]
    by_expected_mode: dict[str, dict[str, int]] = {}
    for row in ok:
        mode = row["expected_llm_mode"]
        entry = by_expected_mode.setdefault(mode, {"cases": 0, "contract_ok": 0})
        entry["cases"] += 1
        entry["contract_ok"] += int(row["contract_ok"])
    return {
        "cases": len(results),
        "ok_cases": len(ok),
        "errors": len(results) - len(ok),
        "final_exact_pct": round(100 * final_exact / len(ok), 1) if ok else 0.0,
        "deterministic_exact_pct": round(100 * deterministic_exact / len(ok), 1) if ok else 0.0,
        "mean_wer": round(statistics.mean(row["final_score"]["wer"] for row in ok), 4)
        if ok
        else 0.0,
        "aggregate_edit_word_ratio_pct": round(
            100
            * sum(row["final_score"]["edits"] for row in ok)
            / sum(expected_words),
            2,
        )
        if ok
        else 0.0,
        "mean_similarity": round(statistics.mean(row["final_score"]["similarity"] for row in ok), 4)
        if ok
        else 0.0,
        "minimal_contract_pct": round(100 * contract_ok / len(ok), 1) if ok else 0.0,
        "by_expected_llm_mode": {
            mode: {
                **entry,
                "contract_pct": round(100 * entry["contract_ok"] / entry["cases"], 1)
                if entry["cases"]
                else 0.0,
            }
            for mode, entry in sorted(by_expected_mode.items())
        },
        "daemon_total_p50_ms": round(statistics.median(total_lat)) if total_lat else None,
        "daemon_total_p95_ms": percentile(total_lat, 95),
        "daemon_total_max_ms": max(total_lat) if total_lat else None,
        "client_wall_p50_ms": round(statistics.median(client_lat)) if client_lat else None,
        "llm_p50_ms": round(statistics.median(llm_lat)) if llm_lat else None,
        "llm_p95_ms": percentile(llm_lat, 95),
        "llm_max_ms": max(llm_lat) if llm_lat else None,
        "completion_tokens_p50": round(statistics.median(completion_tokens))
        if completion_tokens
        else None,
        "completion_tokens_p95": percentile(completion_tokens, 95),
        "decision_cases": len(decision_rows),
        "decision_accuracy_pct": round(100 * len(decision_correct) / len(decision_rows), 1)
        if decision_rows
        else None,
        "decision_unchanged_precision_pct": round(
            100
            * sum(1 for row in predicted_unchanged if row["expected_llm_mode"] == "UNCHANGED")
            / len(predicted_unchanged),
            1,
        )
        if predicted_unchanged
        else None,
        "decision_edit_recall_pct": round(
            100 * sum(1 for row in expected_edit if row.get("decision_label") == "EDIT") / len(expected_edit),
            1,
        )
        if expected_edit
        else None,
        "decision_malformed": sum(1 for row in decision_rows if row.get("decision_malformed")),
        "decision_latency_p50_ms": round(statistics.median(decision_lat)) if decision_lat else None,
        "decision_latency_p95_ms": percentile(decision_lat, 95),
        "decision_completion_tokens_p50": round(statistics.median(decision_tokens))
        if decision_tokens
        else None,
        "decision_completion_tokens_p95": percentile(decision_tokens, 95),
        "rewrite_calls": len(rewrite_rows),
        "rewrite_call_pct": round(100 * len(rewrite_rows) / len(accepted), 1)
        if accepted
        else None,
        "rewrite_latency_p50_ms": round(statistics.median(rewrite_lat)) if rewrite_lat else None,
        "rewrite_latency_p95_ms": percentile(rewrite_lat, 95),
        "validation_rejections": sum(1 for row in accepted if row.get("validation_rejected")),
    }


def run(args: argparse.Namespace) -> dict[str, Any]:
    rows = load_dataset(Path(args.dataset))
    if args.category:
        wanted = set(args.category)
        rows = [row for row in rows if row.get("category") in wanted]
    if args.limit is not None:
        rows = rows[: args.limit]
    expanded = []
    for repeat in range(args.repeat):
        for row in rows:
            expanded.append((repeat, row))

    path = Path(args.socket) if args.socket else control_socket_path()
    if not path.exists():
        raise SystemExit(f"daemon control socket not found: {path}")

    results: list[dict[str, Any]] = []
    for index, (repeat, row) in enumerate(expanded, 1):
        try:
            response, client_ms = request_polish(path, row["input"], args.timeout_s)
        except Exception as error:  # noqa: BLE001 - report and keep going in bench output.
            result = {
                "id": row["id"],
                "repeat": repeat,
                "ok": False,
                "error": str(error),
                "input": row["input"],
                "expected": row["expected"],
            }
            results.append(result)
            print(f"X  {index:03d} {row['id']:18s} ERROR {error}", flush=True)
            continue

        deterministic = response.get("deterministic_output") or ""
        output = response.get("output") or ""
        final_score = bench.score(output, row["expected"])
        deterministic_score = bench.score(deterministic, row["expected"])
        llm = response.get("llm") or {}
        diagnostics = llm.get("diagnostics") or {}
        decision_diagnostics = diagnostics.get("decision") or {}
        rewrite_diagnostics = diagnostics.get("rewrite") or {}
        actual_mode = actual_llm_mode(llm)
        expected_llm_mode = "UNCHANGED" if deterministic_score["exact"] else "EDITED"
        contract_ok = actual_mode == expected_llm_mode
        result = {
            "id": row["id"],
            "category": row.get("category"),
            "repeat": repeat,
            "ok": bool(response.get("ok")),
            "input": row["input"],
            "expected": row["expected"],
            "raw_expected_mode": row.get("expected_mode"),
            "expected_llm_mode": expected_llm_mode,
            "actual_llm_mode": actual_mode,
            "contract_ok": contract_ok,
            "decision_label": diagnostics.get("decision_label"),
            "decision_malformed": bool(diagnostics.get("decision_malformed")),
            "decision_diagnostics": decision_diagnostics,
            "rewrite_called": bool(diagnostics.get("rewrite_called")),
            "rewrite_diagnostics": rewrite_diagnostics,
            "validation_rejected": bool(diagnostics.get("validation_rejected")),
            "deterministic_output": deterministic,
            "output": output,
            "deterministic_score": deterministic_score,
            "final_score": final_score,
            "daemon_total_latency_ms": response.get("total_latency_ms"),
            "client_wall_latency_ms": client_ms,
            "llm": llm,
            "response": response if args.keep_response else None,
        }
        results.append(result)
        quality = "OK " if final_score["exact"] else ("~  " if final_score["similarity"] >= 0.9 else "X  ")
        contract = "COK" if contract_ok else f"CNO({expected_llm_mode}->{actual_mode})"
        llm_ms = llm.get("latency_ms") if isinstance(llm, dict) else None
        comp = (llm.get("diagnostics") or {}).get("completion_tokens") if isinstance(llm, dict) else None
        decision = result["decision_label"]
        if result["decision_malformed"]:
            decision = "MAL"
        rewrite = "rw" if result["rewrite_called"] else "no-rw"
        print(
            f"{quality}{index:03d} {row['id']:18s} sim={final_score['similarity']:.2f} "
            f"llm={llm_ms}ms tok={comp} decision={decision or '-'} {rewrite} {contract}",
            flush=True,
        )

    return {
        "kind": "daemon_architecture_llm_polish",
        "created_at": datetime.now().isoformat(timespec="seconds"),
        "socket": str(path),
        "dataset": str(Path(args.dataset)),
        "repeat": args.repeat,
        "summary": summarize(results),
        "results": results,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset", default=str(DEFAULT_DATASET))
    parser.add_argument("--socket")
    parser.add_argument("--category", action="append")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--timeout-s", type=float, default=45.0)
    parser.add_argument("--out-dir", default=str(DEFAULT_OUT_DIR))
    parser.add_argument("--output")
    parser.add_argument("--keep-response", action="store_true")
    args = parser.parse_args()

    report = run(args)
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    output = Path(args.output) if args.output else out_dir / "latest.json"
    output.write_text(json.dumps(report, indent=2) + "\n")
    timestamped = out_dir / f"{datetime.now().strftime('%Y%m%d-%H%M%S')}.json"
    if timestamped != output:
        timestamped.write_text(json.dumps(report, indent=2) + "\n")
    summary = report["summary"]
    print(
        "\nSUMMARY "
        f"final_exact={summary['final_exact_pct']}% "
        f"mean_wer={summary['mean_wer']} "
        f"contract={summary['minimal_contract_pct']}% "
        f"decision_acc={summary['decision_accuracy_pct']}% "
        f"rewrites={summary['rewrite_calls']} "
        f"llm_p50={summary['llm_p50_ms']}ms "
        f"llm_p95={summary['llm_p95_ms']}ms "
        f"daemon_p50={summary['daemon_total_p50_ms']}ms "
        f"-> {output}",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
