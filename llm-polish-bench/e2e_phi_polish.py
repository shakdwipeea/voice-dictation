#!/usr/bin/env python3
"""End-to-end Phi-4-mini LLM polish verification.

This is a driven (not unit) integration test. It exercises the *real* dictation
pipeline with the Phi-4-mini Q5 model that the post-ASR harness selected as the
experimental default:

  WAV -> parakeet_mlx_streaming final -> deterministic polish -> LLM polish

It has two parts, run strictly sequentially so they never share the GPU:

  1. No-insertion post-ASR bench (`sunoto-daemon bench --post-asr-llm`) for a
     clean, an edit, and a wait case. Parses the JSON report for latency
     percentiles and validation behavior.
  2. Live daemon control-socket smoke: start the real daemon with LLM polish
     enabled, wait for post-ASR warmup, call `sunoto-daemon polish TEXT`, read
     the latency from the response, then grep the daemon log for the
     `llm polish accepted in Nms` line.

It refuses to run if any sunoto-daemon / parakeet sidecar is already running
(two instances fight over the control socket and the GPU).

This is macOS-only: it synthesizes audio with `say` + `ffmpeg`. Run it from the
repo root on a machine that has the `.venv-llm-polish-mac` and
`.venv-nemotron-mac` venvs and the Phi GGUF under models/llm-polish-hf/.

Gates (exit non-zero on failure):
  - clean: release_to_llm_done p50 < 1500ms, output unchanged
  - edit:  output == "Please open the dashboard.", llm p50 < 1500ms
  - wait:  validation_rejected true (or output preserved)
  - live polish: accepts, output == "Please open the dashboard."
"""
from __future__ import annotations

import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
BENCH_OUT = REPO / "llm-polish-bench" / "out" / "post-asr-llm-latency"
PHI_GGUF = (
    REPO
    / "models/llm-polish-hf/phi-4-mini-q5/microsoft_Phi-4-mini-instruct-Q5_K_M.gguf"
)
LLM_VENV = REPO / ".venv-llm-polish-mac" / "bin" / "python"
ASR_VENV = REPO / ".venv-nemotron-mac" / "bin" / "python"
BINARY = REPO / "target" / "release" / "sunoto-daemon"

# Audio cases: (name, spoken text, expected final output / behavior).
CLEAN_TEXT = "The dashboard is ready and the response is fast."
EDIT_TEXT = "Please open settings. No wait. Open the dashboard."
WAIT_TEXT = "Wait, are you sure?"

CASES = [
    ("clean", CLEAN_TEXT, CLEAN_TEXT),
    ("edit", EDIT_TEXT, "Please open the dashboard."),
    ("wait", WAIT_TEXT, None),  # None => must be rejected or preserved
]


def die(message: str) -> "None":
    print(f"[e2e] FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def info(message: str) -> "None":
    print(f"[e2e] {message}")


def require(cmd: str) -> "None":
    if not shutil.which(cmd):
        die(f"required tool missing: {cmd}")


def check_prereqs() -> "None":
    if sys.platform != "darwin":
        die("this harness is macOS-only (uses `say` + `ffmpeg`)")
    for tool in ("say", "ffmpeg"):
        require(tool)
    if not PHI_GGUF.is_file():
        die(f"Phi GGUF missing: {PHI_GGUF}")
    if not LLM_VENV.is_file():
        die(f"LLM polish venv missing: {LLM_VENV}")
    if not ASR_VENV.is_file():
        die(f"ASR venv missing: {ASR_VENV}")
    if not BINARY.is_file():
        die(
            "release binary missing; run `cargo build --release` "
            "(with ~/.cargo/bin on PATH)"
        )


def no_daemon_running() -> "None":
    result = subprocess.run(
        ["pgrep", "-fl", "sunoto-daemon run|llm_polish_sidecar|parakeet_mlx_streaming"],
        capture_output=True,
        text=True,
    )
    if result.stdout.strip():
        die(
            "a daemon/sidecar is already running; stop it first:\n"
            f"{result.stdout.strip()}\n"
            "(two instances fight over the control socket and the GPU)"
        )


def synthesize_wav(text: str, dest: Path) -> "None":
    """Render `text` to a 16 kHz mono 16-bit PCM WAV via say + ffmpeg."""
    aiff = dest.with_suffix(".aiff")
    dest.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["say", "-o", str(aiff), text], check=True)
    subprocess.run(
        [
            "ffmpeg",
            "-y",
            "-loglevel",
            "error",
            "-i",
            str(aiff),
            "-ar",
            "16000",
            "-ac",
            "1",
            "-sample_fmt",
            "s16",
            str(dest),
        ],
        check=True,
    )


def write_config(path: Path, socket: Path) -> "None":
    config = {
        "shortcut": "Ctrl+F1",
        "microphone": "auto",
        "backend": "parakeet_mlx_streaming",
        "profile_ms": 560,
        "preroll_ms": 300,
        "final_timeout_ms": 8000,
        "final_timeout_rtf": 3.0,
        "sidecar_python": None,
        "sidecar_script": None,
        "asr_device": None,
        "asr_model": None,
        "allow_enter_and_tab": False,
        "overlay_enabled": False,
        "overlay_backend": "macos",
        "polish_enabled": True,
        "llm_polish_enabled": True,
        "llm_polish_python": str(LLM_VENV),
        "llm_polish_script": None,
        "llm_polish_model_path": None,
        "llm_polish_model": "phi4_mini",
        "llm_polish_timeout_ms": 30000,
        "polish": {
            "resolve_corrections": True,
            "remove_fillers": True,
            "fillers": ["um", "uh", "uhm", "umm", "er", "erm", "hmm", "mhm"],
            "dictionary": [],
            "snippets": [],
            "style": "prose",
            "app_styles": [],
        },
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(config, indent=2) + "\n")
    # The control socket is selected from the environment, not the config.
    os.environ["SUNOTO_CONTROL_SOCKET"] = str(socket)


def run_bench_case(
    config: Path, audio: Path, sessions: int, output: Path
) -> "dict":
    cmd = [
        str(BINARY),
        "bench",
        "--post-asr-llm",
        "--config",
        str(config),
        "--sessions",
        str(sessions),
        "--audio",
        str(audio),
        "--output",
        str(output),
    ]
    info(f"bench: {audio.name} x{sessions}")
    env = dict(os.environ)
    env.setdefault("SUNOTO_LLM_POLISH_MODE", "constrained_one_call")
    env.setdefault("SUNOTO_LLM_POLISH_GRAMMAR", "0")
    env.setdefault("SUNOTO_LLM_POLISH_TIMING_THRESHOLD_MS", "-1")
    proc = subprocess.run(
        cmd, capture_output=True, text=True, env=env, timeout=600
    )
    if proc.returncode != 0:
        die(
            f"bench failed for {audio.name}: rc={proc.returncode}\n"
            f"stderr:\n{proc.stderr[-4000:]}"
        )
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        die(f"bench stdout was not JSON for {audio.name}: {exc}\n{proc.stdout[-2000:]}")


def percentile(report: "dict", name: str, key: str) -> "int | None":
    block = report.get("percentiles", {}).get(name)
    if not block:
        return None
    value = block.get(key)
    return int(value) if value is not None else None


def evaluate_bench_case(
    name: str, report: "dict", expected: "str | None"
) -> "dict":
    runs = report.get("runs", [])
    failures = report.get("failures", [])
    llm_p50 = percentile(report, "llm_latency_ms", "p50")
    llm_p95 = percentile(report, "llm_latency_ms", "p95")
    release_p50 = percentile(report, "release_to_llm_done_ms", "p50")
    outputs = [run.get("llm_output") for run in runs]
    rejections = sum(
        1
        for run in runs
        if (run.get("llm_diagnostics") or {}).get("validation_rejected")
    )
    details = {
        "case": name,
        "sessions": len(runs),
        "failures": failures,
        "llm_p50_ms": llm_p50,
        "llm_p95_ms": llm_p95,
        "release_to_llm_done_p50_ms": release_p50,
        "outputs": outputs,
        "validation_rejections": rejections,
        "raw_transcript": runs[0].get("raw_transcript") if runs else None,
    }
    checks: list[str] = []
    ok = True
    if failures:
        ok = False
        checks.append("had failures")
    if name == "clean":
        if release_p50 is not None and release_p50 >= 1500:
            ok = False
            checks.append(f"clean release_to_llm_done p50 {release_p50}ms >= 1500")
        if outputs and any(o != expected for o in outputs):
            ok = False
            checks.append(f"clean output changed: {outputs}")
    elif name == "edit":
        if outputs and any(o != expected for o in outputs):
            ok = False
            checks.append(f"edit output mismatch: {outputs} (want {expected!r})")
        if llm_p50 is not None and llm_p50 >= 1500:
            ok = False
            checks.append(f"edit llm p50 {llm_p50}ms >= 1500")
    elif name == "wait":
        # The validator must reject dropping the leading "Wait," marker, or the
        # output must be preserved. Phi itself drops it; the patch restores it
        # by rejecting, so rejections>=1 is the expected success path.
        rejected = rejections >= 1
        preserved = bool(outputs) and all(o == WAIT_TEXT for o in outputs)
        if not rejected and not preserved:
            ok = False
            checks.append(
                f"wait case should be validation_rejected or preserved; "
                f"rejections={rejections} outputs={outputs}"
            )
    details["ok"] = ok
    details["checks"] = checks
    return details


def wait_for(log_path: Path, pattern: str, timeout_s: int) -> "str":
    deadline = time.time() + timeout_s
    compiled = re.compile(pattern)
    while time.time() < deadline:
        if log_path.is_file() and log_path.stat().st_size > 0:
            text = log_path.read_text(errors="replace")
            match = compiled.search(text)
            if match:
                return text
        time.sleep(0.5)
    die(f"timed out waiting {timeout_s}s for /{pattern}/ in {log_path}")


def live_daemon_smoke(config: Path, socket: Path) -> "dict":
    log_path = Path(tempfile.gettempdir()) / "sunoto-phi-e2e-daemon.log"
    if log_path.exists():
        log_path.unlink()
    env = dict(os.environ)
    env["SUNOTO_CONTROL_SOCKET"] = str(socket)
    env.setdefault("SUNOTO_LLM_POLISH_MODE", "constrained_one_call")
    env.setdefault("SUNOTO_LLM_POLISH_GRAMMAR", "0")
    log = open(log_path, "w")
    info(f"starting daemon; log -> {log_path}")
    proc = subprocess.Popen(
        [str(BINARY), "run", "--config", str(config)],
        stdout=log,
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )
    try:
        wait_for(
            log_path,
            r"LLM polish post-ASR warmup complete",
            timeout_s=180,
        )
        info("daemon warm; sending control-socket polish")
        result = subprocess.run(
            [
                str(BINARY),
                "polish",
                EDIT_TEXT,
            ],
            capture_output=True,
            text=True,
            env=env,
            timeout=120,
        )
        if result.returncode != 0:
            die(f"polish CLI failed: rc={result.returncode}\n{result.stderr}")
        try:
            response = json.loads(result.stdout)
        except json.JSONDecodeError as exc:
            die(f"polish response not JSON: {exc}\n{result.stdout}")
        log_text = log_path.read_text(errors="replace")
        accepted = re.findall(r"llm polish accepted in (\d+)ms", log_text)
        sidecar = re.findall(
            r"\[llm-polish-sidecar\] polish session=\d+: latency=(\d+)ms", log_text
        )
        log_latencies = [int(value) for value in accepted + sidecar]
        detail = {
            "accepted": bool(response.get("llm", {}).get("accepted")),
            "output": response.get("output"),
            "total_latency_ms": response.get("total_latency_ms"),
            "llm_latency_ms": response.get("llm", {}).get("latency_ms"),
            "log_latencies_ms": log_latencies,
        }
        checks: list[str] = []
        ok = True
        if not detail["accepted"]:
            ok = False
            checks.append(f"live polish not accepted: {response.get('llm')}")
        if detail["output"] != "Please open the dashboard.":
            ok = False
            checks.append(f"live polish output mismatch: {detail['output']!r}")
        if detail["llm_latency_ms"] is None:
            ok = False
            checks.append("no latency in polish response")
        detail["ok"] = ok
        detail["checks"] = checks
        return detail
    finally:
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()
        info(f"daemon log tail:\n{log_path.read_text(errors='replace')[-1200:]}")


def main(argv: "list[str]") -> "int":
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sessions", type=int, default=5)
    parser.add_argument(
        "--skip-live", action="store_true", help="skip the live daemon control-socket smoke"
    )
    parser.add_argument(
        "--out", type=Path, default=BENCH_OUT / "phi-e2e-summary.json"
    )
    args = parser.parse_args(argv)

    check_prereqs()
    no_daemon_running()

    work = Path(tempfile.mkdtemp(prefix="sunoto-phi-e2e-"))
    config = work / "config.json"
    socket = work / "daemon.sock"
    write_config(config, socket)

    # Synthesize the case WAVs.
    audio_paths: list[tuple[str, Path]] = []
    for name, text, _ in CASES:
        wav = work / f"{name}.wav"
        synthesize_wav(text, wav)
        audio_paths.append((name, wav))

    summary: dict = {
        "model": "phi4_mini",
        "gguf": str(PHI_GGUF),
        "sessions_per_case": args.sessions,
        "bench_cases": [],
        "live_daemon": None,
    }

    # Part 1: no-insertion post-ASR bench per case. These are sequential and
    # self-contained (the bench spawns and tears down its own sidecars), so no
    # daemon must be running concurrently.
    for name, audio in audio_paths:
        out_json = BENCH_OUT / f"phi-e2e-{name}-{int(time.time())}.json"
        report = run_bench_case(config, audio, args.sessions, out_json)
        case_def = next(case for case in CASES if case[0] == name)
        detail = evaluate_bench_case(name, report, case_def[2])
        detail["report"] = str(out_json)
        summary["bench_cases"].append(detail)
        info(
            f"  {name}: llm_p50={detail['llm_p50_ms']}ms "
            f"p95={detail['llm_p95_ms']}ms rel_done_p50={detail['release_to_llm_done_p50_ms']}ms "
            f"ok={detail['ok']} {detail['checks']}"
        )

    # Part 2: live daemon control-socket smoke. Run only after the bench
    # sidecars have exited so the GPU is free.
    if not args.skip_live:
        summary["live_daemon"] = live_daemon_smoke(config, socket)

    summary["ok"] = all(case["ok"] for case in summary["bench_cases"]) and (
        summary["live_daemon"] is None or summary["live_daemon"]["ok"]
    )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(summary, indent=2) + "\n")
    info(f"summary written to {args.out}")

    print("\n=== Phi-4-mini e2e summary ===")
    print(f"model: phi4_mini ({PHI_GGUF.name})")
    for case in summary["bench_cases"]:
        print(
            f"  bench/{case['case']:<5}: llm_p50={case['llm_p50_ms']}ms "
            f"p95={case['llm_p95_ms']}ms ok={case['ok']} {case['checks']}"
        )
    if summary["live_daemon"]:
        live = summary["live_daemon"]
        print(
            f"  live polish: accepted={live['accepted']} output={live['output']!r} "
            f"latency={live['llm_latency_ms']}ms ok={live['ok']} {live['checks']}"
        )
    print(f"overall: {'PASS' if summary['ok'] else 'FAIL'}")
    return 0 if summary["ok"] else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
